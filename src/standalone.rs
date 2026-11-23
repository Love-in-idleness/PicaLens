use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use adw::prelude::*;
use futures_channel::oneshot;
use futures_util::FutureExt;
use gtk::{gdk, gio, glib};

use crate::{
    capture::Capture,
    kwin,
    preview::{self, StopSender},
    session::{self, Result, Stop},
};

const APP_ID: &str = "io.github.LoveInIdleness.PicaLens";
const USAGE: &str = "Usage: picalens [toggle]\nOpen PicaLens, or start and stop magnification with picalens toggle.";

pub fn run() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().collect();
    match args[1..] {
        [] => {}
        [ref arg] if arg == "toggle" => {}
        [ref arg] if arg == "-h" || arg == "--help" => {
            println!("{USAGE}");
            return std::process::ExitCode::SUCCESS;
        }
        _ => {
            diagnostic!("{USAGE}");
            return std::process::ExitCode::from(2);
        }
    }

    gdk::set_allowed_backends("wayland");
    if let Err(error) = gtk::init() {
        diagnostic!("PicaLens needs a Wayland session ({error})");
        return std::process::ExitCode::FAILURE;
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|_| {
        if let Some(display) = gdk::Display::default() {
            preview::install_style(&display);
        }
    });
    let view = RefCell::new(None::<Rc<View>>);
    app.connect_command_line(move |app, command| {
        let args = command.arguments();
        let view = view
            .borrow_mut()
            .get_or_insert_with(|| View::new(app))
            .clone();
        match args.len() {
            1 => {
                view.keep_open.set(true);
                if !view.stop("PicaLens opened") {
                    view.window.present();
                }
            }
            2 if args[1] == "toggle" => {
                if !view.stop("toggle") {
                    view.start(app);
                }
            }
            _ => return glib::ExitCode::from(2),
        }
        glib::ExitCode::SUCCESS
    });
    app.run_with_args(&args).into()
}

struct View {
    window: adw::ApplicationWindow,
    toolbar: adw::ToolbarView,
    controls: gtk::Box,
    picture: gtk::Picture,
    status: gtk::Label,
    action: gtk::Button,
    toasts: adw::ToastOverlay,
    view_only_notice: RefCell<Option<adw::Toast>>,
    keep_open: Cell<bool>,
    active: RefCell<Option<StopSender>>,
}

impl View {
    fn new(app: &adw::Application) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("PicaLens")
            .default_width(420)
            .default_height(260)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let controls = gtk::Box::new(gtk::Orientation::Vertical, 16);
        controls.set_halign(gtk::Align::Center);
        controls.set_valign(gtk::Align::Center);
        controls.set_margin_start(24);
        controls.set_margin_end(24);
        let status = gtk::Label::new(Some("Magnify an open window"));
        status.set_wrap(true);
        status.set_justify(gtk::Justification::Center);
        let action = gtk::Button::with_label("Choose Window…");
        action.add_css_class("suggested-action");
        action.set_halign(gtk::Align::Center);
        let hint = gtk::Label::new(Some("Shortcut command: picalens toggle"));
        hint.add_css_class("dim-label");
        controls.append(&status);
        controls.append(&action);
        controls.append(&hint);
        toolbar.set_content(Some(&controls));
        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .alternative_text("Magnified view of the selected window")
            .build();
        picture.add_css_class("capture-view");
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));
        window.set_content(Some(&toasts));
        let view = Rc::new(Self {
            window,
            toolbar,
            controls,
            picture,
            status,
            action,
            toasts,
            view_only_notice: RefCell::new(None),
            keep_open: Cell::new(false),
            active: RefCell::new(None),
        });

        view.action.connect_clicked({
            let view = Rc::downgrade(&view);
            let app = app.downgrade();
            move |_| {
                if let (Some(view), Some(app)) = (view.upgrade(), app.upgrade())
                    && !view.stop("Stop button")
                {
                    view.start(&app);
                }
            }
        });
        view.window.connect_close_request({
            let view = Rc::downgrade(&view);
            move |window| {
                if let Some(view) = view.upgrade() {
                    view.keep_open.set(false);
                    if !view.stop("window closed")
                        && let Some(app) = window.application()
                    {
                        app.quit();
                    }
                }
                window.set_visible(false);
                glib::Propagation::Stop
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let view = Rc::downgrade(&view);
            move |_, key, _, _| {
                if key == gdk::Key::Escape && view.upgrade().is_some_and(|view| view.stop("Escape"))
                {
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        });
        view.window.add_controller(keys);
        view
    }

    fn start(self: &Rc<Self>, app: &adw::Application) {
        let (sender, receiver) = oneshot::channel();
        let stop = Rc::new(RefCell::new(Some(sender)));
        *self.active.borrow_mut() = Some(stop.clone());
        self.status.set_label("Choose an open window");
        self.action.set_label("Cancel");
        let hold = app.hold();
        let view = self.clone();
        let signals = [libc::SIGINT, libc::SIGTERM].map(|signal| {
            let view = self.clone();
            glib::source::unix_signal_add_local(signal, move || {
                view.keep_open.set(false);
                view.stop("termination signal");
                glib::ControlFlow::Continue
            })
        });
        glib::spawn_future_local(async move {
            let _hold = hold;
            let result = view.magnify(receiver).await;
            for signal in signals {
                signal.remove();
            }
            view.active.borrow_mut().take();
            view.reset(result);
        });
    }

    fn stop(&self, reason: &'static str) -> bool {
        if let Some(stop) = self.active.borrow().as_ref() {
            preview::request_stop(stop, reason);
            return true;
        }
        false
    }

    async fn magnify(&self, mut stop: Stop) -> Result<String> {
        let backend = futures_util::select_biased! {
            reason = stop => return Ok(reason.unwrap_or("application shutdown").into()),
            backend = kwin::discover().fuse() => backend,
        }?;
        self.window.set_visible(false);
        if let Some(backend) = backend {
            let (reason, return_to_controls) = kwin::magnify(backend, stop).await?;
            if !return_to_controls {
                self.keep_open.set(false);
            }
            return Ok(reason);
        }

        diagnostic!("picalens backend=portal mode=view-only");
        session::capture_window(None, stop, |capture, _| self.show_capture(capture))
            .await
            .map(str::to_owned)
    }

    async fn show_capture(&self, capture: Capture) -> Result<&'static str> {
        self.status.set_label("Waiting for the window image…");
        let started_at = Instant::now();
        let mut started = false;
        loop {
            if let Some(error) = capture.take_error() {
                return Err(error.into());
            }
            if let Some(frame) = capture.take_frame() {
                self.picture
                    .set_paintable(Some(&preview::frame_texture(frame)?));
                if !started {
                    self.toolbar.set_content(Some(&self.picture));
                    self.toolbar.set_reveal_top_bars(false);
                    self.window.fullscreen();
                    self.window.present();
                    let notice = adw::Toast::new("View only · Esc to stop");
                    self.toasts.add_toast(notice.clone());
                    self.view_only_notice.replace(Some(notice));
                    diagnostic!("picalens portal first frame drawn");
                    started = true;
                }
            }
            if !started && started_at.elapsed() > Duration::from_secs(15) {
                return Err("No usable window image received within 15 seconds".into());
            }
            glib::timeout_future(Duration::from_millis(16)).await;
        }
    }

    fn reset(&self, result: Result<String>) {
        self.window.set_visible(false);
        self.window.unfullscreen();
        if let Some(notice) = self.view_only_notice.take() {
            notice.dismiss();
        }
        self.toolbar.set_content(Some(&self.controls));
        self.toolbar.set_reveal_top_bars(true);
        self.picture.set_paintable(gdk::Paintable::NONE);
        self.action.set_label("Choose Window…");
        match result {
            Ok(reason) => {
                diagnostic!("picalens stopped: {reason}");
                self.status.set_label("Magnify an open window");
            }
            Err(error) => {
                diagnostic!("picalens stopped: {error}");
                self.status.set_label(&error.to_string());
                self.keep_open.set(true);
            }
        }
        if self.keep_open.get() {
            self.window.present();
        } else if let Some(app) = self.window.application() {
            app.quit();
        }
    }
}
