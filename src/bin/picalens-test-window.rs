use std::{
    cell::Cell, env, fmt::Display, io, os::unix::net::UnixStream, path::PathBuf, process::Command,
    rc::Rc, time::Duration,
};

use adw::prelude::*;
use gtk::{gdk, glib};

const SCENES: &[&str] = &[
    "Rain taps against the station window.",
    "A magpie lands on the empty platform.",
    "The station clock strikes midnight.",
    "A light appears at the end of the tracks.",
];
const USAGE: &str = "Usage: picalens-test-window [--xwayland] [--sentinel]";

fn main() -> glib::ExitCode {
    let mut sentinel = false;
    let mut xwayland = false;
    for flag in env::args_os().skip(1) {
        match flag.to_str() {
            Some("--sentinel") if !sentinel => sentinel = true,
            Some("--xwayland") if !xwayland => xwayland = true,
            Some("--help" | "-h") => {
                println!("{USAGE}");
                return glib::ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("{USAGE}");
                return glib::ExitCode::FAILURE;
            }
        }
    }

    let (backend, fixture, suffix) = if xwayland {
        match check_xwayland() {
            Ok(true) => ("x11", "xwayland", " (XWayland)"),
            Ok(false) => ("x11", "x11-unverified", " (X11 fixture, unverified)"),
            Err(error) => {
                eprintln!("Cannot start the XWayland fixture: {error}");
                return glib::ExitCode::FAILURE;
            }
        }
    } else {
        ("wayland", "wayland", "")
    };

    gdk::set_allowed_backends(backend);
    if let Err(error) = adw::init() {
        if xwayland {
            eprintln!("A GTK X11 connection in a Wayland session is required: {error}");
        } else {
            eprintln!("A native Wayland session is required: {error}");
        }
        if let Some(configured) = env::var_os("GDK_BACKEND") {
            eprintln!("GDK_BACKEND={configured:?} may exclude {backend}. Unset it and retry.");
        }
        return glib::ExitCode::FAILURE;
    }

    let (app_id, title, role) = if sentinel {
        (
            "io.github.LoveInIdleness.PicaLens.InputSentinel",
            "PicaLens Input Sentinel",
            "sentinel",
        )
    } else {
        (
            "io.github.LoveInIdleness.PicaLens.TestWindow",
            "PicaLens Source Window",
            "source",
        )
    };
    let app_id = if xwayland {
        format!("{app_id}.X11")
    } else {
        app_id.to_owned()
    };
    let app = adw::Application::builder().application_id(app_id).build();
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(format!("{title}{suffix}"))
            .default_width(if sentinel { 440 } else { 800 })
            .default_height(if sentinel { 320 } else { 600 })
            .build();
        observe(&window, role, fixture);

        let header = adw::HeaderBar::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        if sentinel {
            let label = gtk::Label::new(Some("Pointer, keyboard and scroll events are logged."));
            label.set_wrap(true);
            label.set_margin_start(18);
            label.set_margin_end(18);
            toolbar.set_content(Some(&label));
        } else {
            let popup = gtk::Button::with_label("Open test popup");
            let parent = window.downgrade();
            popup.connect_clicked(move |_| {
                if let Some(parent) = parent.upgrade() {
                    open_popup(&parent, fixture);
                }
            });
            header.pack_end(&popup);
            toolbar.set_content(Some(&source_content()));
        }
        window.set_content(Some(&toolbar));
        window.present();
    });
    app.run_with_args(&["picalens-test-window"])
}

fn check_xwayland() -> Result<bool, String> {
    let mut socket =
        PathBuf::from(env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into()));
    if socket.is_relative() {
        let runtime = env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is unset")?;
        socket = PathBuf::from(runtime).join(socket);
    }
    UnixStream::connect(&socket).map_err(|error| {
        format!(
            "Wayland socket {} is unreachable: {error}",
            socket.display()
        )
    })?;

    match Command::new("xdpyinfo").arg("-queryExtensions").output() {
        Ok(output) if output.status.success() => {
            // XWayland 23.1+ advertises this extension to X11 clients.
            if String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.split_whitespace().next() == Some("XWAYLAND"))
            {
                Ok(true)
            } else {
                Err("DISPLAY does not advertise the XWAYLAND extension".into())
            }
        }
        Ok(output) => Err(format!(
            "xdpyinfo failed to query DISPLAY ({})",
            output.status
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland") {
                Ok(false)
            } else {
                Err("XDG_SESSION_TYPE is not wayland and xdpyinfo is unavailable".into())
            }
        }
        Err(error) => Err(format!("could not run xdpyinfo: {error}")),
    }
}

fn source_content() -> gtk::Box {
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 12);
    layout.set_margin_start(12);
    layout.set_margin_end(12);
    layout.set_margin_top(12);
    layout.set_margin_bottom(12);

    let grid = gtk::Grid::builder()
        .row_homogeneous(true)
        .column_homogeneous(true)
        .row_spacing(8)
        .column_spacing(8)
        .vexpand(true)
        .build();
    let scene = gtk::Label::new(Some(SCENES[0]));
    scene.add_css_class("title-4");
    scene.set_wrap(true);
    let step = Rc::new(Cell::new(0usize));
    for row in 0..3 {
        for column in 0..3 {
            let number = 1 + row * 3 + column;
            let button = gtk::Button::with_label(&number.to_string());
            button.set_hexpand(true);
            button.set_vexpand(true);
            button.add_css_class("title-1");
            let scene = scene.downgrade();
            let step = step.clone();
            button.connect_clicked(move |_| {
                let next = step.get() + 1;
                step.set(next);
                if let Some(scene) = scene.upgrade() {
                    scene.set_label(SCENES[next % SCENES.len()]);
                }
                record("source", "hit", format_args!("target={number} step={next}"));
            });
            grid.attach(&button, column, row, 1, 1);
        }
    }

    let counter = gtk::Label::new(Some("Tick 0"));
    counter.add_css_class("numeric");
    let swatch = gtk::DrawingArea::new();
    swatch.set_content_width(24);
    swatch.set_content_height(24);
    let tick = Rc::new(Cell::new(0u64));
    swatch.set_draw_func({
        let tick = tick.clone();
        move |_, cr, _, _| {
            let rgb = [(0.21, 0.48, 0.86), (0.97, 0.55, 0.10), (0.19, 0.68, 0.40)];
            let (red, green, blue) = rgb[tick.get() as usize % rgb.len()];
            cr.set_source_rgb(red, green, blue);
            let _ = cr.paint();
        }
    });
    let counter_ref = counter.downgrade();
    let swatch_ref = swatch.downgrade();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        let (Some(counter), Some(swatch)) = (counter_ref.upgrade(), swatch_ref.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        let next = tick.get() + 1;
        tick.set(next);
        counter.set_label(&format!("Tick {next}"));
        swatch.queue_draw();
        record("source", "tick", format_args!("count={next}"));
        glib::ControlFlow::Continue
    });
    let status = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    status.append(&swatch);
    status.append(&counter);
    scene.set_hexpand(true);
    status.append(&scene);

    layout.append(&grid);
    layout.append(&status);
    layout
}

fn open_popup(parent: &adw::ApplicationWindow, fixture: &'static str) {
    let popup = adw::ApplicationWindow::builder()
        .title("PicaLens Test Popup")
        .transient_for(parent)
        .destroy_with_parent(true)
        .modal(true)
        .default_width(320)
        .default_height(180)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let close = gtk::Button::with_label("Close popup");
    close.set_halign(gtk::Align::Center);
    close.set_valign(gtk::Align::Center);
    let popup_ref = popup.downgrade();
    close.connect_clicked(move |_| {
        if let Some(popup) = popup_ref.upgrade() {
            popup.close();
        }
    });
    toolbar.set_content(Some(&close));
    popup.set_content(Some(&toolbar));
    if let Some(app) = parent.application() {
        popup.set_application(Some(&app));
    }
    observe(&popup, "popup", fixture);
    popup.present();
}

fn observe(window: &adw::ApplicationWindow, role: &'static str, fixture: &'static str) {
    window.connect_realize(move |window| {
        if let Some(surface) = window.surface() {
            let previous = Cell::new(None);
            surface.connect_layout(move |surface, width, height| {
                let scale = surface.scale_factor();
                let layout = Some((width, height, scale));
                if previous.replace(layout) == layout {
                    return;
                }
                record(
                    role,
                    "layout",
                    format_args!("size={width}x{height} scale_factor={scale}"),
                );
            });
        }
    });
    window.connect_map(move |window| {
        if let Some(surface) = window.surface() {
            record(
                role,
                "mapped",
                format_args!(
                    "backend={} fixture={fixture}",
                    surface.display().type_().name()
                ),
            );
        }
    });
    window.connect_close_request(move |_| {
        record(role, "close", "");
        glib::Propagation::Proceed
    });
    window.connect_is_active_notify(move |window| {
        record(role, "focus", format_args!("active={}", window.is_active()));
    });

    let controller = gtk::EventControllerLegacy::builder()
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    controller.connect_event(move |_, event| {
        use gdk::EventType::*;
        match event.event_type() {
            ButtonPress | ButtonRelease => {
                if let Some(button) = event.downcast_ref::<gdk::ButtonEvent>() {
                    let kind = if event.event_type() == ButtonPress {
                        "button-press"
                    } else {
                        "button-release"
                    };
                    record(
                        role,
                        kind,
                        format_args!("button={} position={:?}", button.button(), event.position()),
                    );
                }
            }
            KeyPress | KeyRelease => {
                if let Some(key) = event.downcast_ref::<gdk::KeyEvent>() {
                    let kind = if event.event_type() == KeyPress {
                        "key-press"
                    } else {
                        "key-release"
                    };
                    record(
                        role,
                        kind,
                        format_args!("key={:?} keycode={}", key.keyval().name(), key.keycode()),
                    );
                }
            }
            Scroll => {
                if let Some(scroll) = event.downcast_ref::<gdk::ScrollEvent>() {
                    record(
                        role,
                        "scroll",
                        format_args!(
                            "direction={:?} deltas={:?} position={:?}",
                            scroll.direction(),
                            scroll.deltas(),
                            event.position()
                        ),
                    );
                }
            }
            _ => {}
        }
        glib::Propagation::Proceed
    });
    window.add_controller(controller);
}

fn record(role: &str, event: &str, details: impl Display) {
    println!(
        "{role} event={event} {details} monotonic_us={}",
        glib::monotonic_time()
    );
}
