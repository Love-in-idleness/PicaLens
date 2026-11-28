use std::{
    ffi::OsString,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use crate::session::Result;
use gtk::glib;

pub const SCALERS: &[&str] = &["auto", "fit", "fill", "stretch", "integer"];
pub const FILTERS: &[&str] = &["linear", "nearest", "fsr", "nis", "pixel", "sgsr"];
pub const MODES: &[&str] = &["windowed", "borderless", "fullscreen"];
pub const KINDS: &[&str] = &["native", "wine", "steam"];
pub const ORIENTATIONS: &[&str] = &["auto", "normal", "left", "right", "upsidedown"];
pub const TEXT: &[&str] = &[
    "name",
    "program",
    "arguments",
    "directory",
    "runner",
    "prefix",
    "env_vars",
    "extra_args",
];
pub const OPTION_TEXT: &[(&str, &str)] = &[
    ("vk_device", "--prefer-vk-device"),
    ("reshade_effect", "--reshade-effect"),
];
pub const NUMERIC: &[(&str, &str)] = &[
    ("input_width", "--nested-width"),
    ("input_height", "--nested-height"),
    ("output_width", "--output-width"),
    ("output_height", "--output-height"),
    ("game_refresh", "--nested-refresh"),
    ("background_refresh", "--nested-unfocused-refresh"),
    ("fps_limit", "--framerate-limit"),
    ("sharpness", "--sharpness"),
    ("display_index", "--display-index"),
    ("hide_cursor_delay", "--hide-cursor-delay"),
    ("hdr_sdr_nits", "--hdr-sdr-content-nits"),
    ("reshade_technique", "--reshade-technique-idx"),
];
pub const FLOATS: &[(&str, &str)] = &[
    ("max_scale", "--max-scale"),
    ("mouse_sensitivity", "--mouse-sensitivity"),
];
pub const SWITCHES: &[(&str, &str)] = &[
    ("grab", "--grab"),
    ("force_grab_cursor", "--force-grab-cursor"),
    ("force_windows_fullscreen", "--force-windows-fullscreen"),
    ("expose_wayland", "--expose-wayland"),
    ("performance_overlay", "--mangoapp"),
    ("hdr", "--hdr-enabled"),
    ("hdr_itm", "--hdr-itm-enabled"),
    ("vrr", "--adaptive-sync"),
    ("rt", "--rt"),
    ("immediate_flips", "--immediate-flips"),
    ("steam_integration", "--steam"),
];

pub struct Gamescope {
    path: PathBuf,
    help: String,
    filters: Vec<String>,
    scalers: Vec<String>,
    mangoapp: bool,
}

impl Gamescope {
    pub fn detect() -> Result<Option<Self>> {
        let Some(path) = glib::find_program_in_path("gamescope") else {
            return Ok(None);
        };
        let output = Command::new(&path)
            .arg("--help")
            .env("LC_ALL", "C")
            .output()?;
        let mut help = String::from_utf8_lossy(&output.stdout).into_owned();
        help.push_str(&String::from_utf8_lossy(&output.stderr));
        if !help.contains("--scaler") || !help.contains("--filter") {
            return Err(help.into());
        }
        Ok(Some(Self::from_help(
            path,
            help,
            glib::find_program_in_path("mangoapp").is_some(),
        )))
    }

    fn from_help(path: PathBuf, help: String, mangoapp: bool) -> Self {
        let read = |flag| {
            help.lines()
                .find(|line| line.split_ascii_whitespace().any(|word| word == flag))
                .and_then(|line| line.rsplit_once('('))
                .and_then(|(_, list)| list.strip_suffix(')'))
                .map(|list| list.split(',').map(|part| part.trim().to_owned()).collect())
                .unwrap_or_default()
        };
        Self {
            path,
            filters: read("--filter"),
            scalers: read("--scaler"),
            help,
            mangoapp,
        }
    }

    pub fn has(&self, flag: &str) -> bool {
        if flag == "--mangoapp" && !self.mangoapp {
            return false;
        }
        self.help.lines().any(|line| {
            line.trim_start().starts_with('-')
                && line.split_ascii_whitespace().any(|token| {
                    token.trim_end_matches(',') == flag
                })
        })
    }

    pub fn filter(&self, id: &str) -> bool {
        self.filters.iter().any(|item| item == id)
    }

    pub fn scaler(&self, id: &str) -> bool {
        self.scalers.iter().any(|item| item == id)
    }

    pub fn hotkeys(&self) -> &str {
        self.help
            .split_once("Keyboard shortcuts:")
            .map(|(_, keys)| keys.trim())
            .unwrap_or_default()
    }

    pub fn options<'a>(
        &self,
        get: impl Fn(&str) -> &'a str,
    ) -> std::result::Result<Vec<OsString>, &'static str> {
        let mut args = Vec::new();
        let mut push = |option: &str| {
            if self.has(option) {
                args.push(option.into());
                Ok(())
            } else {
                Err("unsupported_option")
            }
        };
        match get("mode") {
            "fullscreen" => push("--fullscreen")?,
            "borderless" => push("--borderless")?,
            _ => {}
        }
        let scaler = get("scaler");
        if !scaler.is_empty() {
            if !self.scaler(scaler) {
                return Err("unsupported_option");
            }
            args.extend(["--scaler".into(), scaler.into()]);
        }
        let filter = get("filter");
        if !filter.is_empty() {
            if !self.filter(filter) {
                return Err("unsupported_option");
            }
            args.extend(["--filter".into(), filter.into()]);
        }
        let orientation = get("orientation");
        if !orientation.is_empty() && orientation != "auto" {
            if !self.has("--force-orientation") {
                return Err("unsupported_option");
            }
            args.extend(["--force-orientation".into(), orientation.into()]);
        }
        for (width, height) in [
            ("input_width", "input_height"),
            ("output_width", "output_height"),
        ] {
            if get(width).trim().is_empty() != get(height).trim().is_empty() {
                return Err("paired_resolution");
            }
        }
        let has_reshade = !get("reshade_effect").trim().is_empty();
        for &(id, flag) in NUMERIC {
            let value = get(id).trim();
            if value.is_empty()
                || (id == "sharpness" && !matches!(filter, "fsr" | "nis" | "sgsr"))
                || (id == "reshade_technique" && !has_reshade)
            {
                continue;
            }
            let number: i32 = value.parse().map_err(|_| "invalid_number")?;
            let valid = match id {
                "sharpness" => (0..=20).contains(&number),
                "display_index" | "reshade_technique" => number >= 0,
                _ => number > 0,
            };
            if !valid {
                return Err("invalid_number");
            }
            if !self.has(flag) {
                return Err("unsupported_option");
            }
            args.extend([flag.into(), number.to_string().into()]);
        }
        for &(id, flag) in FLOATS {
            let value = get(id).trim();
            if value.is_empty() {
                continue;
            }
            let number: f32 = value.parse().map_err(|_| "invalid_number")?;
            if !number.is_finite() || number <= 0.0 {
                return Err("invalid_number");
            }
            if !self.has(flag) {
                return Err("unsupported_option");
            }
            args.extend([flag.into(), value.into()]);
        }
        for &(id, flag) in OPTION_TEXT {
            let value = get(id).trim();
            if value.is_empty() {
                continue;
            }
            if id == "vk_device" && !is_valid_vk_device(value) {
                return Err("invalid_vk_device");
            }
            if !self.has(flag) {
                return Err("unsupported_option");
            }
            args.extend([flag.into(), value.into()]);
        }
        for &(id, flag) in SWITCHES {
            if get(id) == "true" {
                if !self.has(flag) {
                    return Err("unsupported_option");
                }
                args.push(flag.into());
            }
        }
        let extra = get("extra_args").trim();
        if !extra.is_empty() {
            let parsed = glib::shell_parse_argv(extra).map_err(|_| "invalid_extra_args")?;
            args.extend(parsed);
        }
        Ok(args)
    }

    pub fn env_vars(raw: &str) -> std::result::Result<Vec<(String, String)>, &'static str> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let tokens = glib::shell_parse_argv(trimmed).map_err(|_| "invalid_env_vars")?;
        let mut pairs = Vec::with_capacity(tokens.len());
        for token in tokens {
            let text = token.to_str().ok_or("invalid_env_vars")?;
            let (key, value) = text.split_once('=').ok_or("invalid_env_vars")?;
            if key.is_empty()
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                || key.starts_with(|ch: char| ch.is_ascii_digit())
            {
                return Err("invalid_env_vars");
            }
            pairs.push((key.to_owned(), value.to_owned()));
        }
        Ok(pairs)
    }

    pub fn steam_options(&self, options: &[OsString], env: &[(String, String)]) -> String {
        let mut parts = Vec::with_capacity(env.len() + options.len() + 3);
        for (key, value) in env {
            parts.push(format!(
                "{key}={}",
                glib::shell_quote(value).to_string_lossy()
            ));
        }
        parts.push("gamescope".into());
        for arg in options {
            parts.push(glib::shell_quote(arg).to_string_lossy().into_owned());
        }
        parts.push("--".into());
        parts.push("%command%".into());
        parts.join(" ")
    }

    pub fn spawn<'a>(
        &self,
        options: &[OsString],
        env: &[(String, String)],
        get: impl Fn(&str) -> &'a str,
    ) -> Result<Child> {
        let program = get("program").trim();
        let wine = get("kind") == "wine";
        if program.is_empty() {
            return Err("missing_program".into());
        }
        let program = if wine {
            std::path::absolute(program)?
        } else {
            PathBuf::from(program)
        };
        let arguments = get("arguments");
        let arguments = if arguments.trim().is_empty() {
            Vec::new()
        } else {
            glib::shell_parse_argv(arguments)?
        };
        let mut command = Command::new(&self.path);
        for (key, value) in env {
            command.env(key, value);
        }
        command.args(options).arg("--");
        if wine {
            let runner = get("runner").trim();
            command.arg(if runner.is_empty() { "wine" } else { runner });
            let prefix = get("prefix").trim();
            if !prefix.is_empty() {
                command.env("WINEPREFIX", prefix);
            }
        }
        command
            .arg(&program)
            .args(arguments)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let cwd = get("directory").trim();
        if !cwd.is_empty() {
            command.current_dir(cwd);
        } else if wine {
            if let Some(parent) = Path::new(&program)
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
            {
                command.current_dir(parent);
            }
        }
        Ok(command.spawn()?)
    }
}

fn is_valid_vk_device(value: &str) -> bool {
    let Some((vendor, device)) = value.split_once(':') else {
        return false;
    };
    let hex = |part: &str| {
        (1..=4).contains(&part.len()) && part.chars().all(|ch| ch.is_ascii_hexdigit())
    };
    hex(vendor) && hex(device)
}

/// Scans `/sys/class/drm/renderD*/device/{vendor,device}` for PCI IDs
/// formatted as `vendor:device` suitable for `--prefer-vk-device`.
pub fn detected_vk_devices() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("renderD") {
            continue;
        }
        let dev = entry.path().join("device");
        let (Ok(vendor), Ok(device)) = (
            std::fs::read_to_string(dev.join("vendor")),
            std::fs::read_to_string(dev.join("device")),
        ) else {
            continue;
        };
        let clean = |raw: &str| raw.trim().strip_prefix("0x").unwrap_or(raw.trim()).to_ascii_lowercase();
        let id = format!("{}:{}", clean(&vendor), clean(&device));
        if is_valid_vk_device(&id) && !devices.contains(&id) {
            devices.push(id);
        }
    }
    devices.sort();
    devices
}

pub fn wait(mut child: Child) -> (io::Result<std::process::ExitStatus>, String) {
    const MAX_LOG: usize = 8192;
    let tail = Arc::new(Mutex::new(Vec::new()));
    let (finished, done) = std::sync::mpsc::channel();
    if let Some(mut stderr) = child.stderr.take() {
        let tail = tail.clone();
        std::thread::spawn(move || {
            let mut chunk = [0; 4096];
            while let Ok(n) = stderr.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut tail = tail.lock().unwrap();
                let overflow = (tail.len() + n).saturating_sub(MAX_LOG);
                tail.drain(..overflow);
                tail.extend_from_slice(&chunk[..n]);
            }
            let _ = finished.send(());
        });
    }
    let status = child.wait();
    let _ = done.recv_timeout(std::time::Duration::from_millis(250));
    let tail = String::from_utf8_lossy(&tail.lock().unwrap()).into_owned();
    (status, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn builds_versioned_flags_and_keeps_steam_placeholder() {
        let backend = Gamescope::from_help(
            "gamescope".into(),
            "\
          -S, --scaler  modes (auto, fit, integer)\n\
          -F, --filter  filters (linear, nearest, fsr)\n\
          -w, --nested-width WIDTH\n\
          -h, --nested-height HEIGHT\n\
          --framerate-limit FPS\n\
          -m, --max-scale FACTOR\n\
          -s, --mouse-sensitivity SENS\n\
          --display-index IDX\n\
          -C, --hide-cursor-delay MS\n\
          --prefer-vk-device DEV\n\
          --force-grab-cursor\n\
          --force-windows-fullscreen\n\
          --force-orientation ORIENT\n\
          --reshade-effect NAME\n\
          --reshade-technique-idx IDX\n\
          --hdr-sdr-content-nits NITS\n\
          --hdr-itm-enabled\n\
          --rt\n\
          --immediate-flips\n\
          --steam\n"
                .into(),
            false,
        );
        let mut input = HashMap::from([
            ("scaler", "integer"),
            ("filter", "nearest"),
            ("orientation", "right"),
            ("input_width", "800"),
            ("input_height", "600"),
            ("fps_limit", "30"),
            ("display_index", "0"),
            ("hide_cursor_delay", "2000"),
            ("hdr_sdr_nits", "400"),
            ("max_scale", "2.5"),
            ("mouse_sensitivity", "1.25"),
            ("vk_device", "1002:7300"),
            ("reshade_effect", "CRT.fx"),
            ("reshade_technique", "0"),
            ("force_grab_cursor", "true"),
            ("force_windows_fullscreen", "true"),
            ("hdr_itm", "true"),
            ("rt", "true"),
            ("immediate_flips", "true"),
            ("steam_integration", "true"),
            ("extra_args", "--keep-alive --fade-out-duration 500"),
        ]);
        let args = backend
            .options(|key| input.get(key).copied().unwrap_or(""))
            .unwrap();
        assert_eq!(
            args,
            [
                "--scaler",
                "integer",
                "--filter",
                "nearest",
                "--force-orientation",
                "right",
                "--nested-width",
                "800",
                "--nested-height",
                "600",
                "--framerate-limit",
                "30",
                "--display-index",
                "0",
                "--hide-cursor-delay",
                "2000",
                "--hdr-sdr-content-nits",
                "400",
                "--reshade-technique-idx",
                "0",
                "--max-scale",
                "2.5",
                "--mouse-sensitivity",
                "1.25",
                "--prefer-vk-device",
                "1002:7300",
                "--reshade-effect",
                "CRT.fx",
                "--force-grab-cursor",
                "--force-windows-fullscreen",
                "--hdr-itm-enabled",
                "--rt",
                "--immediate-flips",
                "--steam",
                "--keep-alive",
                "--fade-out-duration",
                "500",
            ]
            .map(OsString::from)
        );
        let env = Gamescope::env_vars("DXVK_HUD=fps PROTON_LOG='1 2'").unwrap();
        assert_eq!(
            env,
            vec![
                ("DXVK_HUD".into(), "fps".into()),
                ("PROTON_LOG".into(), "1 2".into())
            ]
        );
        let steam = backend.steam_options(&args, &env);
        assert!(steam.starts_with("DXVK_HUD='fps' PROTON_LOG='1 2' gamescope "));
        assert!(steam.ends_with(" -- %command%"));

        input.insert("vk_device", "not-a-pci-id");
        assert_eq!(
            backend.options(|key| input.get(key).copied().unwrap_or("")),
            Err("invalid_vk_device")
        );
        input.insert("vk_device", "1002:7300");
        input.insert("max_scale", "-1");
        assert_eq!(
            backend.options(|key| input.get(key).copied().unwrap_or("")),
            Err("invalid_number")
        );
        input.insert("max_scale", "");
        input.insert("extra_args", "--unterminated 'quote");
        assert_eq!(
            backend.options(|key| input.get(key).copied().unwrap_or("")),
            Err("invalid_extra_args")
        );
        input.insert("extra_args", "");
        input.insert("filter", "sgsr");
        assert_eq!(
            backend.options(|key| input.get(key).copied().unwrap_or("")),
            Err("unsupported_option")
        );
        assert_eq!(Gamescope::env_vars("NO_EQUALS"), Err("invalid_env_vars"));
        assert_eq!(Gamescope::env_vars("1BAD=1"), Err("invalid_env_vars"));
    }
}
