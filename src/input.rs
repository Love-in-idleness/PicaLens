use reis::PendingRequestResult;
use reis::ei;
use reis::event::{Device, DeviceCapability, EiEvent, EiEventConverter, Region};
use reis::handshake::{EiHandshaker, HandshakeResp};
use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 32;
const READ_INTERVAL: Duration = Duration::from_millis(10);
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputEvent {
    /// Coordinates normalized to the visible picture.
    Motion {
        x: f64,
        y: f64,
    },
    /// Linux evdev mouse button code.
    Button {
        code: u32,
        pressed: bool,
    },
    /// Wheel steps, positive values scroll right or down.
    Scroll {
        dx: f64,
        dy: f64,
    },
    /// Linux evdev key code.
    Key {
        code: u32,
        pressed: bool,
    },
    ReleaseAll,
}

/// Converts a point in an aspect-fitted image to normalized source coordinates.
/// The right and bottom edges and any letterbox bars are outside the image.
pub fn normalize_point(
    x: f64,
    y: f64,
    view_width: f64,
    view_height: f64,
    source_width: u32,
    source_height: u32,
) -> Option<(f64, f64)> {
    if !view_width.is_finite()
        || !view_height.is_finite()
        || source_width == 0
        || source_height == 0
    {
        return None;
    }
    let scale = (view_width / f64::from(source_width)).min(view_height / f64::from(source_height));
    if scale <= 0.0 {
        return None;
    }
    let width = f64::from(source_width) * scale;
    let height = f64::from(source_height) * scale;
    let x = (x - (view_width - width) / 2.0) / width;
    let y = (y - (view_height - height) / 2.0) / height;
    normalized(x, y).then_some((x, y))
}

fn normalized(x: f64, y: f64) -> bool {
    (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y)
}

#[derive(Clone, Copy)]
struct Capabilities {
    region: Extent,
    keyboard: bool,
    scroll: bool,
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<InputEvent>,
    capabilities: Option<Capabilities>,
    status: Option<String>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

pub struct EisInput {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl EisInput {
    /// Consumes a socket from the authorized portal session. Matching a region
    /// identifies coordinates only; the experiment must verify actual delivery.
    pub fn start(fd: OwnedFd, mapping_id: String, keyboard: bool) -> io::Result<Self> {
        if mapping_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "missing EIS mapping_id",
            ));
        }
        let context = ei::Context::new(UnixStream::from(fd))?;
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("picalens-eis".into())
            .spawn(move || {
                let result = run(&context, &worker_shared, &mapping_id, keyboard);
                let mut queue = worker_shared.queue.lock().unwrap();
                queue.closed = true;
                queue.pending.clear();
                queue.capabilities = None;
                if let Err(error) = result {
                    queue.status = Some(error);
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    pub fn is_ready(&self) -> bool {
        self.shared.queue.lock().unwrap().capabilities.is_some()
    }

    pub fn region_size(&self) -> Option<(u32, u32)> {
        let caps = self.shared.queue.lock().unwrap().capabilities?;
        Some((caps.region.width, caps.region.height))
    }

    pub fn supports_scroll(&self) -> bool {
        self.shared
            .queue
            .lock()
            .unwrap()
            .capabilities
            .is_some_and(|caps| caps.scroll)
    }

    pub fn supports_keyboard(&self) -> bool {
        self.shared
            .queue
            .lock()
            .unwrap()
            .capabilities
            .is_some_and(|caps| caps.keyboard)
    }

    pub fn take_status(&self) -> Option<String> {
        self.shared.queue.lock().unwrap().status.take()
    }

    /// Queues a deliberate trial atomically so a click cannot enqueue a press
    /// without its matching release when the queue is full.
    pub fn send_batch(&self, events: &[InputEvent]) -> bool {
        let mut queue = self.shared.queue.lock().unwrap();
        let Some(caps) = queue.capabilities else {
            return false;
        };
        if events.is_empty()
            || !events.iter().all(|event| match *event {
                InputEvent::Motion { x, y } => normalized(x, y),
                InputEvent::Button { .. } => true,
                InputEvent::Scroll { dx, dy } => caps.scroll && dx.is_finite() && dy.is_finite(),
                InputEvent::Key { .. } => caps.keyboard,
                InputEvent::ReleaseAll => false,
            })
        {
            return false;
        }
        if events.len() > QUEUE_CAPACITY - queue.pending.len() {
            queue.pending.clear();
            queue.capabilities = None;
            queue.closed = true;
            queue.status = Some("EIS input stopped: trial queue overflow".into());
            self.shared.wake.notify_one();
            return false;
        }
        queue.pending.extend(events);
        self.shared.wake.notify_one();
        true
    }

    /// Discards pending trials and clears any pressed state on the worker.
    pub fn release(&self) {
        let mut queue = self.shared.queue.lock().unwrap();
        if !queue.closed {
            queue.pending.clear();
            queue.pending.push_back(InputEvent::ReleaseAll);
            self.shared.wake.notify_one();
        }
    }
}

impl Drop for EisInput {
    fn drop(&mut self) {
        {
            let mut queue = self.shared.queue.lock().unwrap();
            queue.closed = true;
            queue.capabilities = None;
            queue.pending.clear();
            self.shared.wake.notify_one();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read(context: &ei::Context) -> Result<(), String> {
    match context.read() {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(()),
        Err(error) => Err(format!("EIS session ended: {error}")),
    }
}

fn flush(context: &ei::Context) -> Result<(), String> {
    context
        .flush()
        .map_err(|error| format!("EIS send failed: {error}"))
}

fn protocol_event(result: PendingRequestResult<ei::Event>) -> Result<ei::Event, String> {
    match result {
        PendingRequestResult::Request(event) => Ok(event),
        PendingRequestResult::InvalidObject(id) => Err(format!("EIS invalid object {id}")),
        PendingRequestResult::ParseError(error) => Err(format!("EIS invalid message: {error:?}")),
    }
}

fn handshake(context: &ei::Context, shared: &Shared) -> Result<Option<HandshakeResp>, String> {
    let mut handshake = EiHandshaker::new("PicaLens probe", ei::handshake::ContextType::Sender);
    let deadline = Instant::now() + SETUP_TIMEOUT;
    loop {
        let queue = shared.queue.lock().unwrap();
        if queue.closed {
            return Ok(None);
        }
        read(context)?;
        while let Some(result) = context.pending_event() {
            if let Some(response) = handshake
                .handle_event(protocol_event(result)?)
                .map_err(|error| format!("EIS handshake failed: {error}"))?
            {
                return Ok(Some(response));
            }
        }
        if Instant::now() >= deadline {
            return Err("EIS handshake timed out".into());
        }
        flush(context)?;
        let _wait = shared.wake.wait_timeout(queue, READ_INTERVAL).unwrap();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Extent {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl From<&Region> for Extent {
    fn from(region: &Region) -> Self {
        Self {
            x: region.x,
            y: region.y,
            width: region.width,
            height: region.height,
        }
    }
}

impl Extent {
    fn point(self, x: f64, y: f64) -> (f32, f32) {
        fn coordinate(offset: u32, size: u32, fraction: f64) -> f32 {
            let end = f64::from(offset) + f64::from(size);
            let rounded = (f64::from(offset) + fraction * f64::from(size)) as f32;
            if f64::from(rounded) >= end {
                rounded.next_down()
            } else if f64::from(rounded) < f64::from(offset) {
                rounded.next_up()
            } else {
                rounded
            }
        }
        (
            coordinate(self.x, self.width, x),
            coordinate(self.y, self.height, y),
        )
    }
}

struct Target {
    pointer: Device,
    region: Extent,
    scroll: bool,
    keyboard: Option<Device>,
}

impl Target {
    fn resolve(
        devices: &[(Device, bool)],
        mapping_id: &str,
        keyboard_requested: bool,
    ) -> Result<Option<Self>, String> {
        let mut matching = devices
            .iter()
            .filter(|(device, _)| device.has_capability(DeviceCapability::PointerAbsolute))
            .flat_map(|(device, resumed)| {
                device
                    .regions()
                    .iter()
                    .filter(|region| region.mapping_id.as_deref() == Some(mapping_id))
                    .map(move |region| (device, *resumed, region))
            });
        let Some((device, resumed, region)) = matching.next() else {
            return Ok(None);
        };
        if matching.next().is_some() {
            return Err(format!(
                "EIS mapping_id={mapping_id:?} matches multiple regions"
            ));
        }
        if region.width == 0 || region.height == 0 {
            return Err(format!("EIS mapping_id={mapping_id:?} has an empty region"));
        }
        let extent = Extent::from(region);
        let (x, y) = extent.point(0.0, 0.0);
        if f64::from(x) >= f64::from(extent.x) + f64::from(extent.width)
            || f64::from(y) >= f64::from(extent.y) + f64::from(extent.height)
        {
            return Err(format!(
                "EIS mapping_id={mapping_id:?} has no representable coordinates"
            ));
        }
        if !resumed || !device.has_capability(DeviceCapability::Button) {
            return Ok(None);
        }

        let keyboard = if keyboard_requested {
            let mut keyboards = devices.iter().filter(|(other, _)| {
                other.seat() == device.seat() && other.has_capability(DeviceCapability::Keyboard)
            });
            match (keyboards.next(), keyboards.next()) {
                (Some((keyboard, true)), None) => Some(keyboard.clone()),
                _ => None,
            }
        } else {
            None
        };
        Ok(Some(Self {
            pointer: device.clone(),
            region: extent,
            scroll: device.has_capability(DeviceCapability::Scroll),
            keyboard,
        }))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            region: self.region,
            keyboard: self.keyboard.is_some(),
            scroll: self.scroll,
        }
    }
}

#[derive(Default)]
struct Pressed {
    buttons: BTreeSet<u32>,
    keys: BTreeSet<u32>,
    positioned: bool,
}

impl Pressed {
    fn apply(&mut self, target: &Target, event: InputEvent, serial: u32) {
        match event {
            InputEvent::Motion { x, y } => {
                if let Some(pointer) = target.pointer.interface::<ei::PointerAbsolute>() {
                    let (x, y) = target.region.point(x, y);
                    pointer.motion_absolute(x, y);
                    frame(&target.pointer, serial);
                    self.positioned = true;
                }
            }
            InputEvent::Button { code, pressed } => {
                if (self.positioned || !pressed) && change_pressed(&mut self.buttons, code, pressed)
                {
                    button(&target.pointer, serial, code, pressed);
                }
            }
            InputEvent::Scroll { dx, dy } => {
                let x = (dx * 120.0).round() as i32;
                let y = (dy * 120.0).round() as i32;
                if self.positioned
                    && (x != 0 || y != 0)
                    && let Some(scroll) = target.pointer.interface::<ei::Scroll>()
                {
                    scroll.scroll_discrete(x, y);
                    frame(&target.pointer, serial);
                    scroll.scroll_stop(u32::from(x != 0), u32::from(y != 0), 0);
                    frame(&target.pointer, serial);
                }
            }
            InputEvent::Key { code, pressed } => {
                if let Some(device) = &target.keyboard
                    && change_pressed(&mut self.keys, code, pressed)
                {
                    key(device, serial, code, pressed);
                }
            }
            InputEvent::ReleaseAll => self.release(target, serial, |_| true),
        }
    }

    fn release(&mut self, target: &Target, serial: u32, resumed: impl Fn(&Device) -> bool) {
        if resumed(&target.pointer) {
            for &code in &self.buttons {
                button(&target.pointer, serial, code, false);
            }
        }
        if let Some(keyboard) = &target.keyboard
            && resumed(keyboard)
        {
            for &code in &self.keys {
                key(keyboard, serial, code, false);
            }
        }
        *self = Self::default();
    }
}

fn change_pressed(codes: &mut BTreeSet<u32>, code: u32, pressed: bool) -> bool {
    if pressed {
        codes.insert(code)
    } else {
        codes.remove(&code)
    }
}

fn button(device: &Device, serial: u32, code: u32, pressed: bool) {
    if let Some(button) = device.interface::<ei::Button>() {
        let state = if pressed {
            ei::button::ButtonState::Press
        } else {
            ei::button::ButtonState::Released
        };
        button.button(code, state);
        frame(device, serial);
    }
}

fn key(device: &Device, serial: u32, code: u32, pressed: bool) {
    if let Some(keyboard) = device.interface::<ei::Keyboard>() {
        let state = if pressed {
            ei::keyboard::KeyState::Press
        } else {
            ei::keyboard::KeyState::Released
        };
        keyboard.key(code, state);
        frame(device, serial);
    }
}

fn frame(device: &Device, serial: u32) {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: time points to a writable timespec, and Linux supports CLOCK_MONOTONIC.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    let micros = time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1_000;
    device.device().frame(serial, micros);
}

fn run(
    context: &ei::Context,
    shared: &Shared,
    mapping_id: &str,
    keyboard: bool,
) -> Result<(), String> {
    let Some(response) = handshake(context, shared)? else {
        return Ok(());
    };
    let mut converter = EiEventConverter::new(context, response);
    let mut devices = Vec::<(Device, bool)>::new();
    let mut target = None::<Target>;
    let mut pressed = Pressed::default();
    let deadline = Instant::now() + SETUP_TIMEOUT;
    let result = (|| {
        loop {
            read(context)?;
            while let Some(result) = context.pending_event() {
                converter
                    .handle_event(protocol_event(result)?)
                    .map_err(|error| format!("EIS protocol error: {error}"))?;
                while let Some(event) = converter.next_event() {
                    match event {
                        EiEvent::SeatAdded(event) => {
                            let mut caps = DeviceCapability::PointerAbsolute
                                | DeviceCapability::Button
                                | DeviceCapability::Scroll;
                            if keyboard {
                                caps |= DeviceCapability::Keyboard;
                            }
                            event.seat.bind_capabilities(caps);
                        }
                        EiEvent::DeviceAdded(event) => devices.push((event.device, false)),
                        EiEvent::DevicePaused(event) => {
                            if let Some((_, active)) = devices
                                .iter_mut()
                                .find(|(device, _)| *device == event.device)
                            {
                                *active = false;
                            }
                            if target.as_ref().is_some_and(|current| {
                                current.pointer == event.device
                                    || current.keyboard.as_ref() == Some(&event.device)
                            }) {
                                return Err("EIS mapped input device was paused".into());
                            }
                        }
                        EiEvent::DeviceResumed(event) => {
                            if let Some((_, active)) = devices
                                .iter_mut()
                                .find(|(device, _)| *device == event.device)
                            {
                                *active = true;
                            }
                        }
                        EiEvent::DeviceRemoved(event) => {
                            devices.retain(|(device, _)| *device != event.device)
                        }
                        EiEvent::SeatRemoved(event) => {
                            devices.retain(|(device, _)| *device.seat() != event.seat)
                        }
                        EiEvent::Disconnected(event) => {
                            devices.clear();
                            return Err(format!(
                                "EIS disconnected: {:?} {:?}",
                                event.reason, event.explanation
                            ));
                        }
                        _ => {}
                    }
                }
            }

            let mut queue = shared.queue.lock().unwrap();
            if queue.closed {
                return Ok(());
            }
            let serial = converter.connection().serial();
            let resolved = Target::resolve(&devices, mapping_id, keyboard)?;
            if let Some(current) = &mut target {
                let Some(next) = resolved else {
                    return Err("EIS mapped device is no longer available".into());
                };
                if current.pointer != next.pointer
                    || current.region != next.region
                    || current.scroll != next.scroll
                    || (current.keyboard.is_some() && current.keyboard != next.keyboard)
                {
                    return Err("EIS mapped device or its input capabilities changed".into());
                }
                if current.keyboard.is_none()
                    && let Some(keyboard) = next.keyboard
                {
                    if keyboard != current.pointer {
                        keyboard.device().start_emulating(serial, 2);
                    }
                    current.keyboard = Some(keyboard);
                    queue.capabilities = Some(current.capabilities());
                    queue.status = Some(
                        "EIS keyboard now available on the mapped seat; delivery follows focus"
                            .into(),
                    );
                }
            } else if let Some(resolved) = resolved {
                resolved.pointer.device().start_emulating(serial, 1);
                if let Some(keyboard) = &resolved.keyboard
                    && *keyboard != resolved.pointer
                {
                    keyboard.device().start_emulating(serial, 2);
                }
                let region = resolved.region;
                let caps = resolved.capabilities();
                queue.capabilities = Some(caps);
                queue.status = Some(format!(
                    "EIS mapping_id={mapping_id:?} region={}x{}+{}+{} scroll={} keyboard={}; delivery unverified",
                    region.width, region.height, region.x, region.y, caps.scroll, caps.keyboard
                ));
                target = Some(resolved);
            } else if Instant::now() >= deadline {
                return Err(format!(
                    "EIS mapping_id={mapping_id:?} has no usable pointer region"
                ));
            }

            if let Some(target) = &target {
                while let Some(event) = queue.pending.pop_front() {
                    pressed.apply(target, event, serial);
                }
            }
            flush(context)?;
            let _wait = shared.wake.wait_timeout(queue, READ_INTERVAL).unwrap();
        }
    })();

    {
        let mut queue = shared.queue.lock().unwrap();
        queue.closed = true;
        queue.capabilities = None;
        queue.pending.clear();
    }
    if let Some(target) = &target {
        let serial = converter.connection().serial();
        let resumed = |device: &Device| {
            devices
                .iter()
                .any(|(known, active)| known == device && *active)
        };
        pressed.release(target, serial, resumed);
        if resumed(&target.pointer) {
            target.pointer.device().stop_emulating(serial);
        }
        if let Some(keyboard) = &target.keyboard
            && *keyboard != target.pointer
            && resumed(keyboard)
        {
            keyboard.device().stop_emulating(serial);
        }
    }
    let _ = flush(context);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_fit_excludes_letterbox_and_maps_visible_points() {
        assert_eq!(
            normalize_point(240.0, 0.0, 1920.0, 1080.0, 800, 600),
            Some((0.0, 0.0))
        );
        assert_eq!(
            normalize_point(960.0, 540.0, 1920.0, 1080.0, 800, 600),
            Some((0.5, 0.5))
        );
        assert_eq!(
            normalize_point(239.0, 540.0, 1920.0, 1080.0, 800, 600),
            None
        );
        assert_eq!(
            normalize_point(1680.0, 540.0, 1920.0, 1080.0, 800, 600),
            None
        );
        assert_eq!(normalize_point(400.0, 19.0, 800.0, 640.0, 800, 600), None);
        assert_eq!(normalize_point(0.0, 620.0, 800.0, 640.0, 800, 600), None);
    }

    #[test]
    fn rejects_invalid_dimensions_and_non_finite_coordinates() {
        assert_eq!(normalize_point(0.0, 0.0, 0.0, 100.0, 800, 600), None);
        assert_eq!(normalize_point(0.0, 0.0, 100.0, 100.0, 800, 0), None);
        assert_eq!(normalize_point(f64::NAN, 0.0, 100.0, 100.0, 800, 600), None);
        assert_eq!(
            normalize_point(0.0, 0.0, f64::INFINITY, 100.0, 800, 600),
            None
        );
    }

    #[test]
    fn eis_uses_logical_extent_and_stays_inside_fractional_edges() {
        let extent = Extent {
            x: 120,
            y: 20,
            width: 640,
            height: 480,
        };
        assert_eq!(extent.point(0.0, 0.0), (120.0, 20.0));
        assert_eq!(extent.point(0.5, 0.5), (440.0, 260.0));
        let (x, y) = extent.point(1.0_f64.next_down(), 1.0_f64.next_down());
        assert!(x < 760.0 && y < 500.0);
    }
}
