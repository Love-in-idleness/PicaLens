use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use adw::prelude::*;
use futures_channel::oneshot;
use futures_util::FutureExt;
use gtk::{gdk, gio, glib};

use crate::{
    capture::Capture,
    game_page::GamePage,
    gnome,
    gpu::GpuUpscaler,
    kwin,
    locale::Locale,
    native,
    preview::{self, StopSender},
    render_client::{self, RenderUpscaler},
    session::{self, Result, Stop, Target},
    settings::{
        Crop, DEFAULT_ID, Filter, HIDE_CURSOR_CHOICES, Language, Layout, Presentation, Preset,
        Settings, Theme, normalize_app_id,
    },
    shortcut::{self, DesktopShortcut},
    upscale::{Pixels, Upscaler},
    viewport::Viewport,
};

const APP_ID: &str = "io.github.LoveInIdleness.PicaLens";
const HIDE_CURSOR_LABELS: &[&str] = &[
    "hide_cursor_off",
    "hide_cursor_1s",
    "hide_cursor_2s",
    "hide_cursor_3s",
    "hide_cursor_5s",
];

pub fn run() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    match args.get(1).and_then(|arg| arg.to_str()) {
        None if args.len() == 1 => {}
        Some("toggle" | "screenshot") if args.len() == 2 => {}
        flag => {
            let locale = Locale::new(Settings::load().language());
            let help = locale.text("cli_help");
            return if args.len() == 2 && matches!(flag, Some("-h" | "--help")) {
                println!("{help}");
                std::process::ExitCode::SUCCESS
            } else {
                diagnostic!("{help}");
                std::process::ExitCode::from(2)
            };
        }
    }

    gdk::set_allowed_backends("wayland");
    if let Err(error) = gtk::init() {
        let locale = Locale::new(Settings::load().language());
        diagnostic!("{} ({error})", locale.text("need_wayland"));
        return std::process::ExitCode::FAILURE;
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let view = RefCell::new(None::<Rc<View>>);
    app.connect_command_line(move |app, command| {
        let view = view
            .borrow_mut()
            .get_or_insert_with(|| View::new(app))
            .clone();
        let arguments = command.arguments();
        if arguments.len() == 1 {
            if !view.stop("PicaLens opened") {
                view.window.present();
            }
        } else if arguments.get(1).and_then(|arg| arg.to_str()) == Some("screenshot") {
            view.trigger_screenshot();
        } else if !view.stop("toggle") {
            let target = if session::portal_desktop() {
                Target::Choose
            } else {
                Target::Active
            };
            view.start(app, target);
        }
        glib::ExitCode::SUCCESS
    });
    app.run_with_args(
        &args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>(),
    )
    .into()
}

struct View {
    window: adw::ApplicationWindow,
    status: adw::ActionRow,
    backend: adw::ActionRow,
    launch: gtk::Button,
    toasts: adw::ToastOverlay,
    settings: Rc<RefCell<Settings>>,
    locale: Locale,
    chooser: adw::ComboRow,
    presets: gtk::ListBox,
    app_rules: gtk::ListBox,
    filling_presets: Cell<bool>,
    active: RefCell<Option<StopSender>>,
    capture_window: RefCell<Option<adw::ApplicationWindow>>,
    preview_screenshot: RefCell<Option<Rc<dyn Fn()>>>,
    kwin_active: Cell<bool>,
    last_app_id: RefCell<Option<String>>,
    closing: Cell<bool>,
    _games: Rc<GamePage>,
}

impl View {
    fn new(app: &adw::Application) -> Rc<Self> {
        let settings = Rc::new(RefCell::new(Settings::load()));
        let locale = Locale::new(settings.borrow().language());
        let style = app.style_manager();
        let current_theme = settings.borrow().theme();
        apply_theme(&style, current_theme);
        let t = |key| locale.text(key);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(t("app_name"))
            .default_width(850)
            .default_height(640)
            .build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new(&t("magnify"), &t("app_name"));
        header.set_title_widget(Some(&title));
        let nav = gtk::Button::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(t("navigation"))
            .build();
        header.pack_start(&nav);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));

        let sidebar = gtk::StackSidebar::builder().stack(&stack).build();
        let split = adw::OverlaySplitView::builder()
            .min_sidebar_width(155.0)
            .max_sidebar_width(205.0)
            .sidebar(&sidebar)
            .content(&toolbar)
            .show_sidebar(true)
            .build();
        nav.connect_clicked({
            let split = split.downgrade();
            move |_| {
                if let Some(split) = split.upgrade() {
                    split.set_show_sidebar(!split.shows_sidebar());
                }
            }
        });
        window.add_tick_callback({
            let split = split.downgrade();
            move |window, _| {
                if let Some(split) = split.upgrade() {
                    let collapsed = window.width() < 690;
                    if split.is_collapsed() != collapsed {
                        split.set_collapsed(collapsed);
                        split.set_show_sidebar(!collapsed);
                    }
                }
                glib::ControlFlow::Continue
            }
        });
        stack.connect_visible_child_name_notify({
            let split = split.downgrade();
            move |stack| {
                if let Some(page) = stack.visible_child().map(|child| stack.page(&child)) {
                    title.set_title(page.title().as_deref().unwrap_or_default());
                }
                if let Some(split) = split.upgrade()
                    && split.is_collapsed()
                {
                    split.set_show_sidebar(false);
                }
            }
        });
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        let home = adw::PreferencesPage::new();
        let main_group = adw::PreferencesGroup::builder()
            .title(t("open_window"))
            .build();
        let status = adw::ActionRow::builder().title(t("ready")).build();
        let launch = gtk::Button::with_label(&t("choose_window"));
        launch.add_css_class("suggested-action");
        launch.set_valign(gtk::Align::Center);
        status.add_suffix(&launch);
        main_group.add(&status);
        let chooser = adw::ComboRow::builder().title(t("current_preset")).build();
        main_group.add(&chooser);
        let backend = adw::ActionRow::builder()
            .title(t("backend"))
            .subtitle(t("backend_auto"))
            .build();
        main_group.add(&backend);
        home.add(&main_group);

        let preset_page = adw::PreferencesPage::new();
        let preset_group = adw::PreferencesGroup::builder().title(t("presets")).build();
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(t("add_preset"))
            .build();
        preset_group.set_header_suffix(Some(&add));
        let presets = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        presets.add_css_class("boxed-list");
        preset_group.add(&presets);
        preset_page.add(&preset_group);

        let app_rules_group = adw::PreferencesGroup::builder()
            .title(t("app_rules"))
            .description(t("app_rules_hint"))
            .build();
        let add_rule = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(t("add_app_rule"))
            .build();
        app_rules_group.set_header_suffix(Some(&add_rule));
        let app_rules = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        app_rules.add_css_class("boxed-list");
        app_rules_group.add(&app_rules);
        preset_page.add(&app_rules_group);

        let games = GamePage::new(&window, settings.clone(), locale);
        let options = adw::PreferencesPage::new();
        let ui = adw::PreferencesGroup::builder()
            .title(t("appearance"))
            .build();
        let theme = combo(
            locale,
            "theme",
            Theme::ALL.iter().map(|item| item.id()),
            index(Theme::ALL, current_theme),
        );
        ui.add(&theme);
        let language = combo(
            locale,
            "language",
            Language::ALL.iter().map(|item| item.id()),
            Language::ALL
                .iter()
                .position(|item| *item == settings.borrow().language())
                .unwrap_or(0),
        );
        language.set_subtitle(&t("language_restart"));
        ui.add(&language);
        options.add(&ui);

        let behavior = adw::PreferencesGroup::builder()
            .title(t("magnification_behavior"))
            .build();
        let current_hide_cursor = settings.borrow().hide_cursor_seconds();
        let hide_cursor = combo(
            locale,
            "hide_cursor",
            HIDE_CURSOR_LABELS.iter().copied(),
            index(HIDE_CURSOR_CHOICES, current_hide_cursor),
        );
        behavior.add(&hide_cursor);
        let inhibit_idle = adw::SwitchRow::builder()
            .title(t("inhibit_idle"))
            .subtitle(t("inhibit_idle_hint"))
            .active(settings.borrow().inhibit_idle())
            .build();
        behavior.add(&inhibit_idle);
        let current_shot_dir = settings.borrow().screenshot_dir();
        let screenshot_dir = adw::ActionRow::builder()
            .title(t("screenshot_dir"))
            .subtitle(if current_shot_dir.is_empty() {
                t("screenshot_ask")
            } else {
                current_shot_dir.clone()
            })
            .build();
        let choose_shot_dir = gtk::Button::with_label(&t("choose_folder"));
        choose_shot_dir.set_valign(gtk::Align::Center);
        let clear_shot_dir = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text(t("clear"))
            .valign(gtk::Align::Center)
            .visible(!current_shot_dir.is_empty())
            .build();
        screenshot_dir.add_suffix(&choose_shot_dir);
        screenshot_dir.add_suffix(&clear_shot_dir);
        behavior.add(&screenshot_dir);
        options.add(&behavior);
        let shortcuts = adw::PreferencesGroup::builder()
            .title(t("shortcuts"))
            .build();
        let toggle = adw::ActionRow::builder()
            .title(t("toggle_magnification"))
            .build();
        if let Some(desktop) = DesktopShortcut::detect() {
            let key = gtk::Label::new(None);
            key.add_css_class("dim-label");
            let edit = gtk::Button::with_label(&t("edit"));
            edit.set_valign(gtk::Align::Center);
            toggle.add_suffix(&key);
            toggle.add_suffix(&edit);
            toggle.set_activatable_widget(Some(&edit));
            edit.connect_clicked({
                let window = window.downgrade();
                let key = key.clone();
                let desktop = desktop.clone();
                move |_| {
                    if let Some(window) = window.upgrade() {
                        edit_shortcut(&window, &key, desktop.clone(), locale);
                    }
                }
            });
            glib::spawn_future_local(async move {
                update_shortcut_label(&key, &desktop, locale).await;
            });
        } else {
            toggle.set_subtitle(&t("shortcut_command_hint"));
            let command = shortcut::toggle_command().unwrap_or_else(|_| "picalens toggle".into());
            let label = gtk::Label::new(Some(&command));
            label.set_wrap(true);
            label.set_selectable(true);
            toggle.add_suffix(&label);
        }
        shortcuts.add(&toggle);
        options.add(&shortcuts);

        stack.add_titled(&home, Some("magnify"), &t("magnify"));
        stack.add_titled(&preset_page, Some("presets"), &t("presets"));
        stack.add_titled(&games.widget, Some("games"), &t("games"));
        stack.add_titled(&options, Some("settings"), &t("settings"));

        let view = Rc::new(Self {
            window,
            status,
            backend,
            launch,
            toasts,
            settings,
            locale,
            chooser,
            presets,
            app_rules,
            filling_presets: Cell::new(false),
            active: RefCell::new(None),
            capture_window: RefCell::new(None),
            preview_screenshot: RefCell::new(None),
            kwin_active: Cell::new(false),
            last_app_id: RefCell::new(None),
            closing: Cell::new(false),
            _games: games,
        });
        view.reload_presets();
        for (button, handler) in [
            (add, Self::new_preset as fn(&Rc<Self>)),
            (add_rule, Self::new_app_rule as fn(&Rc<Self>)),
            (view.launch.clone(), Self::launch),
        ] {
            button.connect_clicked({
                let weak = Rc::downgrade(&view);
                move |_| {
                    if let Some(view) = weak.upgrade() {
                        handler(&view);
                    }
                }
            });
        }
        view.chooser.connect_selected_notify({
            let weak = Rc::downgrade(&view);
            move |row| {
                let Some(view) = weak.upgrade() else { return };
                if view.filling_presets.get() {
                    return;
                }
                let id = view
                    .settings
                    .borrow()
                    .preset_ids()
                    .get(row.selected() as usize)
                    .cloned();
                if let Some(id) = id {
                    let result = view.settings.borrow_mut().select(&id);
                    if let Err(error) = result {
                        view.save_failed(error);
                        view.reload_presets();
                    }
                }
                view.show_mode();
            }
        });
        language.connect_selected_notify({
            let weak = Rc::downgrade(&view);
            move |row| {
                let Some(view) = weak.upgrade() else { return };
                if let Some(&language) = Language::ALL.get(row.selected() as usize) {
                    let result = view.settings.borrow_mut().set_language(language);
                    match result {
                        Ok(()) => view.toast("language_restart"),
                        Err(error) => view.save_failed(error),
                    }
                }
            }
        });
        theme.connect_selected_notify({
            let weak = Rc::downgrade(&view);
            move |row| {
                let (Some(view), Some(&theme)) =
                    (weak.upgrade(), Theme::ALL.get(row.selected() as usize))
                else {
                    return;
                };
                let saved = view.settings.borrow().theme();
                if theme == saved {
                    return;
                }
                let result = view.settings.borrow_mut().set_theme(theme);
                match result {
                    Ok(()) => apply_theme(&style, theme),
                    Err(error) => {
                        row.set_selected(index(Theme::ALL, saved) as u32);
                        view.save_failed(error);
                    }
                }
            }
        });
        hide_cursor.connect_selected_notify({
            let weak = Rc::downgrade(&view);
            move |row| {
                let (Some(view), Some(&seconds)) = (
                    weak.upgrade(),
                    HIDE_CURSOR_CHOICES.get(row.selected() as usize),
                ) else {
                    return;
                };
                let saved = view.settings.borrow().hide_cursor_seconds();
                if seconds == saved {
                    return;
                }
                let result = view.settings.borrow_mut().set_hide_cursor_seconds(seconds);
                if let Err(error) = result {
                    row.set_selected(index(HIDE_CURSOR_CHOICES, saved) as u32);
                    view.save_failed(error);
                }
            }
        });
        inhibit_idle.connect_active_notify({
            let weak = Rc::downgrade(&view);
            move |row| {
                let Some(view) = weak.upgrade() else { return };
                let active = row.is_active();
                if active == view.settings.borrow().inhibit_idle() {
                    return;
                }
                let result = view.settings.borrow_mut().set_inhibit_idle(active);
                if let Err(error) = result {
                    row.set_active(!active);
                    view.save_failed(error);
                }
            }
        });
        choose_shot_dir.connect_clicked({
            let weak = Rc::downgrade(&view);
            let row = screenshot_dir.downgrade();
            let clear = clear_shot_dir.downgrade();
            move |_| {
                let Some(view) = weak.upgrade() else { return };
                let picker = gtk::FileDialog::builder()
                    .title(view.t("screenshot_dir"))
                    .modal(true)
                    .build();
                let current = view.settings.borrow().screenshot_dir();
                if !current.is_empty() {
                    picker.set_initial_folder(Some(&gio::File::for_path(current)));
                }
                let window = view.window.clone();
                let row = row.clone();
                let clear = clear.clone();
                glib::spawn_future_local(async move {
                    let Ok(folder) = picker.select_folder_future(Some(&window)).await else {
                        return;
                    };
                    let Some(path) = folder.path() else { return };
                    let Some(text) = path.to_str() else { return };
                    let result = view.settings.borrow_mut().set_screenshot_dir(text);
                    match result {
                        Ok(()) => {
                            if let Some(row) = row.upgrade() {
                                row.set_subtitle(text);
                            }
                            if let Some(clear) = clear.upgrade() {
                                clear.set_visible(true);
                            }
                        }
                        Err(error) => view.save_failed(error),
                    }
                });
            }
        });
        clear_shot_dir.connect_clicked({
            let weak = Rc::downgrade(&view);
            let row = screenshot_dir.downgrade();
            move |button| {
                let Some(view) = weak.upgrade() else { return };
                let result = view.settings.borrow_mut().set_screenshot_dir("");
                match result {
                    Ok(()) => {
                        if let Some(row) = row.upgrade() {
                            row.set_subtitle(&view.t("screenshot_ask"));
                        }
                        button.set_visible(false);
                    }
                    Err(error) => view.save_failed(error),
                }
            }
        });
        view.window.connect_close_request({
            let weak = Rc::downgrade(&view);
            move |window| {
                if let Some(view) = weak.upgrade() {
                    view.closing.set(true);
                    if !view.stop("window closed")
                        && let Some(app) = window.application()
                    {
                        app.quit();
                    }
                }
                window.set_visible(false);
                glib::Propagation::Stop
            }
        });
        view.window.add_controller(view.escape());
        view
    }

    fn t(&self, key: &str) -> String {
        self.locale.text(key)
    }

    fn toast(&self, key: &str) {
        self.toasts.add_toast(adw::Toast::new(&self.t(key)));
    }

    fn save_failed(&self, error: Box<dyn std::error::Error>) {
        diagnostic!("picalens could not save settings: {error}");
        self.toast("save_failed");
    }

    fn preset_name(&self, preset: &Preset) -> String {
        if preset.name.is_empty() {
            self.t("default_preset")
        } else {
            preset.name.clone()
        }
    }

    fn reload_presets(self: &Rc<Self>) {
        while let Some(row) = self.presets.first_child() {
            self.presets.remove(&row);
        }
        let settings = self.settings.borrow();
        let ids = settings.preset_ids();
        let mut names = Vec::with_capacity(ids.len());
        for id in &ids {
            let preset = settings.preset(id);
            let name = self.preset_name(&preset);
            let mut subtitle = [
                preset.layout.id(),
                preset.filter.id(),
                preset.presentation.id(),
            ]
            .map(|key| self.t(key))
            .join(" · ");
            if preset.post_sharpen && preset.filter != Filter::Cas {
                subtitle.push_str(&format!(" + {}", self.t("post_sharpen")));
            }
            if preset.post_deband {
                subtitle.push_str(&format!(" + {}", self.t("post_deband")));
            }
            if !preset.crop.is_empty() {
                let [l, t, r, b] = preset.crop.values();
                subtitle.push_str(&format!(" · {} {l}/{t}/{r}/{b}", self.t("cropped")));
            }
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name))
                .subtitle(subtitle)
                .build();
            let edit = self.preset_button("document-edit-symbolic", "edit");
            let copy = self.preset_button("edit-copy-symbolic", "duplicate");
            row.add_suffix(&edit);
            row.add_suffix(&copy);
            for (button, duplicate) in [(edit, false), (copy, true)] {
                button.connect_clicked({
                    let weak = Rc::downgrade(self);
                    let preset = preset.clone();
                    let id = id.clone();
                    move |_| {
                        if let Some(view) = weak.upgrade() {
                            let mut preset = preset.clone();
                            if duplicate {
                                preset.name =
                                    format!("{} ({})", view.preset_name(&preset), view.t("copy"));
                            }
                            view.edit_preset((!duplicate).then(|| id.clone()), preset);
                        }
                    }
                });
            }
            if id != DEFAULT_ID {
                let remove = self.preset_button("user-trash-symbolic", "delete");
                row.add_suffix(&remove);
                remove.connect_clicked({
                    let weak = Rc::downgrade(self);
                    let id = id.clone();
                    move |_| {
                        if let Some(view) = weak.upgrade() {
                            let result = view.settings.borrow_mut().delete("preset", &id);
                            match result {
                                Ok(()) => view.reload_presets(),
                                Err(error) => view.save_failed(error),
                            }
                        }
                    }
                });
            }
            self.presets.append(&row);
            names.push(name);
        }
        self.filling_presets.set(true);
        let selected = ids
            .iter()
            .position(|id| *id == settings.selected())
            .unwrap_or(0);
        self.chooser.set_model(Some(&gtk::StringList::new(
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        )));
        self.chooser.set_selected(selected as u32);
        self.filling_presets.set(false);
        drop(settings);
        self.reload_app_rules();
        self.show_mode();
    }

    fn reload_app_rules(self: &Rc<Self>) {
        while let Some(row) = self.app_rules.first_child() {
            self.app_rules.remove(&row);
        }
        let settings = self.settings.borrow();
        let rules = settings.app_rules();
        if rules.is_empty() {
            let empty = adw::ActionRow::builder()
                .title(self.t("no_app_rules"))
                .build();
            empty.add_css_class("dim-label");
            self.app_rules.append(&empty);
            return;
        }
        let ids = settings.preset_ids();
        for (app, preset_id) in rules {
            let preset = if ids.contains(&preset_id) {
                settings.preset(&preset_id)
            } else {
                settings.preset(DEFAULT_ID)
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&app))
                .subtitle(glib::markup_escape_text(&self.preset_name(&preset)))
                .build();
            let edit = self.preset_button("document-edit-symbolic", "edit");
            let remove = self.preset_button("user-trash-symbolic", "delete");
            row.add_suffix(&edit);
            row.add_suffix(&remove);
            edit.connect_clicked({
                let weak = Rc::downgrade(self);
                let app = app.clone();
                let preset_id = preset_id.clone();
                move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.edit_app_rule(Some(app.clone()), preset_id.clone());
                    }
                }
            });
            remove.connect_clicked({
                let weak = Rc::downgrade(self);
                move |_| {
                    if let Some(view) = weak.upgrade() {
                        let result = view.settings.borrow_mut().remove_app_rule(&app);
                        match result {
                            Ok(()) => view.reload_app_rules(),
                            Err(error) => view.save_failed(error),
                        }
                    }
                }
            });
            self.app_rules.append(&row);
        }
    }

    fn new_app_rule(self: &Rc<Self>) {
        let initial_preset = self.settings.borrow().selected();
        self.edit_app_rule(None, initial_preset);
    }

    #[allow(deprecated)]
    fn edit_app_rule(self: &Rc<Self>, original: Option<String>, preset_id: String) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&self.t("edit_app_rule")),
            None,
        );
        let group = adw::PreferencesGroup::new();
        let initial_app = original
            .clone()
            .or_else(|| self.last_app_id.borrow().clone())
            .unwrap_or_default();
        let app_entry = adw::EntryRow::builder()
            .title(self.t("app_id"))
            .text(initial_app)
            .build();
        let settings = self.settings.borrow();
        let ids = settings.preset_ids();
        let names = ids
            .iter()
            .map(|id| self.preset_name(&settings.preset(id)))
            .collect::<Vec<_>>();
        drop(settings);
        let selected = ids.iter().position(|id| *id == preset_id).unwrap_or(0);
        let preset_row = adw::ComboRow::builder()
            .title(self.t("current_preset"))
            .model(&gtk::StringList::new(
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
            ))
            .selected(selected as u32)
            .build();
        let hint = gtk::Label::builder()
            .label(self.t("app_id_hint"))
            .wrap(true)
            .xalign(0.0)
            .build();
        hint.add_css_class("dim-label");
        group.add(&app_entry);
        group.add(&preset_row);
        group.add(&hint);
        dialog.set_extra_child(Some(&group));
        dialog.add_response("cancel", &self.t("cancel"));
        dialog.add_response("save", &self.t("save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("save", normalize_app_id(&app_entry.text()).is_some());
        app_entry.connect_changed({
            let dialog = dialog.downgrade();
            move |entry| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.set_response_enabled("save", normalize_app_id(&entry.text()).is_some());
                }
            }
        });
        dialog.connect_response(Some("save"), {
            let weak = Rc::downgrade(self);
            move |_, _| {
                let Some(view) = weak.upgrade() else { return };
                let Some(normalized) = normalize_app_id(&app_entry.text()) else {
                    view.toast("invalid_app_id");
                    return;
                };
                let chosen = ids
                    .get(preset_row.selected() as usize)
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_ID.into());
                let result = (|| -> Result<()> {
                    let mut settings = view.settings.borrow_mut();
                    if let Some(old) = original.as_deref()
                        && old != normalized
                    {
                        settings.remove_app_rule(old)?;
                    }
                    settings.set_app_rule(&normalized, &chosen)
                })();
                match result {
                    Ok(()) => view.reload_app_rules(),
                    Err(error) => view.save_failed(error),
                }
            }
        });
        dialog.present();
    }

    fn preset_button(&self, icon: &str, title: &str) -> gtk::Button {
        gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(self.t(title))
            .valign(gtk::Align::Center)
            .build()
    }

    fn new_preset(self: &Rc<Self>) {
        self.edit_preset(None, Preset::default());
    }

    #[allow(deprecated)]
    fn edit_preset(self: &Rc<Self>, id: Option<String>, preset: Preset) {
        let dialog =
            adw::MessageDialog::new(Some(&self.window), Some(&self.t("edit_preset")), None);
        let group = adw::PreferencesGroup::new();
        let name = adw::EntryRow::builder()
            .title(self.t("name"))
            .text(self.preset_name(&preset))
            .build();
        if id.is_none() && preset.name.is_empty() {
            name.set_text("");
        }
        let layout = combo(
            self.locale,
            "scaler",
            Layout::ALL.iter().map(|item| item.id()),
            index(Layout::ALL, preset.layout),
        );
        let filter = combo(
            self.locale,
            "filter",
            Filter::ALL.iter().map(|item| item.id()),
            index(Filter::ALL, preset.filter),
        );
        let mode = combo(
            self.locale,
            "mode",
            Presentation::ALL.iter().map(|item| item.id()),
            index(Presentation::ALL, preset.presentation),
        );
        let device = adw::ComboRow::builder().title(self.t("render_gpu")).build();
        device.connect_selected_item_notify(|row| {
            let title = row
                .selected_item()
                .and_downcast::<gtk::StringObject>()
                .map(|item| item.string());
            row.set_tooltip_text(title.as_deref());
        });
        let saved_device = preset.render_device.clone();
        let device_ids = Rc::new(RefCell::new(vec![String::new()]));
        let automatic = self.t("render_gpu_auto");
        let missing = self.t("render_gpu_saved");
        let model = gtk::StringList::new(&[&automatic]);
        if !saved_device.is_empty() {
            model.append(&format!("{missing} ({saved_device})"));
            device_ids.borrow_mut().push(saved_device.clone());
        }
        device.set_model(Some(&model));
        device.set_selected(u32::from(!saved_device.is_empty()));

        let sharpness = adw::SpinRow::with_range(0.0, 100.0, 5.0);
        sharpness.set_title(&self.t("filter_sharpness"));
        sharpness.set_subtitle(&self.t("filter_sharpness_hint"));
        sharpness.set_value(f64::from(preset.sharpness));

        let post_sharpen = adw::SwitchRow::builder()
            .title(self.t("post_sharpen"))
            .subtitle(self.t("post_sharpen_hint"))
            .active(preset.post_sharpen)
            .build();
        let post_deband = adw::SwitchRow::builder()
            .title(self.t("post_deband"))
            .subtitle(self.t("post_deband_hint"))
            .active(preset.post_deband)
            .build();

        let sync_gpu_controls = {
            let filter = filter.downgrade();
            let device = device.downgrade();
            let sharpness = sharpness.downgrade();
            let post_sharpen = post_sharpen.downgrade();
            let post_deband = post_deband.downgrade();
            move || {
                let (Some(filter), Some(device), Some(sharpness), Some(post_sharpen), Some(post_deband)) = (
                    filter.upgrade(),
                    device.upgrade(),
                    sharpness.upgrade(),
                    post_sharpen.upgrade(),
                    post_deband.upgrade(),
                ) else {
                    return;
                };
                let selected = Filter::ALL[filter.selected() as usize];
                let gpu = selected.uses_gpu();
                device.set_visible(gpu);
                post_sharpen.set_visible(gpu && selected != Filter::Cas);
                post_deband.set_visible(gpu);
                sharpness.set_visible(
                    selected.supports_sharpness()
                        || (gpu && selected != Filter::Cas && post_sharpen.is_active()),
                );
            }
        };
        sync_gpu_controls();
        filter.connect_selected_notify({
            let sync = sync_gpu_controls.clone();
            move |_| sync()
        });
        post_sharpen.connect_active_notify({
            let sync = sync_gpu_controls.clone();
            move |_| sync()
        });
        glib::spawn_future_local({
            let device = device.downgrade();
            let ids = device_ids.clone();
            let unavailable = self.t("render_gpu_list_failed");
            async move {
                let result = render_client::devices().await;
                let Some(device) = device.upgrade() else {
                    return;
                };
                let Ok(choices) = result else {
                    device.set_subtitle(&unavailable);
                    return;
                };
                let mut ids = ids.borrow_mut();
                let selected = ids
                    .get(device.selected() as usize)
                    .cloned()
                    .unwrap_or_default();
                let new_model = gtk::StringList::new(&[&automatic]);
                ids.truncate(1);
                for item in choices {
                    new_model.append(&item.title());
                    ids.push(item.id);
                }
                if !saved_device.is_empty() && !ids.contains(&saved_device) {
                    new_model.append(&format!("{missing} ({saved_device})"));
                    ids.push(saved_device);
                }
                device.set_model(Some(&new_model));
                device.set_selected(ids.iter().position(|id| *id == selected).unwrap_or(0) as u32);
            }
        });
        group.add(&name);
        for row in [&layout, &filter, &mode] {
            group.add(row);
        }
        group.add(&device);
        group.add(&sharpness);
        group.add(&post_sharpen);
        group.add(&post_deband);
        let crop_expander = adw::ExpanderRow::builder()
            .title(self.t("crop"))
            .subtitle(self.t("crop_hint"))
            .expanded(!preset.crop.is_empty())
            .build();
        let crop_spin = |key: &str, initial: u32| {
            let row = adw::SpinRow::with_range(0.0, f64::from(Crop::MAX), 1.0);
            row.set_title(&self.t(key));
            row.set_value(f64::from(initial));
            crop_expander.add_row(&row);
            row
        };
        let crop_left = crop_spin("crop_left", preset.crop.left);
        let crop_top = crop_spin("crop_top", preset.crop.top);
        let crop_right = crop_spin("crop_right", preset.crop.right);
        let crop_bottom = crop_spin("crop_bottom", preset.crop.bottom);
        group.add(&crop_expander);
        let hint = gtk::Label::builder()
            .label(self.t("preview_only_options"))
            .wrap(true)
            .xalign(0.0)
            .build();
        hint.add_css_class("dim-label");
        group.add(&hint);
        dialog.set_extra_child(Some(&group));
        dialog.add_response("cancel", &self.t("cancel"));
        dialog.add_response("save", &self.t("save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("save", !name.text().trim().is_empty());
        name.connect_changed({
            let dialog = dialog.downgrade();
            move |name| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.set_response_enabled("save", !name.text().trim().is_empty());
                }
            }
        });
        dialog.connect_response(Some("save"), {
            let weak = Rc::downgrade(self);
            move |_, _| {
                let Some(view) = weak.upgrade() else { return };
                let id = id
                    .clone()
                    .unwrap_or_else(|| glib::uuid_string_random().to_string());
                let name = name.text();
                let preset = Preset {
                    name: if id == DEFAULT_ID && name == view.t("default_preset") {
                        String::new()
                    } else {
                        name.trim().into()
                    },
                    layout: Layout::ALL[layout.selected() as usize],
                    filter: Filter::ALL[filter.selected() as usize],
                    presentation: Presentation::ALL[mode.selected() as usize],
                    render_device: device_ids
                        .borrow()
                        .get(device.selected() as usize)
                        .cloned()
                        .unwrap_or_default(),
                    crop: Crop::from_values([
                        crop_left.value() as u32,
                        crop_top.value() as u32,
                        crop_right.value() as u32,
                        crop_bottom.value() as u32,
                    ]),
                    sharpness: sharpness.value().round().clamp(0.0, 100.0) as u32,
                    post_sharpen: post_sharpen.is_active(),
                    post_deband: post_deband.is_active(),
                };
                let result = view.settings.borrow_mut().save_preset(&id, &preset);
                match result {
                    Ok(()) => view.reload_presets(),
                    Err(error) => view.save_failed(error),
                }
            }
        });
        dialog.present();
    }

    fn active_preset(&self) -> Preset {
        let settings = self.settings.borrow();
        settings.preset(&settings.selected())
    }

    fn show_mode(&self) {
        let preset = self.active_preset();
        let preview_only = portal_required(&preset);
        let desktop = native::Desktop::current();
        let key = if !preview_only && session::interactive_desktop_unsupported() {
            "desktop_interactive_unsupported"
        } else if !preview_only && matches!(desktop, Some(native::Desktop::Treeland)) {
            "backend_treeland_in_progress"
        } else if interactive_gpu(&preset) {
            "backend_gpu_auto"
        } else if preview_only {
            "backend_portal"
        } else if is_plasma() {
            "backend_auto"
        } else if is_gnome() {
            "backend_gnome_auto"
        } else if preset.layout == Layout::Fit && desktop.is_some() {
            "backend_native_auto"
        } else {
            "backend_portal"
        };
        self.backend.set_subtitle(&self.t(key));
    }

    fn launch(self: &Rc<Self>) {
        if !self.stop("Stop button")
            && let Some(app) = self.window.application()
        {
            self.start(&app, Target::Choose);
        }
    }

    fn escape(self: &Rc<Self>) -> gtk::EventControllerKey {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let weak = Rc::downgrade(self);
            move |_, key, _, _| {
                if key == gdk::Key::Escape && weak.upgrade().is_some_and(|view| view.stop("Escape"))
                {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        keys
    }

    fn start(self: &Rc<Self>, app: &impl IsA<gio::Application>, target: Target) {
        let fallback_preset = self.active_preset();
        let inhibit = self.settings.borrow().inhibit_idle();
        let (sender, receiver) = oneshot::channel();
        self.active
            .replace(Some(Rc::new(RefCell::new(Some(sender)))));
        self.status.set_tooltip_text(None);
        self.status.set_title(&self.t(match target {
            Target::Active => "starting_active_window",
            Target::Choose => "select_window",
        }));
        self.launch.set_label(&self.t("cancel"));
        let hold = app.hold();
        let idle = inhibit.then(|| IdleInhibitor::acquire(&self.window));
        let view = self.clone();
        let signals = [libc::SIGINT, libc::SIGTERM].map(|signal| {
            let weak = Rc::downgrade(self);
            glib::source::unix_signal_add_local(signal, move || {
                if let Some(view) = weak.upgrade() {
                    view.closing.set(true);
                    view.stop("termination signal");
                }
                glib::ControlFlow::Continue
            })
        });
        glib::spawn_future_local(async move {
            let _hold = hold;
            let _idle = idle;
            let preset = if target == Target::Active {
                view.resolve_active_preset().await.unwrap_or(fallback_preset)
            } else {
                fallback_preset
            };
            let result = view.magnify(receiver, &preset, target).await;
            for signal in signals {
                signal.remove();
            }
            view.active.borrow_mut().take();
            view.reset(result, target);
        });
    }

    async fn resolve_active_preset(&self) -> Option<Preset> {
        let conn = self.window.application()?.dbus_connection()?;
        let app_id = if is_plasma() {
            let _ = kwin::discover().await;
            kwin::active_app_id(&conn).await
        } else if is_gnome() {
            gnome::active_app_id(&conn).await
        } else {
            None
        }?;
        let normalized = normalize_app_id(&app_id)?;
        self.last_app_id.replace(Some(normalized.clone()));
        let settings = self.settings.borrow();
        let preset_id = settings.preset_for_app(&normalized)?;
        diagnostic!("picalens matched app={normalized} preset={preset_id}");
        Some(settings.preset(&preset_id))
    }

    fn trigger_screenshot(&self) {
        if let Some(capture) = self.preview_screenshot.borrow().clone() {
            capture();
            return;
        }
        if !self.kwin_active.get() {
            return;
        }
        let Some(conn) = self
            .window
            .application()
            .and_then(|app| app.dbus_connection())
        else {
            return;
        };
        let dir = self.auto_screenshot_dir();
        glib::spawn_future_local(async move {
            if let Err(error) = std::fs::create_dir_all(&dir) {
                diagnostic!("picalens could not create screenshot directory: {error}");
                return;
            }
            let path = next_screenshot_path(&dir);
            match kwin::capture_screenshot(&conn, &path).await {
                Ok(true) => diagnostic!("picalens requested screenshot path={}", path.display()),
                Ok(false) => diagnostic!("picalens screenshot unavailable"),
                Err(error) => diagnostic!("picalens screenshot failed: {error}"),
            }
        });
    }

    fn auto_screenshot_dir(&self) -> PathBuf {
        let configured = self.settings.borrow().screenshot_dir();
        if !configured.is_empty() {
            return PathBuf::from(configured);
        }
        glib::user_special_dir(glib::UserDirectory::Pictures)
            .unwrap_or_else(glib::home_dir)
            .join("PicaLens")
    }

    fn stop(&self, reason: &'static str) -> bool {
        if let Some(stop) = self.active.borrow().as_ref() {
            preview::request_stop(stop, reason);
            true
        } else {
            false
        }
    }

    async fn magnify(
        self: &Rc<Self>,
        mut stop: Stop,
        preset: &Preset,
        target: Target,
    ) -> Result<String> {
        let interactive_gpu = interactive_gpu(preset);
        if interactive_gpu && preset.layout == Layout::Integer {
            return Err("interactive_filter_unavailable".into());
        }
        let preview_only = portal_required(preset);
        let render_worker = preset.uses_render_worker();
        if render_worker && !preview_only && !is_plasma() {
            return Err("interactive_gpu_backend".into());
        }
        if !preview_only && session::interactive_desktop_unsupported() {
            return Err("desktop_interactive_unsupported".into());
        }
        let desktop = native::Desktop::current();
        if !preview_only
            && !render_worker
            && preset.layout == Layout::Fit
            && let Some(desktop) = desktop
        {
            if !preset.crop.is_empty() {
                return Err("interactive_crop_unavailable".into());
            }
            let backend = futures_util::select_biased! {
                reason = stop => return Ok(reason.unwrap_or("application shutdown").into()),
                backend = native::discover(desktop, preset.filter).fuse() => backend,
            };
            if let Some(backend) = backend {
                self.backend.set_subtitle(&self.t(desktop.label()));
                self.window.set_visible(false);
                return native::magnify(backend, stop, preset.filter).await;
            }
        }
        if !preview_only && matches!(desktop, Some(native::Desktop::Treeland)) {
            return Err("treeland_interactive_unavailable".into());
        }
        let hide_cursor_seconds = self.settings.borrow().hide_cursor_seconds();
        let needs_portal = preview_only || session::portal_desktop();
        if needs_portal && target == Target::Active && !interactive_gpu {
            return Err("active_window_preset".into());
        }
        let conn = self
            .window
            .application()
            .and_then(|app| app.dbus_connection());
        if !needs_portal {
            let backend = futures_util::select_biased! {
                reason = stop => return Ok(reason.unwrap_or("application shutdown").into()),
                backend = async {
                    let Some(backend) = kwin::discover().await? else { return Ok::<_, Box<dyn std::error::Error>>(None) };
                    if render_worker && !kwin::supports_render_worker(&backend).await? {
                        return Err("interactive_gpu_backend".into());
                    }
                    if !kwin::supports_filter(&backend, preset.filter).await? {
                        return Err("interactive_filter_unavailable".into());
                    }
                    if kwin::supports_layout(&backend, preset.layout).await? { Ok(Some(backend)) } else { Err("kwin_upgrade".into()) }
                }.fuse() => backend,
            }?;
            if let Some(backend) = backend {
                self.backend.set_subtitle(&self.t("backend_kwin"));
                let active = if target == Target::Active {
                    Some(conn.as_ref().ok_or("active_window_backend")?)
                } else {
                    self.window.set_visible(false);
                    None
                };
                self.kwin_active.set(true);
                let result = kwin::magnify(backend, stop, preset, hide_cursor_seconds, active).await;
                self.kwin_active.set(false);
                return result;
            }
        }
        if render_worker && interactive_gpu {
            return Err("interactive_gpu_backend".into());
        }
        if interactive_gpu {
            return Err("interactive_filter_unavailable".into());
        }
        if !needs_portal
            && is_gnome()
            && let Some(conn) = conn.as_ref()
        {
            let available = futures_util::select_biased! {
                reason = stop => return Ok(reason.unwrap_or("application shutdown").into()),
                supported = gnome::available(conn, preset.layout).fuse() => supported,
            }?;
            if available {
                self.backend.set_subtitle(&self.t("backend_gnome"));
                if target == Target::Choose {
                    self.window.set_visible(false);
                }
                return gnome::magnify(
                    conn,
                    stop,
                    preset,
                    hide_cursor_seconds,
                    self.locale,
                    target,
                )
                .await;
            }
        }
        if target == Target::Active {
            return Err("active_window_backend".into());
        }
        if !preview_only && (is_gnome() || matches!(desktop, Some(native::Desktop::Niri))) {
            return Err("desktop_interactive_unsupported".into());
        }
        self.backend.set_subtitle(&self.t("backend_portal"));
        self.window.set_visible(false);
        diagnostic!("picalens backend=portal mode=view-only");
        session::capture_window(None, stop, |capture, size| {
            self.show_capture(capture, preset, size, hide_cursor_seconds)
        })
        .await
        .map(str::to_owned)
    }

    async fn show_capture(
        self: &Rc<Self>,
        capture: Capture,
        preset: &Preset,
        logical_size: Option<(i32, i32)>,
        hide_cursor_seconds: u32,
    ) -> Result<&'static str> {
        let started = Instant::now();
        let viewport = Viewport::new(preset.layout, preset.filter);
        let mut upscaler = Upscaler::new(preset.filter)?;
        let mut render_worker = if preset.uses_render_worker() {
            viewport.require_scaled();
            let hint = if preset.render_device.is_empty() {
                crate::gpu::current_render_node(&viewport.display())
            } else {
                None
            };
            Some(RenderUpscaler::new(preset, hint)?)
        } else {
            None
        };
        let mut gpu = preset
            .filter
            .uses_gpu()
            .then(|| GpuUpscaler::new(preset.filter, preset.sharpness_f32()))
            .filter(|_| render_worker.is_none());
        viewport.update_property(&[gtk::accessible::Property::Label(&self.t("preview_image"))]);
        let mut toasts = None;
        loop {
            if let Some(error) = capture.take_error() {
                return Err(error.into());
            }
            if let Some(frame) = capture.take_frame() {
                let mut pixels = Pixels::from(frame);
                if !preset.crop.is_empty() {
                    let scale = frame_crop_scale(pixels.size, logical_size);
                    let rect = preset
                        .crop
                        .visible(pixels.size, scale)
                        .ok_or("invalid_crop")?;
                    pixels = pixels.crop(rect).ok_or("invalid_crop")?;
                }
                viewport.set_frame(preview::rgba_texture(pixels.size, &pixels.bytes)?.upcast());
                if let Some(upscaler) = &mut upscaler {
                    upscaler.set_source(pixels);
                } else if let Some(worker) = &mut render_worker {
                    worker.set_source(pixels);
                } else if let Some(gpu) = &mut gpu {
                    gpu.set_source(pixels);
                }
                if toasts.is_none() {
                    toasts = Some(self.open_preview(
                        &viewport,
                        preset.presentation != Presentation::Windowed,
                        hide_cursor_seconds,
                    )?);
                    diagnostic!("picalens portal first frame drawn");
                }
            }
            if let Some(upscaler) = &mut upscaler
                && let Some(frame) = upscaler.update(viewport.processing_size())?
            {
                viewport.set_scaled_frame(frame)?;
            }
            if let Some(upscaler) = &mut gpu {
                match upscaler.update(&viewport.display(), viewport.processing_size()) {
                    Ok(Some((source, texture))) => viewport.set_scaled_texture(source, texture),
                    Ok(None) => {}
                    Err(error) => {
                        diagnostic!("picalens preview GPU filter unavailable: {error}");
                        gpu = None;
                        viewport.clear_scaled_texture();
                        if let Some(toasts) = &toasts {
                            toasts.add_toast(adw::Toast::new(&self.t("gpu_filter_fallback")));
                        }
                    }
                }
            }
            if let Some(worker) = &mut render_worker
                && let Some(frame) = worker.update(viewport.processing_size())?
            {
                viewport.set_scaled_frame(frame)?;
            }
            if toasts.is_none() && started.elapsed() > Duration::from_secs(15) {
                return Err("no_frame".into());
            }
            glib::timeout_future(Duration::from_millis(16)).await;
        }
    }

    fn open_preview(
        self: &Rc<Self>,
        viewport: &Viewport,
        fullscreen: bool,
        hide_cursor_seconds: u32,
    ) -> Result<adw::ToastOverlay> {
        let app = self.window.application().ok_or("application shutdown")?;
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .title(self.t("backend_portal"))
            .default_width(960)
            .default_height(640)
            .build();
        let view = gtk::Overlay::builder().child(viewport).build();
        let tools = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        tools.add_css_class("toolbar");
        tools.set_halign(gtk::Align::Center);
        tools.set_valign(gtk::Align::Start);
        tools.set_margin_top(8);
        let fullscreen_button = self.preset_button("view-fullscreen-symbolic", "toggle_fullscreen");
        let screenshot = self.preset_button("camera-photo-symbolic", "screenshot");
        let stop = gtk::Button::with_label(&self.t("stop"));
        for button in [&fullscreen_button, &screenshot, &stop] {
            tools.append(button);
        }
        view.add_overlay(&tools);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&view));
        window.set_content(Some(&toasts));
        toasts.add_toast(adw::Toast::new(&self.t("preview_hint")));

        let idle_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let arm_hide_cursor: Rc<dyn Fn()> = Rc::new({
            let view = view.downgrade();
            let tools = tools.downgrade();
            let idle_timer = idle_timer.clone();
            move || {
                if let Some(id) = idle_timer.borrow_mut().take() {
                    id.remove();
                }
                if let Some(view) = view.upgrade() {
                    view.set_cursor(None);
                }
                if hide_cursor_seconds == 0 {
                    return;
                }
                let view = view.clone();
                let tools = tools.clone();
                let timer_slot = idle_timer.clone();
                let id = glib::timeout_add_local_once(
                    Duration::from_secs(u64::from(hide_cursor_seconds)),
                    move || {
                        timer_slot.borrow_mut().take();
                        if let Some(view) = view.upgrade() {
                            let hidden = gdk::Cursor::from_name("none", None);
                            view.set_cursor(hidden.as_ref());
                        }
                        if let Some(tools) = tools.upgrade() {
                            set_tools_visible(&tools, false);
                        }
                    },
                );
                idle_timer.borrow_mut().replace(id);
            }
        });
        arm_hide_cursor();

        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        motion.connect_motion({
            let tools = tools.downgrade();
            let arm = arm_hide_cursor.clone();
            move |_, _, y| {
                arm();
                if let Some(tools) = tools.upgrade() {
                    set_tools_visible(&tools, y <= 58.0);
                }
            }
        });
        view.add_controller(motion);
        window.connect_destroy({
            let idle_timer = idle_timer.clone();
            move |_| {
                if let Some(id) = idle_timer.borrow_mut().take() {
                    id.remove();
                }
            }
        });
        let switch = {
            let window = window.downgrade();
            move || {
                if let Some(window) = window.upgrade() {
                    if window.is_fullscreen() {
                        window.unfullscreen();
                    } else {
                        window.fullscreen();
                    }
                }
            }
        };
        fullscreen_button.connect_clicked({
            let switch = switch.clone();
            move |_| switch()
        });
        let save: Rc<dyn Fn()> = Rc::new({
            let weak = Rc::downgrade(self);
            let viewport = viewport.downgrade();
            let window = window.downgrade();
            let toasts = toasts.downgrade();
            move || {
                if let (Some(owner), Some(viewport), Some(window), Some(toasts)) = (
                    weak.upgrade(),
                    viewport.upgrade(),
                    window.upgrade(),
                    toasts.upgrade(),
                ) {
                    owner.capture_screenshot(&viewport, &window, &toasts);
                }
            }
        });
        self.preview_screenshot.replace(Some(save.clone()));
        screenshot.connect_clicked({
            let save = save.clone();
            move |_| save()
        });
        stop.connect_clicked({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(view) = weak.upgrade() {
                    view.stop("Stop button");
                }
            }
        });
        let keys = self.escape();
        window.add_controller(keys);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            match key {
                gdk::Key::F11 => switch(),
                gdk::Key::F12 => save(),
                gdk::Key::Tab => {
                    set_tools_visible(&tools, true);
                    return glib::Propagation::Proceed;
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        window.add_controller(keys);
        window.connect_close_request({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(view) = weak.upgrade() {
                    view.stop("preview window closed");
                }
                glib::Propagation::Stop
            }
        });
        if fullscreen {
            window.fullscreen();
        }
        window.present();
        self.capture_window.replace(Some(window));
        Ok(toasts)
    }

    fn capture_screenshot(
        &self,
        viewport: &Viewport,
        window: &adw::ApplicationWindow,
        toasts: &adw::ToastOverlay,
    ) {
        let Some(image) = viewport.screenshot() else {
            return;
        };
        let configured_dir = self.settings.borrow().screenshot_dir();
        if !configured_dir.is_empty() {
            let dir = PathBuf::from(configured_dir);
            let key = if std::fs::create_dir_all(&dir).is_ok()
                && image.save_to_png(next_screenshot_path(&dir)).is_ok()
            {
                "screenshot_saved"
            } else {
                "screenshot_failed"
            };
            toasts.add_toast(adw::Toast::new(&self.t(key)));
            return;
        }
        let picker = gtk::FileDialog::builder()
            .title(self.t("screenshot"))
            .modal(true)
            .build();
        picker.set_initial_name(Some(&default_screenshot_name()));
        let window = window.clone();
        let toasts = toasts.clone();
        let locale = self.locale;
        glib::spawn_future_local(async move {
            let Ok(file) = picker.save_future(Some(&window)).await else {
                return;
            };
            let key = if let Some(path) = file.path() {
                if image.save_to_png(path).is_ok() {
                    "screenshot_saved"
                } else {
                    "screenshot_failed"
                }
            } else {
                "screenshot_failed"
            };
            toasts.add_toast(adw::Toast::new(&locale.text(key)));
        });
    }

    fn reset(&self, result: Result<String>, target: Target) {
        self.preview_screenshot.borrow_mut().take();
        self.kwin_active.set(false);
        let preview = self.capture_window.take();
        let showed_preview = preview.is_some();
        if let Some(window) = preview {
            window.destroy();
        }
        self.launch.set_label(&self.t("choose_window"));
        let mut show_controls = target == Target::Choose;
        match result {
            Ok(reason) => {
                diagnostic!("picalens stopped: {reason}");
                let key = if reason == session::PORTAL_CLOSED && !showed_preview {
                    "portal_closed_before_frame"
                } else {
                    "ready"
                };
                self.status.set_title(&self.t(key));
                if reason == native::FINISHED {
                    // The compositor may have just transferred focus to a modal window.
                    show_controls = false;
                } else {
                    show_controls |= reason == "PicaLens opened";
                }
            }
            Err(error) => {
                diagnostic!("picalens stopped: {error}");
                let error = error.to_string();
                let key = match error.as_str() {
                    "no_frame"
                    | "window_portal_unavailable"
                    | "portal_non_window"
                    | "kwin_upgrade"
                    | "gnome_unavailable"
                    | "gnome_no_windows"
                    | "gnome_source_lost"
                    | "native_backend_lost"
                    | "native_backend_busy"
                    | "desktop_interactive_unsupported"
                    | "treeland_interactive_unavailable"
                    | "interactive_filter_unavailable"
                    | "interactive_gpu_backend"
                    | "render_gpu_missing"
                    | "render_gpu_driver"
                    | "render_size"
                    | "render_worker_failed"
                    | "no_active_window"
                    | "active_window_preset"
                    | "active_window_backend"
                    | "gnome_upgrade"
                    | "invalid_crop"
                    | "interactive_crop_unavailable" => error.as_str(),
                    _ => "magnify_failed",
                };
                self.status.set_title(&self.t(key));
                self.status.set_tooltip_text(Some(&error));
                show_controls = true;
            }
        }
        self.show_mode();
        if self.closing.get() {
            if let Some(app) = self.window.application() {
                app.quit();
            }
        } else if show_controls {
            self.window.present();
        }
    }
}

fn portal_required(preset: &Preset) -> bool {
    preset.presentation != Presentation::Fullscreen
        || preset.layout == Layout::Integer
        || (preset.filter != Filter::Linear && !preset.filter.uses_gpu())
}

fn interactive_gpu(preset: &Preset) -> bool {
    preset.presentation == Presentation::Fullscreen && preset.filter.uses_gpu()
}

fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|desktop| desktop.eq_ignore_ascii_case("GNOME"))
}

fn is_plasma() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|desktop| {
            ["KDE", "Plasma"]
                .iter()
                .any(|name| desktop.eq_ignore_ascii_case(name))
        })
}

fn set_tools_visible(tools: &gtk::Box, visible: bool) {
    tools.set_opacity(if visible { 1.0 } else { 0.0 });
    tools.set_can_target(visible);
}

fn apply_theme(style: &adw::StyleManager, theme: Theme) {
    style.set_color_scheme(match theme {
        Theme::System => adw::ColorScheme::Default,
        Theme::Light => adw::ColorScheme::ForceLight,
        Theme::Dark => adw::ColorScheme::ForceDark,
    });
}

fn index<T: PartialEq>(items: &[T], item: T) -> usize {
    items.iter().position(|value| *value == item).unwrap_or(0)
}

fn combo<'a>(
    locale: Locale,
    title: &str,
    ids: impl IntoIterator<Item = &'a str>,
    selected: usize,
) -> adw::ComboRow {
    let labels = ids
        .into_iter()
        .map(|id| locale.text(id))
        .collect::<Vec<_>>();
    let model = gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    adw::ComboRow::builder()
        .title(locale.text(title))
        .model(&model)
        .selected(selected as u32)
        .build()
}

async fn update_shortcut_label(label: &gtk::Label, desktop: &DesktopShortcut, locale: Locale) {
    match desktop.label().await {
        Ok(key) => label.set_label(&key.unwrap_or_else(|| locale.text("disabled"))),
        Err(error) => {
            diagnostic!("picalens could not read desktop shortcut: {error}");
            label.set_label(&locale.text("unavailable"));
        }
    }
}

#[allow(deprecated)] // AdwMessageDialog is available with the libadwaita 1.4 minimum.
fn edit_shortcut(
    window: &adw::ApplicationWindow,
    label: &gtk::Label,
    desktop: DesktopShortcut,
    locale: Locale,
) {
    let dialog = adw::MessageDialog::new(
        Some(window),
        Some(&locale.text("set_shortcut")),
        Some(&locale.text("shortcut_hint")),
    );
    dialog.add_response("cancel", &locale.text("cancel"));
    dialog.set_close_response("cancel");

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let dialog = dialog.downgrade();
        let label = label.clone();
        let saving = Rc::new(Cell::new(false));
        move |controller, key, _, modifiers| {
            use glib::Propagation::{Proceed, Stop};
            let Some(dialog) = dialog.upgrade() else {
                return Proceed;
            };
            if saving.get() {
                return Stop;
            }
            let modifiers = modifiers & gtk::accelerator_get_default_mod_mask();
            if key == gdk::Key::Escape && modifiers.is_empty() {
                dialog.close();
                return Stop;
            }
            if controller.current_event().is_some_and(|event| {
                event
                    .downcast_ref::<gdk::KeyEvent>()
                    .is_some_and(gdk::KeyEvent::is_modifier)
            }) {
                return Stop;
            }
            let chord = if key == gdk::Key::BackSpace && modifiers.is_empty() {
                None
            } else {
                let key = key.to_lower();
                let usable = modifiers.intersects(
                    gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::ALT_MASK
                        | gdk::ModifierType::SUPER_MASK
                        | gdk::ModifierType::META_MASK,
                ) || (gdk::Key::F1..=gdk::Key::F35).contains(&key);
                if !usable || !gtk::accelerator_valid(key, modifiers) {
                    dialog.set_body(&locale.text("shortcut_invalid"));
                    return Stop;
                }
                Some((key, modifiers))
            };

            saving.set(true);
            dialog.set_response_enabled("cancel", false);
            let saving = saving.clone();
            let desktop = desktop.clone();
            let label = label.clone();
            glib::spawn_future_local(async move {
                match desktop.save(chord).await {
                    Ok(()) => {
                        update_shortcut_label(&label, &desktop, locale).await;
                        dialog.close();
                    }
                    Err(error) => {
                        dialog.set_body(&format!(
                            "{}
{error}",
                            locale.text("shortcut_failed")
                        ));
                        dialog.set_response_enabled("cancel", true);
                        saving.set(false);
                    }
                }
            });
            Stop
        }
    });
    dialog.add_controller(keys);
    dialog.present();
}

struct IdleInhibitor {
    app: gtk::Application,
    cookie: u32,
}

impl IdleInhibitor {
    fn acquire(window: &adw::ApplicationWindow) -> Option<Self> {
        let app = window.application()?;
        let cookie = app.inhibit(
            Some(window),
            gtk::ApplicationInhibitFlags::IDLE,
            Some("PicaLens window magnification"),
        );
        (cookie != 0).then_some(Self { app, cookie })
    }
}

impl Drop for IdleInhibitor {
    fn drop(&mut self) {
        self.app.uninhibit(self.cookie);
    }
}

fn frame_crop_scale((width, height): (u32, u32), logical: Option<(i32, i32)>) -> f64 {
    let Some((lw, lh)) = logical.filter(|&(w, h)| w > 0 && h > 0) else {
        return 1.0;
    };
    let sx = f64::from(width) / f64::from(lw);
    let sy = f64::from(height) / f64::from(lh);
    if (sx - sy).abs() <= 0.05 && sx > 0.0 {
        sx
    } else {
        1.0
    }
}

fn default_screenshot_name() -> String {
    let stamp = glib::DateTime::now_local()
        .and_then(|now| now.format("%Y%m%d-%H%M%S"))
        .map(String::from)
        .unwrap_or_else(|_| "capture".into());
    format!("PicaLens-{stamp}.png")
}

fn next_screenshot_path(dir: &Path) -> PathBuf {
    let base = default_screenshot_name();
    let candidate = dir.join(&base);
    if !candidate.exists() {
        return candidate;
    }
    let stem = base.strip_suffix(".png").unwrap_or(&base);
    for suffix in 1..1000 {
        let next = dir.join(format!("{stem}-{suffix}.png"));
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!("{stem}-{}.png", glib::uuid_string_random()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_portal_crop_scale_and_unique_screenshot_paths() {
        assert!((frame_crop_scale((1200, 900), Some((800, 600))) - 1.5).abs() < 1e-6);
        assert_eq!(frame_crop_scale((800, 600), None), 1.0);
        assert_eq!(frame_crop_scale((1600, 600), Some((800, 600))), 1.0);

        let dir = std::env::temp_dir().join(glib::uuid_string_random().as_str());
        std::fs::create_dir(&dir).unwrap();
        let first = next_screenshot_path(&dir);
        assert!(first.file_name().unwrap().to_str().unwrap().starts_with("PicaLens-"));
        std::fs::write(&first, b"png").unwrap();
        let second = next_screenshot_path(&dir);
        assert_ne!(first, second);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
