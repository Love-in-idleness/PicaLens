use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use crate::{
    gamescope::{self, Gamescope},
    locale::Locale,
    settings::Settings,
};
use adw::prelude::*;
use gtk::{gio, glib};

pub struct GamePage {
    pub widget: adw::PreferencesPage,
    window: glib::WeakRef<adw::ApplicationWindow>,
    settings: Rc<RefCell<Settings>>,
    locale: Locale,
    ids: RefCell<Vec<String>>,
    combo: adw::ComboRow,
    entries: HashMap<&'static str, adw::EntryRow>,
    choices: [(&'static str, &'static [&'static str], adw::ComboRow); 4],
    switches: HashMap<&'static str, adw::SwitchRow>,
    status: gtk::Label,
    start: gtk::Button,
    copy: gtk::Button,
    remove: gtk::Button,
    details: gtk::Button,
    running: Cell<bool>,
    changing: Cell<bool>,
    backend: RefCell<Option<Gamescope>>,
    log: RefCell<String>,
}

impl GamePage {
    pub fn new(
        window: &adw::ApplicationWindow,
        settings: Rc<RefCell<Settings>>,
        locale: Locale,
    ) -> Rc<Self> {
        let t = |key| locale.text(key);
        let widget = adw::PreferencesPage::new();
        let launch = group(&widget, &t("game_launch"));
        let profiles = group(&widget, &t("game_profiles"));
        let combo = adw::ComboRow::builder().title(t("game_profile")).build();
        profiles.add(&combo);
        let (buttons, buttons_row) = button_row();
        let save = gtk::Button::with_label(&t("save"));
        let remove = gtk::Button::with_label(&t("delete"));
        buttons.append(&save);
        buttons.append(&remove);
        profiles.add(&buttons_row);

        let command = group(&widget, &t("game_command"));
        let display = group(&widget, &t("game_display"));
        let advanced = group(&widget, &t("advanced"));
        let choices = [
            ("kind", gamescope::KINDS, &command),
            ("mode", gamescope::MODES, &display),
            ("scaler", gamescope::SCALERS, &display),
            ("filter", gamescope::FILTERS, &display),
        ]
        .map(|(key, items, group)| {
            let labels = items.iter().map(|value| t(value)).collect::<Vec<_>>();
            let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
            let strings = gtk::StringList::new(&labels);
            let row = adw::ComboRow::builder()
                .title(t(key))
                .model(&strings)
                .build();
            group.add(&row);
            (key, items, row)
        });
        let mut entries = HashMap::new();
        for &key in gamescope::TEXT {
            let row = adw::EntryRow::builder().title(t(key)).build();
            if matches!(key, "program" | "directory" | "prefix" | "runner") {
                let browse = gtk::Button::builder()
                    .icon_name("folder-open-symbolic")
                    .tooltip_text(t("browse"))
                    .valign(gtk::Align::Center)
                    .build();
                row.add_suffix(&browse);
                browse.connect_clicked({
                    let row = row.downgrade();
                    let window = window.downgrade();
                    move |_| {
                        let (Some(row), Some(window)) = (row.upgrade(), window.upgrade()) else {
                            return;
                        };
                        let picker = gtk::FileDialog::builder()
                            .title(locale.text(key))
                            .modal(true)
                            .build();
                        glib::spawn_future_local(async move {
                            let picked = if matches!(key, "directory" | "prefix") {
                                picker.select_folder_future(Some(&window)).await
                            } else {
                                picker.open_future(Some(&window)).await
                            };
                            if let Ok(file) = picked
                                && let Some(path) = file.path()
                            {
                                row.set_text(&path.to_string_lossy());
                            }
                        });
                    }
                });
            }
            if key == "name" {
                profiles.add(&row);
            } else {
                command.add(&row);
            }
            entries.insert(key, row);
        }
        for &(key, _) in gamescope::NUMERIC {
            let row = adw::EntryRow::builder()
                .title(t(key))
                .input_purpose(gtk::InputPurpose::Digits)
                .build();
            if key == "fps_limit" {
                row.set_tooltip_text(Some(&t("fps_limit_hint")));
            }
            display.add(&row);
            entries.insert(key, row);
        }
        let switches = gamescope::SWITCHES
            .iter()
            .map(|&(id, _)| {
                let row = adw::SwitchRow::builder().title(t(id)).build();
                if matches!(id, "hdr" | "vrr") {
                    row.set_subtitle(&t("display_support_hint"));
                }
                advanced.add(&row);
                (id, row)
            })
            .collect();

        let (actions, actions_row) = button_row();
        let start = gtk::Button::with_label(&t("launch"));
        start.add_css_class("suggested-action");
        let copy = gtk::Button::with_label(&t("copy_steam"));
        let details = gtk::Button::with_label(&t("details"));
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(t("refresh"))
            .build();
        actions.append(&start);
        actions.append(&copy);
        actions.append(&details);
        actions.append(&refresh);
        let status = gtk::Label::builder()
            .label(t("checking_gamescope"))
            .wrap(true)
            .xalign(0.0)
            .build();
        status.set_selectable(true);
        launch.add(&status);
        launch.add(&actions_row);
        let keys = gtk::Label::builder()
            .label(t("gamescope_keys"))
            .wrap(true)
            .xalign(0.0)
            .build();
        keys.add_css_class("dim-label");
        launch.add(&keys);
        let page = Rc::new(Self {
            widget,
            window: window.downgrade(),
            settings,
            locale,
            ids: RefCell::new(Vec::new()),
            combo,
            entries,
            choices,
            switches,
            status,
            start,
            copy,
            remove,
            details,
            running: Cell::new(false),
            changing: Cell::new(false),
            backend: RefCell::new(None),
            log: RefCell::new(String::new()),
        });
        page.combo.connect_selected_notify({
            let page = Rc::downgrade(&page);
            move |_| {
                if let Some(page) = page.upgrade()
                    && !page.changing.get()
                {
                    page.fill();
                }
            }
        });
        for i in [0, 3] {
            page.choices[i].2.connect_selected_notify({
                let page = Rc::downgrade(&page);
                move |_| {
                    if let Some(page) = page.upgrade() {
                        page.sensitivity();
                    }
                }
            });
        }
        for (button, handler) in [
            (save, Self::save as fn(&Rc<Self>)),
            (page.remove.clone(), Self::remove),
            (page.start.clone(), Self::start),
            (page.copy.clone(), Self::copy),
            (page.details.clone(), Self::details),
            (refresh, Self::probe),
        ] {
            button.connect_clicked({
                let page = Rc::downgrade(&page);
                move |_| {
                    if let Some(page) = page.upgrade() {
                        handler(&page);
                    }
                }
            });
        }
        page.reload(None);
        page.probe();
        page
    }

    fn probe(self: &Rc<Self>) {
        self.backend.replace(None);
        self.status("checking_gamescope");
        self.sensitivity();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result =
                gio::spawn_blocking(|| Gamescope::detect().map_err(|error| error.to_string()))
                    .await;
            let Some(page) = weak.upgrade() else { return };
            match result {
                Ok(Ok(Some(backend))) => {
                    page.log.replace(backend.hotkeys().to_owned());
                    page.backend.replace(Some(backend));
                    if !page.running.get() {
                        page.status("gamescope_ready");
                    }
                }
                Ok(Ok(None)) => page.status("install_gamescope"),
                Ok(Err(error)) => page.report("gamescope_unavailable", error),
                Err(_) => page.status("gamescope_unavailable"),
            }
            page.annotate_choices();
            page.sensitivity();
        });
    }

    fn status(&self, key: &str) {
        self.status.set_label(&self.locale.text(key));
    }

    fn report(&self, key: &str, log: String) {
        self.log.replace(log);
        self.status(key);
        self.details.set_sensitive(!self.log.borrow().is_empty());
    }

    fn current(&self) -> Option<String> {
        self.ids
            .borrow()
            .get(
                usize::try_from(self.combo.selected())
                    .ok()?
                    .checked_sub(1)?,
            )
            .cloned()
    }

    fn reload(&self, selected: Option<&str>) {
        let ids = self.settings.borrow().game_ids();
        let mut labels = vec![self.locale.text("new_game")];
        labels.extend(ids.iter().map(|id| self.settings.borrow().game(id, "name")));
        let model = gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        let index = selected
            .and_then(|selected| ids.iter().position(|id| id == selected))
            .map_or(0, |index| index as u32 + 1);
        self.ids.replace(ids);
        self.changing.set(true);
        self.combo.set_model(Some(&model));
        self.combo.set_selected(index);
        self.changing.set(false);
        self.fill();
    }

    fn fill(&self) {
        let id = self.current();
        let value = |key| {
            id.as_ref()
                .map(|id| self.settings.borrow().game(id, key))
                .unwrap_or_default()
        };
        for (key, entry) in &self.entries {
            entry.set_text(&value(key));
        }
        for (key, values, row) in &self.choices {
            row.set_selected(values.iter().position(|id| *id == value(key)).unwrap_or(0) as u32);
        }
        for (id, row) in &self.switches {
            row.set_active(value(id) == "true");
        }
        self.sensitivity();
    }

    fn annotate_choices(&self) {
        let backend = self.backend.borrow();
        let Some(backend) = backend.as_ref() else {
            return;
        };
        for (key, ids, row) in &self.choices[1..] {
            let selected = row.selected();
            let labels = ids
                .iter()
                .map(|id| {
                    let supported = match *key {
                        "scaler" => backend.scaler(id),
                        "filter" => backend.filter(id),
                        _ => match *id {
                            "fullscreen" => backend.has("--fullscreen"),
                            "borderless" => backend.has("--borderless"),
                            _ => true,
                        },
                    };
                    if supported {
                        self.locale.text(id)
                    } else {
                        format!(
                            "{} ({})",
                            self.locale.text(id),
                            self.locale.text("unsupported")
                        )
                    }
                })
                .collect::<Vec<_>>();
            row.set_model(Some(&gtk::StringList::new(
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
            row.set_selected(selected);
        }
    }

    fn sensitivity(&self) {
        let kind = self.choices[0].2.selected();
        let filter = self.choices[3]
            .1
            .get(self.choices[3].2.selected() as usize)
            .copied();
        let backend = self.backend.borrow();
        let ready = backend.is_some();
        self.start
            .set_sensitive(ready && kind != 2 && !self.running.get());
        self.copy.set_sensitive(ready);
        self.remove.set_sensitive(self.current().is_some());
        self.details.set_sensitive(!self.log.borrow().is_empty());
        for key in ["program", "arguments", "directory", "runner", "prefix"] {
            self.entries[key]
                .set_visible(kind != 2 && (!matches!(key, "runner" | "prefix") || kind == 1));
        }
        for &(id, flag) in gamescope::NUMERIC {
            self.entries[id].set_sensitive(
                backend.as_ref().is_none_or(|backend| backend.has(flag))
                    && (id != "sharpness" || matches!(filter, Some("fsr" | "nis" | "sgsr"))),
            );
        }
        for &(id, flag) in gamescope::SWITCHES {
            self.switches[id]
                .set_sensitive(backend.as_ref().is_none_or(|backend| backend.has(flag)));
        }
    }

    fn values(&self) -> HashMap<&'static str, String> {
        self.entries
            .iter()
            .map(|(&key, row)| (key, row.text().to_string()))
            .chain(
                self.switches
                    .iter()
                    .map(|(&key, row)| (key, row.is_active().to_string())),
            )
            .chain(self.choices.iter().map(|(key, ids, row)| {
                (
                    *key,
                    ids.get(row.selected() as usize)
                        .copied()
                        .unwrap_or_default()
                        .to_owned(),
                )
            }))
            .collect()
    }

    fn save(self: &Rc<Self>) {
        let mut values = self.values();
        let name = values["name"].trim();
        if name.is_empty() {
            self.status("missing_name");
            return;
        }
        values.insert("name", name.to_owned());
        let id = self
            .current()
            .unwrap_or_else(|| glib::uuid_string_random().to_string());
        let data = values.into_iter().collect::<Vec<_>>();
        let result = self.settings.borrow_mut().save_game(&id, &data);
        match result {
            Ok(()) => {
                self.reload(Some(&id));
                self.status("saved");
            }
            Err(error) => self.report("save_failed", error.to_string()),
        }
    }

    fn remove(self: &Rc<Self>) {
        if let Some(id) = self.current() {
            let result = self.settings.borrow_mut().delete("game", &id);
            match result {
                Ok(()) => {
                    self.reload(None);
                    self.status("deleted");
                }
                Err(error) => self.report("save_failed", error.to_string()),
            }
        }
    }

    fn copy(self: &Rc<Self>) {
        let values = self.values();
        let backend = self.backend.borrow();
        let Some(backend) = backend.as_ref() else {
            return;
        };
        match backend.options(|key| values[key].as_str()) {
            Ok(options) => {
                self.widget
                    .clipboard()
                    .set_text(&backend.steam_options(&options));
                self.status("copied_steam");
            }
            Err(key) => self.status(key),
        }
    }

    fn start(self: &Rc<Self>) {
        if self.running.get() {
            return;
        }
        let values = self.values();
        if values["program"].trim().is_empty() {
            self.status("missing_program");
            return;
        }
        let backend = self.backend.borrow();
        let Some(backend) = backend.as_ref() else {
            return;
        };
        let options = match backend.options(|key| values[key].as_str()) {
            Ok(options) => options,
            Err(key) => {
                self.status(key);
                return;
            }
        };
        let child = match backend.spawn(&options, |key| values[key].as_str()) {
            Ok(child) => child,
            Err(error) => {
                self.report("launch_failed", error.to_string());
                return;
            }
        };
        self.running.set(true);
        self.status("gamescope_started");
        self.sensitivity();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || gamescope::wait(child)).await;
            let Some(page) = weak.upgrade() else { return };
            page.running.set(false);
            match result {
                Ok((Ok(status), _)) if status.success() => page.status("game_ended"),
                Ok((Ok(status), log)) => page.report("launch_failed", format!("{status}\n{log}")),
                Ok((Err(error), log)) => page.report("launch_failed", format!("{error}\n{log}")),
                Err(_) => page.status("launch_failed"),
            }
            page.sensitivity();
        });
    }

    #[allow(deprecated)]
    fn details(self: &Rc<Self>) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let dialog =
            adw::MessageDialog::new(Some(&window), Some(&self.locale.text("details")), None);
        let log = gtk::TextView::builder()
            .editable(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::Char)
            .build();
        log.buffer().set_text(&self.log.borrow());
        let scroll = gtk::ScrolledWindow::builder()
            .child(&log)
            .min_content_height(200)
            .build();
        dialog.set_extra_child(Some(&scroll));
        dialog.add_response("close", &self.locale.text("close"));
        dialog.set_close_response("close");
        dialog.present();
    }
}

fn group(page: &adw::PreferencesPage, title: &str) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    page.add(&group);
    group
}

fn button_row() -> (gtk::Box, adw::ActionRow) {
    let row = adw::ActionRow::new();
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_valign(gtk::Align::Center);
    row.add_suffix(&buttons);
    (buttons, row)
}
