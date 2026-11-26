use std::{
    cell::{Cell, RefCell},
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
    session::{self, Result, Stop, Target},
    settings::{DEFAULT_ID, Filter, Language, Layout, Presentation, Preset, Settings, Theme},
    shortcut::{self, DesktopShortcut},
    upscale::{Pixels, Upscaler},
    viewport::Viewport,
};

const APP_ID: &str = "io.github.LoveInIdleness.PicaLens";

pub fn run() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    match args.get(1).and_then(|arg| arg.to_str()) {
        None if args.len() == 1 => {}
        Some("toggle") if args.len() == 2 => {}
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
        if command.arguments().len() == 1 {
            if !view.stop("PicaLens opened") {
                view.window.present();
            }
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
    filling_presets: Cell<bool>,
    active: RefCell<Option<StopSender>>,
    capture_window: RefCell<Option<adw::ApplicationWindow>>,
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
            filling_presets: Cell::new(false),
            active: RefCell::new(None),
            capture_window: RefCell::new(None),
            closing: Cell::new(false),
            _games: games,
        });
        view.reload_presets();
        for (button, handler) in [
            (add, Self::new_preset as fn(&Rc<Self>)),
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
            let subtitle = [
                preset.layout.id(),
                preset.filter.id(),
                preset.presentation.id(),
            ]
            .map(|key| self.t(key))
            .join(" · ");
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
        self.show_mode();
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
        group.add(&name);
        for row in [&layout, &filter, &mode] {
            group.add(row);
        }
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
        let key = if interactive_gpu(&preset) {
            "backend_gpu_auto"
        } else if portal_required(&preset) {
            "backend_portal"
        } else if is_plasma() {
            "backend_auto"
        } else if is_gnome() {
            "backend_gnome_auto"
        } else if preset.layout == Layout::Fit && native::Desktop::current().is_some() {
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
        let preset = self.active_preset();
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
            let result = view.magnify(receiver, &preset, target).await;
            for signal in signals {
                signal.remove();
            }
            view.active.borrow_mut().take();
            view.reset(result, target);
        });
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
        if !portal_required(preset)
            && preset.layout == Layout::Fit
            && let Some(desktop) = native::Desktop::current()
        {
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
        let needs_portal = portal_required(preset) || session::portal_desktop();
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
                return kwin::magnify(backend, stop, preset.layout, preset.filter, active).await;
            }
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
                return gnome::magnify(conn, stop, preset.layout, self.locale, target).await;
            }
        }
        if target == Target::Active {
            return Err("active_window_backend".into());
        }
        self.backend.set_subtitle(&self.t("backend_portal"));
        self.window.set_visible(false);
        diagnostic!("picalens backend=portal mode=view-only");
        session::capture_window(None, stop, |capture, _| self.show_capture(capture, preset))
            .await
            .map(str::to_owned)
    }

    async fn show_capture(
        self: &Rc<Self>,
        capture: Capture,
        preset: &Preset,
    ) -> Result<&'static str> {
        let started = Instant::now();
        let viewport = Viewport::new(preset.layout, preset.filter);
        let mut upscaler = Upscaler::new(preset.filter)?;
        let mut gpu = preset
            .filter
            .uses_gpu()
            .then(|| GpuUpscaler::new(preset.filter));
        viewport.update_property(&[gtk::accessible::Property::Label(&self.t("preview_image"))]);
        let mut toasts = None;
        loop {
            if let Some(error) = capture.take_error() {
                return Err(error.into());
            }
            if let Some(frame) = capture.take_frame() {
                let pixels = Pixels::from(frame);
                viewport.set_frame(preview::rgba_texture(pixels.size, &pixels.bytes)?.upcast());
                if let Some(upscaler) = &mut upscaler {
                    upscaler.set_source(pixels);
                } else if let Some(gpu) = &mut gpu {
                    gpu.set_source(pixels);
                }
                if toasts.is_none() {
                    toasts =
                        Some(self.open_preview(
                            &viewport,
                            preset.presentation != Presentation::Windowed,
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
        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        motion.connect_motion({
            let tools = tools.downgrade();
            move |_, _, y| {
                if let Some(tools) = tools.upgrade() {
                    set_tools_visible(&tools, y <= 58.0);
                }
            }
        });
        view.add_controller(motion);
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
        let save = {
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
        };
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
        let picker = gtk::FileDialog::builder()
            .title(self.t("screenshot"))
            .modal(true)
            .build();
        picker.set_initial_name(Some("PicaLens.png"));
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
                    | "interactive_filter_unavailable"
                    | "no_active_window"
                    | "active_window_preset"
                    | "active_window_backend" => error.as_str(),
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
