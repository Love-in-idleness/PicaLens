use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use gtk::{gio, glib};

use crate::{
    render_worker::{FRAME_BYTES, WORKER, frame_len},
    session::Result,
    settings::Filter,
    upscale::{Pixels, ScaledFrame, Size},
};

pub struct Device {
    pub id: String,
    pub label: String,
}

impl Device {
    pub fn title(&self) -> String {
        format!(
            "{} · {}",
            self.id.rsplit('/').next().unwrap_or(&self.id),
            self.label,
        )
    }
}

fn worker_path() -> PathBuf {
    if let Ok(current) = std::env::current_exe()
        && let Some(dir) = current.parent()
    {
        let worker = dir.join("picalens-render");
        if worker.is_file() {
            return worker;
        }
    }
    WORKER.into()
}

pub async fn devices() -> Result<Vec<Device>> {
    let worker = worker_path();
    let command = gio::Subprocess::newv(
        &[worker.as_os_str(), "--list".as_ref()],
        gio::SubprocessFlags::STDOUT_PIPE,
    )?;
    let mut result = Box::pin(command.communicate_utf8_future(None));
    let (stdout, _) = futures_util::select_biased! {
        result = futures_util::FutureExt::fuse(&mut result) => result?,
        _ = futures_util::FutureExt::fuse(glib::timeout_future(Duration::from_secs(8))) => {
            command.force_exit();
            return Err("render_gpu_driver".into());
        }
    };
    if !command.is_successful() {
        return Err("render_gpu_driver".into());
    }
    Ok(stdout
        .as_deref()
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (id, label) = line.split_once('\t')?;
            Some(Device {
                id: id.to_owned(),
                label: label.to_owned(),
            })
        })
        .collect())
}

struct Shared {
    file: File,
    path: PathBuf,
}

impl Shared {
    fn new() -> Result<Self> {
        let path =
            glib::user_runtime_dir().join(format!("picalens-{}.rgba", glib::uuid_string_random()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let shared = Self { file, path };
        shared.file.set_len(2 * FRAME_BYTES as u64)?;
        Ok(shared)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct Worker {
    child: Option<Child>,
    shared: Shared,
    lines: Receiver<String>,
}

impl Worker {
    fn new(filter: Filter, device: &str, hint: Option<PathBuf>) -> Result<Self> {
        let shared = Shared::new()?;
        let mut child = Command::new(worker_path())
            .args(["--run", filter.id(), device])
            .arg(hint.map_or_else(OsString::new, PathBuf::into_os_string))
            .arg(&shared.path)
            .arg("native")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| {
                diagnostic!("picalens render process: {error}");
                "render_worker_failed"
            })?;
        let stdout = child.stdout.take().expect("worker stdout");
        let (sender, lines) = mpsc::channel();
        let worker = Self {
            child: Some(child),
            shared,
            lines,
        };
        thread::Builder::new()
            .name("picalens-render-replies".into())
            .spawn(move || {
                for line in BufReader::new(stdout)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            })?;
        Ok(worker)
    }

    fn submit(&mut self, source: &Pixels, out: Size) -> Result<()> {
        if frame_len(source.size)? != source.bytes.len() {
            return Err("render_size".into());
        }
        frame_len(out)?;
        let (w, h) = source.size;
        let (out_w, out_h) = out;
        self.shared.file.write_all_at(&source.bytes, 0)?;
        let stdin = self
            .child
            .as_mut()
            .and_then(|child| child.stdin.as_mut())
            .ok_or("render_worker_failed")?;
        writeln!(stdin, "{w} {h} {out_w} {out_h}").map_err(|_| "render_worker_failed")?;
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Reap outside the GTK thread even if the driver stalls during shutdown.
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

pub struct RenderUpscaler {
    worker: Worker,
    latest: Option<Pixels>,
    submitted: Option<(u64, Size)>,
    in_flight: Option<(Size, Size)>,
    generation: u64,
    ready: bool,
    deadline: Instant,
}

impl RenderUpscaler {
    pub fn new(filter: Filter, selection: &str, hint: Option<PathBuf>) -> Result<Self> {
        Ok(Self {
            worker: Worker::new(filter, selection, hint)?,
            latest: None,
            submitted: None,
            in_flight: None,
            generation: 0,
            ready: false,
            deadline: Instant::now() + Duration::from_secs(10),
        })
    }

    pub fn set_source(&mut self, source: Pixels) {
        self.latest = Some(source);
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn update(&mut self, target: Option<Size>) -> Result<Option<ScaledFrame>> {
        let result = match self.worker.lines.try_recv() {
            Ok(line) if line == "ready" && !self.ready => {
                self.ready = true;
                None
            }
            Ok(line) if line == "frame" => {
                let (source, out) = self.in_flight.take().ok_or("render_worker_failed")?;
                if target == Some(out) {
                    let mut bytes = vec![0; frame_len(out)?];
                    self.worker
                        .shared
                        .file
                        .read_exact_at(&mut bytes, FRAME_BYTES as u64)?;
                    Some(ScaledFrame {
                        source,
                        image: Pixels {
                            size: out,
                            bytes: glib::Bytes::from_owned(bytes),
                        },
                    })
                } else {
                    None
                }
            }
            Ok(line) => return Err(worker_error(&line).into()),
            Err(TryRecvError::Empty) => {
                if (!self.ready || self.in_flight.is_some()) && Instant::now() >= self.deadline {
                    return Err("render_worker_failed".into());
                }
                None
            }
            Err(TryRecvError::Disconnected) => return Err("render_worker_failed".into()),
        };
        if self.ready
            && self.in_flight.is_none()
            && let (Some(source), Some(target)) = (&self.latest, target)
            && self.submitted != Some((self.generation, target))
        {
            self.worker.submit(source, target)?;
            self.submitted = Some((self.generation, target));
            self.in_flight = Some((source.size, target));
            self.deadline = Instant::now() + Duration::from_secs(10);
        }
        Ok(result)
    }
}

fn worker_error(line: &str) -> &'static str {
    match line {
        "error render_gpu_missing" => "render_gpu_missing",
        "error render_gpu_driver" => "render_gpu_driver",
        "error render_size" => "render_size",
        _ => "render_worker_failed",
    }
}
