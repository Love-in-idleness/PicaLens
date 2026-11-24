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
choices!(Filter { Linear => "linear", Nearest => "nearest" });
choices!(Presentation { Fullscreen => "fullscreen", Windowed => "windowed", Preview => "preview" });
choices!(Language { System => "system", English => "en", Chinese => "zh-CN", Japanese => "ja" });

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preset {
    pub name: String,
    pub layout: Layout,
    pub filter: Filter,
    pub presentation: Presentation,
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
        }
    }

    pub fn save_preset(&mut self, id: &str, preset: &Preset) -> Result<()> {
        let group = format!("preset {id}");
        self.edit(|file| {
            file.set_string(&group, "name", &preset.name);
            file.set_string(&group, "layout", preset.layout.id());
            file.set_string(&group, "filter", preset.filter.id());
            file.set_string(&group, "presentation", preset.presentation.id());
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
        let preset = Preset {
            name: "村正".into(),
            layout: Layout::Integer,
            ..Preset::default()
        };
        settings.save_preset("mine", &preset).unwrap();
        settings.select("mine").unwrap();
        settings.set_language(Language::Japanese).unwrap();
        let mut settings = Settings::open(&path);
        assert_eq!(settings.preset(&settings.selected()), preset);
        assert_eq!(settings.language(), Language::Japanese);
        settings.delete("preset", "mine").unwrap();
        assert_eq!(settings.selected(), DEFAULT_ID);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
