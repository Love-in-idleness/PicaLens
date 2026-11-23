use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use crate::{preview, session};
use adw::prelude::*;
use futures_channel::oneshot;
use gtk::{gdk, gio, glib};

enum Command {
    Capture,
    Experiment,
    Toggle,
    Diagnostics,
    Help,
}

fn command(args: &[String]) -> Option<Command> {
    match args {
        [] => Some(Command::Capture),
        [value] if value == "--experiment" => Some(Command::Experiment),
        [value] if value == "toggle" => Some(Command::Toggle),
        [value] if value == "--desktop-only" => Some(Command::Diagnostics),
        [value] if value == "--help" || value == "-h" => Some(Command::Help),
        _ => None,
    }
}

pub fn run() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().collect();
    match command(&args[1..]) {
        Some(Command::Diagnostics) => {
            return match glib::MainContext::default().block_on(session::diagnostics()) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("desktop diagnostics: {error}");
                    std::process::ExitCode::FAILURE
                }
            };
        }
        Some(Command::Help) => {
            println!("Usage: picalens-probe [--desktop-only | --experiment | toggle]");
            println!("Without arguments, select a window for a view-only preview.");
            return std::process::ExitCode::SUCCESS;
        }
        None => {
            eprintln!("Usage: picalens-probe [--desktop-only | --experiment | toggle]");
            return std::process::ExitCode::from(2);
        }
        _ => {}
    }

    gdk::set_allowed_backends("wayland");
    if let Err(error) = gtk::init() {
        eprintln!("stopped: a native Wayland GTK display is required ({error})");
        return std::process::ExitCode::FAILURE;
    }
    let app = adw::Application::builder()
        .application_id("io.github.LoveInIdleness.PicaLens.Probe")
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|_| {
        if let Some(display) = gdk::Display::default() {
            preview::install_style(&display);
        }
    });
    let active = Rc::new(RefCell::new(None));
    let failed = Rc::new(Cell::new(false));
    app.connect_command_line({
        let failed = failed.clone();
        move |app, invocation| {
            let args: Vec<_> = invocation
                .arguments()
                .iter()
                .skip(1)
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let mode = command(&args);
            if !matches!(
                mode,
                Some(Command::Capture | Command::Experiment | Command::Toggle)
            ) {
                return glib::ExitCode::from(2);
            }
            if let Some(stop) = active.borrow().as_ref() {
                if matches!(mode, Some(Command::Toggle)) {
                    preview::request_stop(stop, "toggle");
                } else {
                    eprintln!("probe already active; use picalens-probe toggle to stop");
                }
                return glib::ExitCode::SUCCESS;
            }

            let (sender, receiver) = oneshot::channel();
            let stop = Rc::new(RefCell::new(Some(sender)));
            *active.borrow_mut() = Some(stop.clone());
            let experiment = matches!(mode, Some(Command::Experiment));
            let view = preview::Preview::new(app, stop.clone(), experiment);
            let hold = app.hold();
            let active = active.clone();
            let failed = failed.clone();
            let shutdown_signals = [libc::SIGINT, libc::SIGTERM].map(|signal| {
                let stop = stop.clone();
                glib::source::unix_signal_add_local(signal, move || {
                    preview::request_stop(&stop, "termination signal");
                    glib::ControlFlow::Continue
                })
            });
            let monitors = gtk::prelude::WidgetExt::display(&view.window).monitors();
            let changed = monitors.connect_items_changed(move |_, _, removed, _| {
                if removed > 0 {
                    preview::request_stop(&stop, "display output removed");
                }
            });
            view.window.present();

            glib::spawn_future_local(async move {
                let _hold = hold;
                match session::run(&view, experiment, receiver).await {
                    Ok(reason) => eprintln!("stopped: {reason}"),
                    Err(error) => {
                        eprintln!("stopped: {error}");
                        failed.set(true);
                    }
                }
                for signal in shutdown_signals {
                    signal.remove();
                }
                monitors.disconnect(changed);
                view.window.destroy();
                active.borrow_mut().take();
            });
            glib::ExitCode::SUCCESS
        }
    });
    let exit = app.run_with_args(&args);
    if failed.get() {
        std::process::ExitCode::FAILURE
    } else {
        exit.into()
    }
}
