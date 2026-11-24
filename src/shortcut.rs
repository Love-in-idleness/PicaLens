use std::{path::Path, time::Duration};

use ashpd::zbus::{self, Proxy};
use gtk::{gdk, gio, glib, prelude::*};

use crate::session::Result;

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM_KEY: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const CUSTOM_PATHS: &str = "custom-keybindings";
const PICALENS_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/picalens/";
const PLASMA_ACTION: [&str; 4] = [
    "io.github.LoveInIdleness.PicaLens.desktop",
    "Toggle",
    "PicaLens",
    "Toggle Magnification",
];

pub type Chord = (gdk::Key, gdk::ModifierType);

pub fn toggle_command() -> Result<String> {
    let executable = glib::shell_quote(std::env::current_exe()?);
    Ok(format!("{} toggle", executable.to_string_lossy()))
}

#[derive(Clone)]
pub enum DesktopShortcut {
    Gnome(gio::Settings, gio::SettingsSchema),
    Plasma,
}

impl DesktopShortcut {
    pub fn detect() -> Option<Self> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        for name in desktop.split(':') {
            match name.to_ascii_lowercase().as_str() {
                "gnome" => {
                    let source = gio::SettingsSchemaSource::default()?;
                    let media = source.lookup(MEDIA_KEYS, true)?;
                    let custom = source.lookup(CUSTOM_KEY, true)?;
                    return Some(Self::Gnome(
                        gio::Settings::new_full(&media, None::<&gio::SettingsBackend>, None),
                        custom,
                    ));
                }
                "kde" | "plasma" => return Some(Self::Plasma),
                _ => {}
            }
        }
        None
    }

    pub async fn label(&self) -> Result<Option<String>> {
        match self {
            Self::Gnome(parent, schema) => {
                let path = gnome_path(parent, schema);
                let binding = gnome_custom(schema, &path).string("binding");
                Ok(if binding.is_empty() {
                    None
                } else if let Some((key, modifiers)) = gtk::accelerator_parse(binding.as_str()) {
                    Some(gtk::accelerator_get_label(key, modifiers).into())
                } else {
                    Some(binding.into())
                })
            }
            Self::Plasma => {
                let proxy = plasma_proxy().await?;
                let keys = plasma_keys(&proxy).await?;
                let labels = keys
                    .iter()
                    .map(|(codes,)| {
                        codes
                            .iter()
                            .copied()
                            .filter(|&code| code != 0)
                            .map(qt_label)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|label| !label.is_empty())
                    .collect::<Vec<_>>();
                Ok(if labels.is_empty() {
                    None
                } else {
                    Some(labels.join(" / "))
                })
            }
        }
    }

    pub async fn save(&self, chord: Option<Chord>) -> Result<()> {
        match self {
            Self::Gnome(parent, schema) => save_gnome(parent, schema, chord),
            Self::Plasma => save_plasma(chord).await,
        }
    }
}

fn gnome_custom(schema: &gio::SettingsSchema, path: &str) -> gio::Settings {
    gio::Settings::new_full(schema, None::<&gio::SettingsBackend>, Some(path))
}

fn gnome_path(parent: &gio::Settings, schema: &gio::SettingsSchema) -> String {
    let paths = parent.strv(CUSTOM_PATHS);
    if paths.iter().any(|path| path == PICALENS_PATH) {
        return PICALENS_PATH.into();
    }
    paths.iter().find(|path| {
        let command = gnome_custom(schema, path).string("command");
        glib::shell_parse_argv(command.as_str()).is_ok_and(|args| {
            matches!(args.as_slice(), [program, action]
                if Path::new(program).file_name().is_some_and(|name| name == "picalens") && action == "toggle")
        })
    }).map_or_else(|| PICALENS_PATH.into(), ToString::to_string)
}

fn save_gnome(
    parent: &gio::Settings,
    schema: &gio::SettingsSchema,
    chord: Option<Chord>,
) -> Result<()> {
    let path = gnome_path(parent, schema);
    let mut paths = parent
        .strv(CUSTOM_PATHS)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let is_new = !paths.contains(&path);
    let shortcut = gnome_custom(schema, &path);
    if let Some(chord) = chord
        && gnome_conflict(schema, &paths, &path, chord)
    {
        return Err("That shortcut is already assigned in GNOME.".into());
    }
    if is_new && !parent.is_writable(CUSTOM_PATHS) {
        return Err("GNOME's custom shortcuts are locked.".into());
    }
    let binding = chord.map(|(key, modifiers)| gtk::accelerator_name(key, modifiers));
    shortcut.delay();
    if shortcut.string("name").is_empty() {
        shortcut.set_string("name", "PicaLens · Toggle magnification")?;
    }
    shortcut.set_string("command", &toggle_command()?)?;
    shortcut.set_string("binding", binding.as_deref().unwrap_or(""))?;
    shortcut.apply();
    if is_new {
        paths.push(path);
        parent.set_strv(CUSTOM_PATHS, paths)?;
    }
    gio::Settings::sync();
    Ok(())
}

fn gnome_conflict(
    schema: &gio::SettingsSchema,
    paths: &[String],
    own_path: &str,
    chord: Chord,
) -> bool {
    let matches = |binding: &str| {
        gtk::accelerator_parse(binding).is_some_and(|(key, mods)| (key.to_lower(), mods) == chord)
    };
    if paths
        .iter()
        .filter(|path| path.as_str() != own_path)
        .any(|path| matches(gnome_custom(schema, path).string("binding").as_str()))
    {
        return true;
    }
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return false;
    };
    [
        MEDIA_KEYS,
        "org.gnome.desktop.wm.keybindings",
        "org.gnome.mutter.keybindings",
        "org.gnome.mutter.wayland.keybindings",
        "org.gnome.shell.keybindings",
    ]
    .into_iter()
    .filter_map(|id| source.lookup(id, true))
    .any(|schema| {
        let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
        schema.list_keys().iter().any(|key| {
            let value = settings.value(key);
            value.str().is_some_and(matches)
                || value
                    .get::<Vec<String>>()
                    .is_some_and(|bindings| bindings.iter().any(|binding| matches(binding)))
        })
    })
}

type QtSequence = (Vec<i32>,);

async fn plasma_proxy() -> Result<Proxy<'static>> {
    let bus = zbus::connection::Builder::session()?
        .method_timeout(Duration::from_secs(3))
        .build()
        .await?;
    let proxy = Proxy::new(
        &bus,
        "org.kde.kglobalaccel",
        "/kglobalaccel",
        "org.kde.KGlobalAccel",
    )
    .await?;
    let component: zbus::zvariant::OwnedObjectPath =
        proxy.call("getComponent", &(PLASMA_ACTION[0],)).await?;
    let actions = Proxy::new(
        &bus,
        "org.kde.kglobalaccel",
        component,
        "org.kde.kglobalaccel.Component",
    )
    .await?;
    let names: Vec<String> = actions.call("shortcutNames", &("default",)).await?;
    if !names.iter().any(|name| name == PLASMA_ACTION[1]) {
        return Err("Plasma has no PicaLens desktop shortcut entry.".into());
    }
    Ok(proxy)
}

async fn plasma_keys(proxy: &Proxy<'_>) -> Result<Vec<QtSequence>> {
    Ok(proxy.call("shortcutKeys", &(&PLASMA_ACTION[..],)).await?)
}

async fn save_plasma(chord: Option<Chord>) -> Result<()> {
    let proxy = plasma_proxy().await?;
    let old = plasma_keys(&proxy).await?;
    let mut keys = Vec::new();
    if let Some(chord) = chord {
        let sequence = (vec![
            qt_code(chord).ok_or("Plasma cannot use this key in a shortcut.")?,
            0,
            0,
            0,
        ],);
        if !old.contains(&sequence) {
            let available: bool = proxy
                .call("globalShortcutAvailable", &(&sequence, ""))
                .await?;
            if !available {
                return Err("That shortcut is already assigned in Plasma.".into());
            }
        }
        keys.push(sequence);
    }
    // NoAutoloading replaces the saved keys of the existing desktop action.
    let saved: Vec<QtSequence> = proxy
        .call("setShortcutKeys", &(&PLASMA_ACTION[..], &keys, 4u32))
        .await?;
    if saved != keys {
        proxy
            .call::<_, _, Vec<QtSequence>>("setShortcutKeys", &(&PLASMA_ACTION[..], &old, 4u32))
            .await?;
        return Err("Plasma could not register that shortcut.".into());
    }
    Ok(())
}

const QT_MODIFIERS: [(gdk::ModifierType, i32); 4] = [
    (gdk::ModifierType::SHIFT_MASK, 0x02000000),
    (gdk::ModifierType::CONTROL_MASK, 0x04000000),
    (gdk::ModifierType::ALT_MASK, 0x08000000),
    (gdk::ModifierType::SUPER_MASK, 0x10000000),
];

const QT_KEYS: [(gdk::Key, i32); 15] = [
    (gdk::Key::Escape, 0x01000000),
    (gdk::Key::Tab, 0x01000001),
    (gdk::Key::BackSpace, 0x01000003),
    (gdk::Key::Return, 0x01000004),
    (gdk::Key::Insert, 0x01000006),
    (gdk::Key::Delete, 0x01000007),
    (gdk::Key::Print, 0x01000009),
    (gdk::Key::Home, 0x01000010),
    (gdk::Key::End, 0x01000011),
    (gdk::Key::Left, 0x01000012),
    (gdk::Key::Up, 0x01000013),
    (gdk::Key::Right, 0x01000014),
    (gdk::Key::Down, 0x01000015),
    (gdk::Key::Page_Up, 0x01000016),
    (gdk::Key::Page_Down, 0x01000017),
];

fn qt_code((key, mut modifiers): Chord) -> Option<i32> {
    use glib::translate::IntoGlib;
    if modifiers.contains(gdk::ModifierType::META_MASK) {
        modifiers |= gdk::ModifierType::SUPER_MASK;
    }
    let key_code = if (gdk::Key::F1..=gdk::Key::F35).contains(&key) {
        0x01000030 + (key.into_glib() - gdk::Key::F1.into_glib()) as i32
    } else if let Some((_, code)) = QT_KEYS.iter().find(|&&(candidate, _)| candidate == key) {
        *code
    } else {
        i32::try_from(u32::from(key.to_upper().to_unicode()?)).ok()?
    };
    Some(QT_MODIFIERS.iter().fold(key_code, |code, &(flag, mask)| {
        if modifiers.contains(flag) {
            code | mask
        } else {
            code
        }
    }))
}

fn qt_label(code: i32) -> String {
    use glib::translate::{IntoGlib, from_glib};
    let mut modifiers = gdk::ModifierType::empty();
    let mut key_code = code;
    for (flag, mask) in QT_MODIFIERS {
        if code & mask != 0 {
            modifiers |= flag;
            key_code &= !mask;
        }
    }
    let key = if (0x01000030..=0x01000052).contains(&key_code) {
        unsafe { from_glib(gdk::Key::F1.into_glib() + (key_code - 0x01000030) as u32) }
    } else if let Some(&(key, _)) = QT_KEYS
        .iter()
        .find(|&&(_, candidate)| candidate == key_code)
    {
        key
    } else if let Some(character) = char::from_u32(key_code as u32) {
        unsafe { from_glib(gdk::unicode_to_keyval(u32::from(character))) }
    } else {
        return "Custom shortcut".into();
    };
    gtk::accelerator_get_label(key.to_lower(), modifiers).into()
}
