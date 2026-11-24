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
pub const TEXT: &[&str] = &[
    "name",
    "program",
    "arguments",
    "directory",
    "runner",
    "prefix",
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
];
pub const SWITCHES: &[(&str, &str)] = &[
    ("grab", "--grab"),
    ("expose_wayland", "--expose-wayland"),
    ("performance_overlay", "--mangoapp"),
    ("hdr", "--hdr-enabled"),
    ("vrr", "--adaptive-sync"),
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
                && line.split_ascii_whitespace().any(|token| token == flag)
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
        for (width, height) in [
            ("input_width", "input_height"),
            ("output_width", "output_height"),
        ] {
            if get(width).trim().is_empty() != get(height).trim().is_empty() {
                return Err("paired_resolution");
            }
        }
        for &(id, flag) in NUMERIC {
            let value = get(id).trim();
            if value.is_empty() || (id == "sharpness" && !matches!(filter, "fsr" | "nis" | "sgsr"))
            {
                continue;
            }
            let number: i32 = value.parse().map_err(|_| "invalid_number")?;
            if (id == "sharpness" && !(0..=20).contains(&number))
                || (id != "sharpness" && number <= 0)
            {
                return Err("invalid_number");
            }
            if !self.has(flag) {
                return Err("unsupported_option");
            }
            args.extend([flag.into(), number.to_string().into()]);
        }
        for &(id, flag) in SWITCHES {
            if get(id) == "true" {
                if !self.has(flag) {
                    return Err("unsupported_option");
                }
                args.push(flag.into());
            }
        }
        Ok(args)
    }

    pub fn steam_options(&self, options: &[OsString]) -> String {
        let options = options
            .iter()
            .map(|arg| glib::shell_quote(arg).to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        format!("gamescope {options} -- %command%")
    }

    pub fn spawn<'a>(&self, options: &[OsString], get: impl Fn(&str) -> &'a str) -> Result<Child> {
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
          --framerate-limit FPS\n"
                .into(),
            false,
        );
        let mut input = HashMap::from([
            ("scaler", "integer"),
            ("filter", "nearest"),
            ("input_width", "800"),
            ("input_height", "600"),
            ("fps_limit", "30"),
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
                "--nested-width",
                "800",
                "--nested-height",
                "600",
                "--framerate-limit",
                "30"
            ]
            .map(OsString::from)
        );
        assert!(backend.steam_options(&args).ends_with(" -- %command%"));
        input.insert("filter", "sgsr");
        assert_eq!(
            backend.options(|key| input.get(key).copied().unwrap_or("")),
            Err("unsupported_option")
        );
    }
}
