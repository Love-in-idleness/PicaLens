use crate::settings::Language;
use gtk::glib;

#[derive(Clone, Copy)]
pub struct Locale(Language);

thread_local! {
    static CATALOGS: [glib::KeyFile; 3] = [
        include_str!("../locale/en.ini"),
        include_str!("../locale/zh-CN.ini"),
        include_str!("../locale/ja.ini"),
    ].map(|data| {
        let catalog = glib::KeyFile::new();
        catalog.load_from_data(data, glib::KeyFileFlags::NONE).expect("embedded translation catalog");
        catalog
    });
}

impl Locale {
    pub fn new(preference: Language) -> Self {
        if preference != Language::System {
            return Self(preference);
        }
        Self(system_language(
            glib::language_names().iter().map(|name| name.as_str()),
        ))
    }

    pub fn text(self, key: &str) -> String {
        let index = match self.0 {
            Language::Chinese => 1,
            Language::Japanese => 2,
            _ => 0,
        };
        CATALOGS.with(|catalogs| {
            catalogs[index]
                .string("messages", key)
                .or_else(|_| catalogs[0].string("messages", key))
                .map(String::from)
                .unwrap_or_else(|_| key.into())
        })
    }
}

fn system_language<'a>(names: impl IntoIterator<Item = &'a str>) -> Language {
    for name in names {
        let name = name
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .replace('-', "_")
            .to_ascii_lowercase();
        match name.as_str() {
            "zh" | "zh_cn" | "zh_sg" | "zh_hans" | "zh_hans_cn" => return Language::Chinese,
            "ja" | "ja_jp" => return Language::Japanese,
            "en" | "c" | "posix" => return Language::English,
            n if n.starts_with("en_") => return Language::English,
            _ => {}
        }
    }
    Language::English
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translated_catalogs_match_and_support_system_fallback() {
        CATALOGS.with(|catalogs| {
            let english = catalogs[0].keys("messages").unwrap();
            for catalog in &catalogs[1..] {
                let keys = catalog.keys("messages").unwrap();
                assert_eq!(english.len(), keys.len());
                for key in &english {
                    assert!(!catalog.string("messages", key.as_str()).unwrap().is_empty());
                }
            }
        });
        assert_eq!(
            system_language(["fr_CA", "ja_JP.UTF-8"]),
            Language::Japanese
        );
        assert_eq!(system_language(["zh_CN.utf8"]), Language::Chinese);
    }
}
