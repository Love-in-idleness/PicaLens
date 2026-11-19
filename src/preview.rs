use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use adw::prelude::*;
use futures_channel::oneshot;
use gtk::{gdk, glib};

use crate::{
    capture::{Capture, Frame},
    input::{EisInput, InputEvent, normalize_point},
};

const ARM_TIME: Duration = Duration::from_secs(5);
const FRESH_FRAME: Duration = Duration::from_secs(3);
type StopSender = Rc<RefCell<Option<oneshot::Sender<&'static str>>>>;

pub fn install_style(display: &gdk::Display) {
    let css = gtk::CssProvider::new();
    css.load_from_data(".capture-view { background: black; }");
    gtk::style_context_add_provider_for_display(
        display,
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

pub struct Preview {
    pub window: adw::ApplicationWindow,
    picture: gtk::Picture,
    header: adw::HeaderBar,
    title: adw::WindowTitle,
    state: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    input: Option<EisInput>,
    frame: Option<Geometry>,
    logical_size: Option<(i32, i32)>,
    arm: Option<Arm>,
    pointer: Option<(f64, f64)>,
    observe_until: Option<Instant>,
}

#[derive(Clone, Copy)]
struct Geometry {
    picture: PictureGeometry,
    received: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PictureGeometry {
    size: (u32, u32),
    stream_size: (u32, u32),
    origin: (u32, u32),
}

struct Arm {
    expires: Instant,
    view_size: (i32, i32),
    geometry: PictureGeometry,
}

impl Preview {
    pub fn new(app: &adw::Application, stop: StopSender, experiment: bool) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("PicaLens Probe")
            .default_width(960)
            .default_height(720)
            .build();
        let picture = gtk::Picture::builder()
            .hexpand(true)
            .vexpand(true)
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Contain)
            .alternative_text("Live frame from the selected window")
            .build();
        picture.add_css_class("capture-view");

        let title = adw::WindowTitle::new("PicaLens Probe", "Waiting for window selection");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&picture));
        window.set_content(Some(&toolbar));

        let state = Rc::new(RefCell::new(State::default()));
        if experiment {
            let arm = gtk::Button::with_label("Arm sample");
            arm.set_tooltip_text(Some("Allow one test input within five seconds"));
            arm.connect_clicked({
                let state = state.clone();
                let picture = picture.downgrade();
                move |_| {
                    if let Some(picture) = picture.upgrade() {
                        state.borrow_mut().arm(&picture);
                    }
                }
            });
            header.pack_start(&arm);
        }
        window.connect_close_request({
            let stop = stop.clone();
            move |_| {
                request_stop(&stop, "preview window closed");
                glib::Propagation::Stop
            }
        });
        window.connect_is_active_notify({
            let state = state.clone();
            move |window| {
                if !window.is_active() {
                    state.borrow_mut().invalidate("focus changed");
                }
            }
        });
        window.connect_fullscreened_notify({
            let state = state.clone();
            let header = header.clone();
            move |window| {
                state.borrow_mut().invalidate("layout changed");
                header.set_visible(!window.is_fullscreen());
                eprintln!("preview fullscreen={}", window.is_fullscreen());
            }
        });

        let observed = gtk::EventControllerLegacy::new();
        observed.set_propagation_phase(gtk::PropagationPhase::Capture);
        observed.connect_event({
            let state = state.clone();
            move |_, event| {
                use gdk::EventType::{ButtonPress, ButtonRelease, KeyPress, KeyRelease, Scroll};
                let kind = event.event_type();
                if matches!(
                    kind,
                    ButtonPress | ButtonRelease | KeyPress | KeyRelease | Scroll
                ) && state
                    .borrow()
                    .observe_until
                    .is_some_and(|until| Instant::now() < until)
                {
                    if let Some(key) = event.downcast_ref::<gdk::KeyEvent>() {
                        eprintln!(
                            "preview monotonic_us={} observed {kind:?} key={:?} during sample",
                            glib::monotonic_time(),
                            key.keyval()
                        );
                        if key.keyval() == gdk::Key::space {
                            return glib::Propagation::Stop;
                        }
                    } else {
                        eprintln!(
                            "preview monotonic_us={} observed {kind:?} position={:?} during sample",
                            glib::monotonic_time(),
                            event.position()
                        );
                    }
                }
                glib::Propagation::Proceed
            }
        });
        window.add_controller(observed);

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let state = state.clone();
            move |_, key, _, _| {
                match key {
                    gdk::Key::Escape => {
                        if !state.borrow_mut().disarm("Escape") {
                            request_stop(&stop, "Escape");
                        }
                    }
                    gdk::Key::F11 | gdk::Key::F8 | gdk::Key::F9 => {}
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        });
        keys.connect_key_released({
            let state = state.clone();
            let picture = picture.downgrade();
            let window = window.downgrade();
            move |_, key, _, modifiers| {
                let (Some(picture), Some(window)) = (picture.upgrade(), window.upgrade()) else {
                    return;
                };
                match key {
                    gdk::Key::F8 => state.borrow_mut().arm(&picture),
                    gdk::Key::F9 => {
                        let mut state = state.borrow_mut();
                        if modifiers.intersects(
                            gdk::ModifierType::CONTROL_MASK
                                | gdk::ModifierType::ALT_MASK
                                | gdk::ModifierType::SUPER_MASK
                                | gdk::ModifierType::SHIFT_MASK,
                        ) {
                            state.disarm("keyboard sample used a modifier");
                        } else {
                            state.sample(&picture, Sample::Key);
                        }
                    }
                    gdk::Key::F11 => {
                        state.borrow_mut().invalidate("layout requested");
                        if window.is_fullscreen() {
                            window.unfullscreen();
                        } else {
                            window.fullscreen();
                        }
                    }
                    _ => {}
                }
            }
        });
        window.add_controller(keys);

        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        click.connect_released({
            let state = state.clone();
            let picture = picture.downgrade();
            move |_, _, x, y| {
                if let Some(picture) = picture.upgrade() {
                    state.borrow_mut().sample(&picture, Sample::Click(x, y));
                }
            }
        });
        picture.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        for_enter_and_motion(&motion, &state);
        motion.connect_leave({
            let state = state.clone();
            move |_| state.borrow_mut().pointer = None
        });
        picture.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::DISCRETE,
        );
        scroll.connect_scroll({
            let state = state.clone();
            let picture = picture.downgrade();
            move |_, dx, dy| {
                if let Some(picture) = picture.upgrade() {
                    state.borrow_mut().sample(&picture, Sample::Scroll(dx, dy));
                }
                glib::Propagation::Stop
            }
        });
        picture.add_controller(scroll);

        Self {
            window,
            picture,
            header,
            title,
            state,
        }
    }

    pub async fn show(
        &self,
        capture: Capture,
        input: Option<EisInput>,
        logical_size: Option<(i32, i32)>,
    ) -> Result<&'static str, Box<dyn std::error::Error>> {
        {
            let mut state = self.state.borrow_mut();
            state.input = input;
            state.logical_size = logical_size;
        }
        let _input = ClearInput(&self.state);
        let mut started = false;
        let ready_since = Instant::now();
        loop {
            if let Some(error) = capture.take_error() {
                return Err(error.into());
            }
            if let Some(frame) = capture.take_frame() {
                self.present_frame(frame)?;
                started = true;
            }
            if !started && ready_since.elapsed() > Duration::from_secs(15) {
                return Err("no usable PipeWire frame received within 15 seconds".into());
            }
            {
                let mut state = self.state.borrow_mut();
                if let Some(status) = state.input.as_ref().and_then(EisInput::take_status) {
                    eprintln!("input: {status}");
                }
                if !state.fresh() || !state.can_input() {
                    state.disarm("stale frame or unavailable mapping");
                }
                if state.arm.as_ref().is_some_and(|arm| {
                    Instant::now() >= arm.expires || arm.view_size != view_size(&self.picture)
                }) {
                    state.disarm("sample expired or view resized");
                }
                if self.header.is_visible() {
                    let subtitle = if !started {
                        "Waiting for PipeWire frames"
                    } else if state.arm.is_some() {
                        "Armed for one sample"
                    } else if state.can_input() && state.fresh() {
                        "Experiment only · F8 arms one sample · F11 fullscreen"
                    } else {
                        "View only · F11 fullscreen"
                    };
                    if self.title.subtitle() != subtitle {
                        self.title.set_subtitle(subtitle);
                    }
                }
            }
            glib::timeout_future(Duration::from_millis(16)).await;
        }
    }

    fn present_frame(&self, frame: Frame) -> Result<(), Box<dyn std::error::Error>> {
        let width = i32::try_from(frame.width)?;
        let height = i32::try_from(frame.height)?;
        let stride = usize::try_from(frame.width)?
            .checked_mul(4)
            .ok_or("stride overflow")?;
        if width == 0
            || height == 0
            || stride.checked_mul(frame.height as usize) != Some(frame.rgba.len())
        {
            return Err("invalid RGBA preview frame".into());
        }
        let mut state = self.state.borrow_mut();
        let picture = PictureGeometry {
            size: (frame.width, frame.height),
            stream_size: frame.stream_size,
            origin: frame.crop_origin,
        };
        if state.frame.is_some_and(|old| old.picture != picture) {
            state.invalidate("source geometry changed");
        }
        state.frame = Some(Geometry {
            picture,
            received: Instant::now(),
        });
        drop(state);

        let bytes = glib::Bytes::from_owned(frame.rgba);
        let texture =
            gdk::MemoryTexture::new(width, height, gdk::MemoryFormat::R8g8b8a8, &bytes, stride);
        self.picture.set_paintable(Some(&texture));
        Ok(())
    }
}

fn for_enter_and_motion(motion: &gtk::EventControllerMotion, state: &Rc<RefCell<State>>) {
    let record = {
        let state = state.clone();
        move |_: &gtk::EventControllerMotion, x, y| state.borrow_mut().pointer = Some((x, y))
    };
    motion.connect_enter(record.clone());
    motion.connect_motion(record);
}

pub fn request_stop(stop: &StopSender, reason: &'static str) {
    if let Some(sender) = stop.borrow_mut().take() {
        let _ = sender.send(reason);
    }
}

#[derive(Debug)]
enum Sample {
    Click(f64, f64),
    Scroll(f64, f64),
    Key,
}

impl State {
    fn fresh(&self) -> bool {
        self.frame
            .is_some_and(|frame| frame.received.elapsed() <= FRESH_FRAME)
    }

    fn can_input(&self) -> bool {
        let (Some(frame), Some(input)) = (self.frame, self.input.as_ref()) else {
            return false;
        };
        if !input.is_ready() {
            return false;
        }
        let Some(region) = input.region_size() else {
            return false;
        };
        let picture = frame.picture;
        if !picture.matches_region(region) {
            return false;
        }
        self.logical_size.is_none_or(|(width, height)| {
            let logical = (width.max(0) as u32, height.max(0) as u32);
            matching_aspect(picture.stream_size, logical) || matching_aspect(picture.size, logical)
        })
    }

    fn arm(&mut self, picture: &gtk::Picture) {
        if self
            .observe_until
            .is_some_and(|until| Instant::now() < until)
        {
            return;
        }
        if self.disarm("F8") {
            return;
        }
        if !self.fresh() || !self.can_input() {
            eprintln!("input cannot arm: no fresh frame with an unambiguous EIS region");
            return;
        }
        if let Some(frame) = self.frame {
            self.arm = Some(Arm {
                expires: Instant::now() + ARM_TIME,
                view_size: view_size(picture),
                geometry: frame.picture,
            });
            eprintln!("input armed for one sample (5 seconds)");
        }
    }

    fn disarm(&mut self, reason: &str) -> bool {
        if self.arm.take().is_none() {
            return false;
        }
        if let Some(input) = &self.input {
            input.release();
        }
        eprintln!("input disarmed: {reason}");
        true
    }

    fn invalidate(&mut self, reason: &str) {
        if !self.disarm(reason)
            && let Some(input) = &self.input
        {
            input.release();
        }
    }

    fn sample(&mut self, picture: &gtk::Picture, sample: Sample) {
        let Some(arm) = self.arm.take() else { return };
        if !self.fresh()
            || !self.can_input()
            || Instant::now() >= arm.expires
            || arm.view_size != view_size(picture)
            || self.frame.is_none_or(|frame| frame.picture != arm.geometry)
        {
            eprintln!("sample rejected: frame, view or mapping changed");
            return;
        }
        let Some(input) = &self.input else { return };

        let position = |x, y| {
            normalize_point(
                x,
                y,
                f64::from(arm.view_size.0),
                f64::from(arm.view_size.1),
                arm.geometry.size.0,
                arm.geometry.size.1,
            )
            .map(|(x, y)| {
                let (x, y) = arm.geometry.stream_point(x, y);
                InputEvent::Motion { x, y }
            })
        };
        let sent = match sample {
            Sample::Click(x, y) => match position(x, y) {
                Some(motion) => input.send_batch(&[
                    motion,
                    InputEvent::Button {
                        code: 0x110,
                        pressed: true,
                    },
                    InputEvent::Button {
                        code: 0x110,
                        pressed: false,
                    },
                ]),
                None => {
                    eprintln!("sample ignored: click in letterbox");
                    return;
                }
            },
            Sample::Scroll(dx, dy) if input.supports_scroll() => {
                let Some(motion) = self.pointer.and_then(|(x, y)| position(x, y)) else {
                    eprintln!("sample ignored: scroll outside the picture");
                    return;
                };
                input.send_batch(&[
                    motion,
                    InputEvent::Scroll {
                        dx: dx.clamp(-1.0, 1.0),
                        dy: dy.clamp(-1.0, 1.0),
                    },
                ])
            }
            Sample::Key if input.supports_keyboard() => input.send_batch(&[
                InputEvent::Key {
                    code: 57,
                    pressed: true,
                },
                InputEvent::Key {
                    code: 57,
                    pressed: false,
                },
            ]),
            _ => {
                eprintln!("sample ignored: device capability unavailable");
                return;
            }
        };
        if sent {
            self.observe_until = Some(Instant::now() + Duration::from_secs(1));
        }
        eprintln!(
            "input monotonic_us={} sample={sample:?} queued={sent}; verify destination in all three logs",
            glib::monotonic_time(),
        );
    }
}

fn view_size(picture: &gtk::Picture) -> (i32, i32) {
    (picture.width(), picture.height())
}

impl PictureGeometry {
    fn matches_region(self, region: (u32, u32)) -> bool {
        if self.origin != (0, 0) || self.size != self.stream_size {
            // The crop offsets are in stream pixels. Require the same space from EIS.
            region == self.stream_size
        } else {
            matching_aspect(self.stream_size, region)
        }
    }

    fn stream_point(self, x: f64, y: f64) -> (f64, f64) {
        (
            (f64::from(self.origin.0) + x * f64::from(self.size.0)) / f64::from(self.stream_size.0),
            (f64::from(self.origin.1) + y * f64::from(self.size.1)) / f64::from(self.stream_size.1),
        )
    }
}

fn matching_aspect(frame: (u32, u32), region: (u32, u32)) -> bool {
    if frame.0 == 0 || frame.1 == 0 || region.0 == 0 || region.1 == 0 {
        return false;
    }
    let frame_ratio = f64::from(frame.0) / f64::from(frame.1);
    let region_ratio = f64::from(region.0) / f64::from(region.1);
    (frame_ratio / region_ratio - 1.0).abs() <= 0.01
}

struct ClearInput<'a>(&'a RefCell<State>);

impl Drop for ClearInput<'_> {
    fn drop(&mut self) {
        let mut state = self.0.borrow_mut();
        state.disarm("session stopping");
        state.input = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{PictureGeometry, matching_aspect};

    #[test]
    fn allows_integer_and_fractional_scaling_with_rounding() {
        assert!(matching_aspect((1200, 900), (800, 600)));
        assert!(matching_aspect((1199, 900), (800, 600)));
        assert!(!matching_aspect((1920, 1080), (800, 600)));
        assert!(!matching_aspect((800, 0), (800, 0)));
    }

    #[test]
    fn maps_visible_crop_into_the_confirmed_stream_region() {
        let picture = PictureGeometry {
            size: (800, 600),
            stream_size: (1600, 1000),
            origin: (100, 50),
        };
        assert!(picture.matches_region((1600, 1000)));
        assert!(!picture.matches_region((800, 600)));
        assert_eq!(picture.stream_point(0.0, 0.0), (0.0625, 0.05));
        assert_eq!(picture.stream_point(0.5, 0.5), (0.3125, 0.35));
    }
}
