use std::path::{Path, PathBuf};

use gtk::glib;

use crate::session::Result;

pub const DEFAULT_ID: &str = "default";

macro_rules! choices {
    ($name:ident { $first:ident => $first_id:literal $(, $item:ident => $id:literal)* $(,)? }) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub enum $name { #[default] $first, $($item),* }
        impl $name {
            pub const ALL: &'static [Self] = &[Self::$first, $(Self::$item),*];
            pub fn id(self) -> &'static str {
                match self { Self::$first => $first_id, $(Self::$item => $id),* }
            }
            pub fn parse(id: &str) -> Self {
                Self::ALL.iter().copied().find(|item| item.id() == id).unwrap_or_default()
            }
        }
    };
}

choices!(Layout { Fit => "fit", Fill => "fill", Stretch => "stretch", Integer => "integer" });
choices!(Filter {
    Linear => "linear",
    Nearest => "nearest",
    Lanczos => "lanczos",
    Scale2x => "scale2x",
    Fsr => "fsr",
    Sgsr => "sgsr",
    Cas => "cas",
    SharpBilinear => "sharp-bilinear",
    Xbrz => "xbrz",
    Crt => "crt",
    Anime4k => "anime4k",
});
choices!(Presentation { Fullscreen => "fullscreen", Windowed => "windowed", Preview => "preview" });
choices!(Language { System => "system", English => "en", Chinese => "zh-CN", Japanese => "ja" });
choices!(Theme { System => "system", Light => "light", Dark => "dark" });

pub const DEFAULT_SHARPNESS: u32 = 80;

impl Filter {
    pub fn uses_gpu(self) -> bool {
        matches!(
            self,
            Self::Fsr
                | Self::Sgsr
                | Self::Cas
                | Self::SharpBilinear
                | Self::Xbrz
                | Self::Crt
                | Self::Anime4k
        )
    }

    pub fn requires_render_worker(self) -> bool {
        matches!(
            self,
            Self::Anime4k | Self::Cas | Self::SharpBilinear | Self::Xbrz | Self::Crt
        )
    }

    pub fn supports_sharpness(self) -> bool {
        matches!(self, Self::Fsr | Self::Sgsr | Self::Cas)
    }
}

/// Margins removed from the source window, in logical window pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Crop {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Crop {
    pub const MAX: u32 = 4096;
    const KEYS: [&'static str; 4] = ["crop-left", "crop-top", "crop-right", "crop-bottom"];

    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    pub fn values(self) -> [u32; 4] {
        [self.left, self.top, self.right, self.bottom]
    }

    pub fn from_values([left, top, right, bottom]: [u32; 4]) -> Self {
        let clamp = |value: u32| value.min(Self::MAX);
        Self {
            left: clamp(left),
            top: clamp(top),
            right: clamp(right),
            bottom: clamp(bottom),
        }
    }

    /// Returns the visible rectangle `(x, y, width, height)` inside `size`,
    /// after scaling the logical margins by `scale`.
    pub fn visible(self, (width, height): (u32, u32), scale: f64) -> Option<(u32, u32, u32, u32)> {
        let px = |value: u32| (f64::from(value) * scale).round() as u32;
        let [left, top, right, bottom] = self.values().map(px);
        let visible_w = width.checked_sub(left)?.checked_sub(right)?;
        let visible_h = height.checked_sub(top)?.checked_sub(bottom)?;
        (visible_w >= Self::MIN_VISIBLE && visible_h >= Self::MIN_VISIBLE)
            .then_some((left, top, visible_w, visible_h))
    }

    pub const MIN_VISIBLE: u32 = 16;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    pub name: String,
    pub layout: Layout,
    pub filter: Filter,
    pub presentation: Presentation,
    pub render_device: String,
    pub crop: Crop,
    pub sharpness: u32,
    pub post_sharpen: bool,
    pub post_deband: bool,
}

impl Default for Preset {
    fn default() -> Self {
        Self {
            name: String::new(),
            layout: Layout::default(),
            filter: Filter::default(),
            presentation: Presentation::default(),
            render_device: String::new(),
            crop: Crop::default(),
            sharpness: DEFAULT_SHARPNESS,
            post_sharpen: false,
            post_deband: false,
        }
    }
}

impl Preset {
    pub fn has_post_processing(&self) -> bool {
        self.post_sharpen || self.post_deband
    }

    pub fn uses_render_worker(&self) -> bool {
        self.filter.requires_render_worker()
            || (self.filter.uses_gpu()
                && (!self.render_device.is_empty() || self.has_post_processing()))
    }

    pub fn sharpness_f32(&self) -> f32 {
        self.sharpness.min(100) as f32 / 100.0
    }
}

/// Seconds without pointer motion before the cursor hides over a magnified
/// window. Zero disables hiding.
pub const HIDE_CURSOR_CHOICES: &[u32] = &[0, 1, 2, 3, 5];

/// Normalizes compositor application identifiers so KWin desktop file names,
/// GNOME application ids and X11 classes compare consistently.
pub fn normalize_app_id(app: &str) -> Option<String> {
    let app = app.trim();
    let app = app.strip_suffix(".desktop").unwrap_or(app).to_lowercase();
    let valid = !app.is_empty()
        && app.len() <= 255
        && !app.contains(['=', '[', ']', '\n', '\r'])
        && !app.starts_with('#');
    valid.then_some(app)
}

pub struct Settings {
    file: glib::KeyFile,
    path: PathBuf,
}

impl Settings {
    pub fn load() -> Self {
        Self::open(&glib::user_config_dir().join("picalens/settings.ini"))
    }

    fn open(path: &Path) -> Self {
        let file = glib::KeyFile::new();
        if let Err(error) = file.load_from_file(path, glib::KeyFileFlags::KEEP_COMMENTS)
            && !error.matches(glib::FileError::Noent)
        {
            diagnostic!("picalens could not read {}: {error}", path.display());
        }
        Self {
            file,
            path: path.to_owned(),
        }
    }

    fn edit(&mut self, change: impl FnOnce(&glib::KeyFile)) -> Result<()> {
        let file = glib::KeyFile::new();
        file.load_from_data(&self.file.to_data(), glib::KeyFileFlags::KEEP_COMMENTS)?;
        change(&file);
        std::fs::create_dir_all(self.path.parent().expect("settings file has a directory"))?;
        file.save_to_file(&self.path)?;
        self.file = file;
        Ok(())
    }

    pub fn language(&self) -> Language {
        Language::parse(&value(&self.file, "ui", "language"))
    }

    pub fn set_language(&mut self, language: Language) -> Result<()> {
        self.edit(|file| file.set_string("ui", "language", language.id()))
    }

    pub fn theme(&self) -> Theme {
        Theme::parse(&value(&self.file, "ui", "theme"))
    }

    pub fn set_theme(&mut self, theme: Theme) -> Result<()> {
        self.edit(|file| file.set_string("ui", "theme", theme.id()))
    }

    pub fn selected(&self) -> String {
        let id = value(&self.file, "scaling", "selected");
        if self.preset_ids().contains(&id) {
            id
        } else {
            DEFAULT_ID.into()
        }
    }

    pub fn select(&mut self, id: &str) -> Result<()> {
        self.edit(|file| file.set_string("scaling", "selected", id))
    }

    pub fn preset_ids(&self) -> Vec<String> {
        let mut ids = self.ids("preset");
        ids.retain(|id| id != DEFAULT_ID);
        ids.insert(0, DEFAULT_ID.into());
        ids
    }

    pub fn preset(&self, id: &str) -> Preset {
        let group = format!("preset {id}");
        let layout = self.file.string(&group, "layout").ok().or_else(|| {
            (id == DEFAULT_ID)
                .then(|| self.file.string("scaling", "layout").ok())
                .flatten()
        });
        Preset {
            name: value(&self.file, &group, "name"),
            layout: Layout::parse(layout.as_deref().unwrap_or_default()),
            filter: Filter::parse(&value(&self.file, &group, "filter")),
            presentation: Presentation::parse(&value(&self.file, &group, "presentation")),
            render_device: value(&self.file, &group, "render-device"),
            crop: Crop::from_values(
                Crop::KEYS.map(|key| self.file.integer(&group, key).unwrap_or(0).max(0) as u32),
            ),
            sharpness: self
                .file
                .integer(&group, "sharpness")
                .map(|value| value.clamp(0, 100) as u32)
                .unwrap_or(DEFAULT_SHARPNESS),
            post_sharpen: self.file.boolean(&group, "post-sharpen").unwrap_or(false),
            post_deband: self.file.boolean(&group, "post-deband").unwrap_or(false),
        }
    }

    pub fn save_preset(&mut self, id: &str, preset: &Preset) -> Result<()> {
        let group = format!("preset {id}");
        self.edit(|file| {
            file.set_string(&group, "name", &preset.name);
            file.set_string(&group, "layout", preset.layout.id());
            file.set_string(&group, "filter", preset.filter.id());
            file.set_string(&group, "presentation", preset.presentation.id());
            file.set_string(&group, "render-device", &preset.render_device);
            for (key, value) in Crop::KEYS.into_iter().zip(preset.crop.values()) {
                if value == 0 {
                    let _ = file.remove_key(&group, key);
                } else {
                    file.set_integer(&group, key, value as i32);
                }
            }
            let sharpness = preset.sharpness.min(100);
            if sharpness == DEFAULT_SHARPNESS {
                let _ = file.remove_key(&group, "sharpness");
            } else {
                file.set_integer(&group, "sharpness", sharpness as i32);
            }
            for (key, enabled) in [
                ("post-sharpen", preset.post_sharpen),
                ("post-deband", preset.post_deband),
            ] {
                if enabled {
                    file.set_boolean(&group, key, true);
                } else {
                    let _ = file.remove_key(&group, key);
                }
            }
        })
    }

    /// Application rules, sorted by application id.
    pub fn app_rules(&self) -> Vec<(String, String)> {
        let mut rules: Vec<_> = self
            .file
            .keys("apps")
            .map(|keys| {
                keys.iter()
                    .filter_map(|app| {
                        let preset = self.file.string("apps", app).ok()?;
                        Some((app.to_string(), preset.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        rules.sort();
        rules
    }

    pub fn set_app_rule(&mut self, app: &str, preset: &str) -> Result<()> {
        let app = normalize_app_id(app).ok_or("invalid_app_id")?;
        self.edit(|file| file.set_string("apps", &app, preset))
    }

    pub fn remove_app_rule(&mut self, app: &str) -> Result<()> {
        self.edit(|file| {
            let _ = file.remove_key("apps", app);
        })
    }

    /// The preset bound to `app`, if the rule and its preset still exist.
    pub fn preset_for_app(&self, app: &str) -> Option<String> {
        let app = normalize_app_id(app)?;
        let preset = self.file.string("apps", &app).ok()?.to_string();
        self.preset_ids().contains(&preset).then_some(preset)
    }

    pub fn hide_cursor_seconds(&self) -> u32 {
        let seconds = self.file.integer("ui", "hide-cursor").unwrap_or(0).max(0) as u32;
        if HIDE_CURSOR_CHOICES.contains(&seconds) { seconds } else { 0 }
    }

    pub fn set_hide_cursor_seconds(&mut self, seconds: u32) -> Result<()> {
        self.edit(|file| file.set_integer("ui", "hide-cursor", seconds as i32))
    }

    pub fn inhibit_idle(&self) -> bool {
        self.file.boolean("ui", "inhibit-idle").unwrap_or(false)
    }

    pub fn set_inhibit_idle(&mut self, inhibit: bool) -> Result<()> {
        self.edit(|file| file.set_boolean("ui", "inhibit-idle", inhibit))
    }

    /// Folder for automatic screenshots. Empty asks for a file each time.
    pub fn screenshot_dir(&self) -> String {
        value(&self.file, "ui", "screenshot-dir")
    }

    pub fn set_screenshot_dir(&mut self, dir: &str) -> Result<()> {
        self.edit(|file| {
            if dir.is_empty() {
                let _ = file.remove_key("ui", "screenshot-dir");
            } else {
                file.set_string("ui", "screenshot-dir", dir);
            }
        })
    }

    pub fn game(&self, id: &str, key: &str) -> String {
        value(&self.file, &format!("game {id}"), key)
    }

    pub fn game_ids(&self) -> Vec<String> {
        self.ids("game")
    }

    pub fn save_game(&mut self, id: &str, data: &[(&str, String)]) -> Result<()> {
        let group = format!("game {id}");
        self.edit(|file| {
            for (key, value) in data {
                file.set_string(&group, key, value);
            }
        })
    }

    pub fn delete(&mut self, kind: &str, id: &str) -> Result<()> {
        if kind == "preset" && id == DEFAULT_ID {
            return Ok(());
        }
        self.edit(|file| {
            let _ = file.remove_group(&format!("{kind} {id}"));
        })
    }

    fn ids(&self, kind: &str) -> Vec<String> {
        let prefix = format!("{kind} ");
        self.file
            .groups()
            .iter()
            .filter_map(|group| group.as_str().strip_prefix(&prefix).map(str::to_owned))
            .collect()
    }
}

fn value(file: &glib::KeyFile, group: &str, key: &str) -> String {
    file.string(group, key)
        .map(String::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_legacy_and_persists_named_choices() {
        let dir = std::env::temp_dir().join(glib::uuid_string_random().as_str());
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("settings.ini");
        std::fs::write(&path, "[scaling]\nlayout=stretch\n").unwrap();
        let mut settings = Settings::open(&path);
        assert_eq!(settings.preset(DEFAULT_ID).layout, Layout::Stretch);
        assert!(settings.preset(DEFAULT_ID).render_device.is_empty());
        assert_eq!(settings.theme(), Theme::System);
        let preset = Preset {
            name: "村正".into(),
            layout: Layout::Integer,
            filter: Filter::Anime4k,
            render_device: "/pci0000:00/0000:00:02.0".into(),
            ..Preset::default()
        };
        settings.save_preset("mine", &preset).unwrap();
        settings.select("mine").unwrap();
        settings.set_language(Language::Japanese).unwrap();
        settings.set_theme(Theme::Dark).unwrap();
        let mut settings = Settings::open(&path);
        assert_eq!(settings.preset(&settings.selected()), preset);
        assert_eq!(settings.language(), Language::Japanese);
        assert_eq!(settings.theme(), Theme::Dark);
        settings.delete("preset", "mine").unwrap();
        assert_eq!(settings.selected(), DEFAULT_ID);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persists_crop_app_rules_and_magnification_options() {
        let dir = std::env::temp_dir().join(glib::uuid_string_random().as_str());
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("settings.ini");
        let mut settings = Settings::open(&path);
        assert!(settings.preset(DEFAULT_ID).crop.is_empty());
        assert_eq!(settings.hide_cursor_seconds(), 0);
        assert!(!settings.inhibit_idle());
        let preset = Preset {
            name: "VN".into(),
            filter: Filter::Xbrz,
            crop: Crop::from_values([4, 30, 4, 9000]),
            sharpness: 65,
            post_sharpen: true,
            post_deband: true,
            ..Preset::default()
        };
        settings.save_preset("vn", &preset).unwrap();
        settings.set_app_rule("Muramasa_chs.exe", "vn").unwrap();
        settings.set_app_rule("org.kde.Dolphin.desktop", "missing").unwrap();
        settings.set_hide_cursor_seconds(3).unwrap();
        settings.set_inhibit_idle(true).unwrap();
        settings.set_screenshot_dir("/tmp/shots").unwrap();

        let mut settings = Settings::open(&path);
        let saved = settings.preset("vn");
        assert_eq!(saved.filter, Filter::Xbrz);
        assert_eq!(saved.crop.values(), [4, 30, 4, Crop::MAX]);
        assert_eq!(saved.sharpness, 65);
        assert!(saved.post_sharpen);
        assert!(saved.post_deband);
        assert!(saved.uses_render_worker());
        assert_eq!(settings.preset(DEFAULT_ID).sharpness, DEFAULT_SHARPNESS);
        assert_eq!(settings.preset_for_app("muramasa_chs.exe").as_deref(), Some("vn"));
        assert_eq!(settings.preset_for_app("MURAMASA_CHS.EXE.desktop").as_deref(), Some("vn"));
        assert_eq!(settings.preset_for_app("org.kde.dolphin"), None);
        assert_eq!(settings.app_rules().len(), 2);
        assert_eq!(settings.hide_cursor_seconds(), 3);
        assert!(settings.inhibit_idle());
        assert_eq!(settings.screenshot_dir(), "/tmp/shots");
        settings.remove_app_rule("muramasa_chs.exe").unwrap();
        settings.set_screenshot_dir("").unwrap();
        assert_eq!(settings.preset_for_app("muramasa_chs.exe"), None);
        assert!(settings.screenshot_dir().is_empty());
        assert!(settings.set_app_rule("bad[app]", "vn").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn crop_keeps_a_visible_area() {
        let crop = Crop::from_values([10, 20, 30, 40]);
        assert_eq!(crop.visible((800, 600), 1.0), Some((10, 20, 760, 540)));
        assert_eq!(crop.visible((800, 600), 1.5), Some((15, 30, 740, 510)));
        assert_eq!(Crop::from_values([400, 0, 390, 0]).visible((800, 600), 1.0), None);
        assert_eq!(Crop::default().visible((800, 600), 2.0), Some((0, 0, 800, 600)));
    }
}
