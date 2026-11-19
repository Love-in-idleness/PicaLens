use std::{
    io::{self, Cursor},
    mem::size_of,
    os::fd::OwnedFd,
    ptr::NonNull,
    rc::Rc,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use pipewire as pw;
use pw::{properties::properties, spa};
use spa::{
    buffer::DataType,
    param::{
        ParamType,
        format::{FormatProperties, MediaSubtype, MediaType},
        format_utils::parse_format,
        video::{VideoFormat, VideoInfoRaw},
    },
    pod::{Object, Pod, Property, PropertyFlags, Value, serialize::PodSerializer},
    sys,
    utils::{Direction, Fraction, Id, Rectangle, SpaTypes},
};

const MAX_DIMENSION: u32 = 4096;
const CHUNK_FLAG_EMPTY: i32 = 1 << 1; // Not exported by older SPA headers.

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub stream_size: (u32, u32),
    pub crop_origin: (u32, u32),
}

#[derive(Default)]
struct Mailbox {
    frame: Option<Frame>,
    error: Option<String>,
}

pub struct Capture {
    mailbox: Arc<Mutex<Mailbox>>,
    stop: pw::channel::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn take_frame(&self) -> Option<Frame> {
        self.mailbox.lock().unwrap().frame.take()
    }

    pub fn take_error(&self) -> Option<String> {
        self.mailbox.lock().unwrap().error.take()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn start(fd: OwnedFd, node_id: u32) -> io::Result<Capture> {
    if node_id == pw::core::PW_ID_CORE || node_id == u32::MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid PipeWire node ID",
        ));
    }
    let mailbox = Arc::new(Mutex::new(Mailbox::default()));
    let (stop, stop_rx) = pw::channel::channel();
    let worker_mailbox = Arc::clone(&mailbox);
    let worker = thread::Builder::new()
        .name("picalens-capture".into())
        .spawn(move || {
            let result = run(fd, node_id, stop_rx, &worker_mailbox);
            if let Err(error) = result {
                eprintln!("PipeWire capture stopped: {error}");
                let mut mailbox = worker_mailbox.lock().unwrap();
                mailbox.frame = None;
                mailbox.error = Some(error.to_string());
            } else {
                eprintln!("PipeWire capture stopped: requested shutdown");
            }
        })?;
    Ok(Capture {
        mailbox,
        stop,
        worker: Some(worker),
    })
}

struct StreamData {
    format: Option<VideoInfoRaw>,
    frames: u64,
    last_report: Instant,
    layout: Option<FrameLayout>,
}

fn run(
    fd: OwnedFd,
    node_id: u32,
    stop: pw::channel::Receiver<()>,
    mailbox: &Arc<Mutex<Mailbox>>,
) -> Result<(), Box<dyn std::error::Error>> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let _stop = stop.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;

    let failure = Rc::new(std::cell::RefCell::new(None));
    let fail = {
        let mainloop = mainloop.clone();
        let failure = Rc::clone(&failure);
        Rc::new(move |reason: String| {
            let mut failure = failure.borrow_mut();
            if failure.is_none() {
                *failure = Some(reason);
            }
            mainloop.quit();
        })
    };
    let _core_listener = core
        .add_listener_local()
        .error({
            let fail = Rc::clone(&fail);
            move |id, _, code, message| {
                fail(format!("core error on object {id} ({code}): {message}"))
            }
        })
        .register();
    let registry = core.get_registry()?;
    let _registry_listener = registry
        .add_listener_local()
        .global_remove({
            let fail = Rc::clone(&fail);
            move |id| {
                if id == node_id {
                    fail(format!("source node {node_id} was removed"));
                }
            }
        })
        .register();

    let stream = pw::stream::StreamBox::new(
        &core,
        "picalens-probe",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let buffers = serialize(buffer_param())?;
    let crop_meta = serialize(crop_param())?;
    let listener = stream.add_local_listener_with_user_data(StreamData {
        format: None,
        frames: 0,
        last_report: Instant::now(),
        layout: None,
    })
    .state_changed({
        let fail = Rc::clone(&fail);
        move |_, _, old, new| {
            eprintln!("PipeWire stream state: {old:?} -> {new:?}");
            match new {
                pw::stream::StreamState::Error(error) => fail(error),
                pw::stream::StreamState::Unconnected
                    if old != pw::stream::StreamState::Unconnected =>
                {
                    fail("source stream disconnected".into())
                }
                _ => {}
            }
        }
    })
    .param_changed({
        let fail = Rc::clone(&fail);
        let mailbox = Arc::clone(mailbox);
        move |stream, state, id, pod| {
            if id != ParamType::Format.as_raw() {
                return;
            }
            state.format = None;
            state.layout = None;
            mailbox.lock().unwrap().frame = None;
            let Some(pod) = pod else { return };
            match negotiated_format(pod).and_then(|format| {
                stream.update_params(&mut [pod_bytes(&buffers), pod_bytes(&crop_meta)])
                    .map_err(|error| format!("negotiate PipeWire buffers: {error}"))?;
                Ok(format)
            }) {
                Ok(format) => {
                    let Rectangle { width, height } = format.size();
                    eprintln!("PipeWire format: {:?}, {width}x{height}, {}/{} Hz",
                        format.format(), format.framerate().num, format.framerate().denom);
                    state.format = Some(format);
                }
                Err(error) => fail(error),
            }
        }
    })
    .process({
        let mailbox = Arc::clone(mailbox);
        move |stream, state| {
            // Keep the most recent buffer and return superseded ones to PipeWire immediately.
            let mut newest = None;
            while let Some(buffer) = Dequeued::new(stream) {
                match buffer.has_frame() {
                    Ok(true) => newest = Some(buffer),
                    Ok(false) => {}
                    Err(error) => {
                        fail(error);
                        return;
                    }
                }
            }
            let (Some(newest), Some(format)) = (newest, state.format) else { return };
            match newest.copy_frame(&format) {
                Ok((frame, layout)) => {
                    let report = state.frames == 0 || state.last_report.elapsed() >= Duration::from_secs(1);
                    if state.layout.is_none() || (report && state.layout != Some(layout)) {
                        eprintln!("PipeWire buffer: {:?}, stride={}, offset={}, crop={:?}, cursor=hidden",
                            layout.kind, layout.stride, layout.offset, layout.crop);
                        state.layout = Some(layout);
                    }
                    state.frames += 1;
                    if report {
                        eprintln!("PipeWire frames received: {}", state.frames);
                        state.last_report = Instant::now();
                    }
                    // No PipeWire memory leaves the worker or remains mapped by the UI.
                    drop(newest);
                    mailbox.lock().unwrap().frame = Some(frame);
                }
                Err(error) => fail(error),
            }
        }
    })
    .register()?;

    let format = serialize(format_param())?;
    stream.connect(
        Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::DONT_RECONNECT,
        &mut [pod_bytes(&format)],
    )?;
    eprintln!("PipeWire remote capture requested for node {node_id}");
    if failure.borrow().is_none() {
        mainloop.run();
    }
    drop(listener);
    let result = failure.borrow_mut().take();
    result.map_or(Ok(()), |message| Err(message.into()))
}

fn negotiated_format(pod: &Pod) -> Result<VideoInfoRaw, String> {
    if parse_format(pod).ok() != Some((MediaType::Video, MediaSubtype::Raw)) {
        return Err("expected raw PipeWire video".into());
    }
    let mut format = VideoInfoRaw::default();
    format
        .parse(pod)
        .map_err(|error| format!("parse PipeWire video format: {error}"))?;
    let Rectangle { width, height } = format.size();
    if !(1..=MAX_DIMENSION).contains(&width)
        || !(1..=MAX_DIMENSION).contains(&height)
        || pixel_order(format.format()).is_none()
    {
        return Err(format!(
            "unsupported PipeWire format {:?} at {width}x{height}",
            format.format()
        ));
    }
    Ok(format)
}

fn format_param() -> Object {
    spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
            VideoFormat::xRGB,
            VideoFormat::ARGB,
            VideoFormat::xBGR,
            VideoFormat::ABGR,
            VideoFormat::RGB,
            VideoFormat::BGR
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle {
                width: 800,
                height: 600
            },
            Rectangle {
                width: 1,
                height: 1
            },
            Rectangle {
                width: MAX_DIMENSION,
                height: MAX_DIMENSION
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 60, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction { num: 240, denom: 1 }
        ),
    )
}

fn property(key: u32, value: Value) -> Property {
    Property {
        key,
        flags: PropertyFlags::empty(),
        value,
    }
}

fn buffer_param() -> Object {
    let data_types = (1 << sys::SPA_DATA_MemFd) | (1 << sys::SPA_DATA_MemPtr);
    Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            property(sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            property(
                sys::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    spa::utils::ChoiceEnum::Flags {
                        default: data_types,
                        flags: vec![],
                    },
                ))),
            ),
        ],
    }
}

fn crop_param() -> Object {
    Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: ParamType::Meta.as_raw(),
        properties: vec![
            property(
                sys::SPA_PARAM_META_type,
                Value::Id(Id(sys::SPA_META_VideoCrop)),
            ),
            property(
                sys::SPA_PARAM_META_size,
                Value::Int(size_of::<sys::spa_meta_region>() as i32),
            ),
        ],
    }
}

fn serialize(object: Object) -> io::Result<Vec<u8>> {
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| io::Error::other(error.to_string()))
}

fn pod_bytes(bytes: &[u8]) -> &Pod {
    // The bytes originate in PodSerializer.
    Pod::from_bytes(bytes).unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Crop {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameLayout {
    kind: DataType,
    stride: i32,
    offset: u32,
    crop: Option<Crop>,
}

struct Dequeued<'a> {
    stream: &'a pw::stream::Stream,
    buffer: NonNull<pw::sys::pw_buffer>,
}

impl<'a> Dequeued<'a> {
    fn new(stream: &'a pw::stream::Stream) -> Option<Self> {
        // Holding the stream borrow keeps it alive until Drop requeues this packet.
        NonNull::new(unsafe { stream.dequeue_raw_buffer() }).map(|buffer| Self { stream, buffer })
    }

    fn spa_buffer(&self) -> Option<&sys::spa_buffer> {
        // PipeWire owns the packet and its SPA buffer for this dequeue interval.
        unsafe { self.buffer.as_ref().buffer.as_ref() }
    }

    fn plane(&self) -> Option<&sys::spa_data> {
        let buffer = self.spa_buffer()?;
        if buffer.n_datas != 1 {
            return None;
        }
        unsafe { buffer.datas.as_ref() }
    }

    fn has_frame(&self) -> Result<bool, String> {
        let plane = self.plane().ok_or("expected one RGB plane")?;
        let chunk = unsafe { plane.chunk.as_ref() }.ok_or("missing PipeWire chunk")?;
        let reject = sys::SPA_CHUNK_FLAG_CORRUPTED as i32 | CHUNK_FLAG_EMPTY;
        Ok(chunk.size != 0 && chunk.flags & reject == 0)
    }

    fn crop(&self) -> Result<Option<Crop>, String> {
        let buffer = self.spa_buffer().ok_or("missing SPA buffer")?;
        for i in 0..buffer.n_metas as usize {
            // PipeWire supplies a valid metadata array for the lifetime of the dequeued packet.
            if buffer.metas.is_null() {
                return Err("missing PipeWire metadata array".into());
            }
            let meta = unsafe { &*buffer.metas.add(i) };
            if meta.type_ != sys::SPA_META_VideoCrop {
                continue;
            }
            if meta.data.is_null() || (meta.size as usize) < size_of::<sys::spa_meta_region>() {
                return Err("invalid PipeWire VideoCrop metadata".into());
            }
            let region = unsafe {
                meta.data
                    .cast::<sys::spa_meta_region>()
                    .read_unaligned()
                    .region
            };
            if region.size.width == 0 || region.size.height == 0 {
                return Ok(None);
            }
            return Ok(Some(Crop {
                x: region
                    .position
                    .x
                    .try_into()
                    .map_err(|_| "negative crop X")?,
                y: region
                    .position
                    .y
                    .try_into()
                    .map_err(|_| "negative crop Y")?,
                width: region.size.width,
                height: region.size.height,
            }));
        }
        Ok(None)
    }

    fn copy_frame(&self, format: &VideoInfoRaw) -> Result<(Frame, FrameLayout), String> {
        let plane = self.plane().ok_or("expected one RGB plane")?;
        let kind = DataType::from_raw(plane.type_);
        if !matches!(kind, DataType::MemFd | DataType::MemPtr) {
            return Err(format!("unsupported PipeWire buffer type {kind:?}"));
        }
        let chunk = unsafe { plane.chunk.as_ref() }.ok_or("missing PipeWire chunk")?;
        let capacity = plane.maxsize as usize;
        if plane.data.is_null() || capacity == 0 || capacity > isize::MAX as usize {
            return Err(format!("unmapped PipeWire buffer ({kind:?})"));
        }
        let offset = chunk.offset as usize % capacity;
        let length = (chunk.size as usize).min(capacity);
        if length > capacity - offset {
            return Err("non-contiguous PipeWire frame".into());
        }
        // MAP_BUFFERS keeps this view mapped until the packet is returned to PipeWire.
        let bytes =
            unsafe { std::slice::from_raw_parts(plane.data.cast::<u8>().add(offset), length) };
        let crop = self.crop()?;
        let frame = copy_pixels(bytes, chunk.stride, format.size(), format.format(), crop)?;
        Ok((
            frame,
            FrameLayout {
                kind,
                stride: chunk.stride,
                offset: chunk.offset,
                crop,
            },
        ))
    }
}

impl Drop for Dequeued<'_> {
    fn drop(&mut self) {
        unsafe { self.stream.queue_raw_buffer(self.buffer.as_ptr()) }
    }
}

fn pixel_order(format: VideoFormat) -> Option<(usize, [usize; 3])> {
    Some(match format {
        VideoFormat::RGB => (3, [0, 1, 2]),
        VideoFormat::BGR => (3, [2, 1, 0]),
        VideoFormat::RGBA | VideoFormat::RGBx => (4, [0, 1, 2]),
        VideoFormat::BGRA | VideoFormat::BGRx => (4, [2, 1, 0]),
        VideoFormat::ARGB | VideoFormat::xRGB => (4, [1, 2, 3]),
        VideoFormat::ABGR | VideoFormat::xBGR => (4, [3, 2, 1]),
        _ => return None,
    })
}

fn copy_pixels(
    bytes: &[u8],
    stride: i32,
    size: Rectangle,
    format: VideoFormat,
    crop: Option<Crop>,
) -> Result<Frame, String> {
    let full = Crop {
        x: 0,
        y: 0,
        width: size.width,
        height: size.height,
    };
    let crop = crop.unwrap_or(full);
    if crop
        .x
        .checked_add(crop.width)
        .is_none_or(|right| right > size.width)
        || crop
            .y
            .checked_add(crop.height)
            .is_none_or(|bottom| bottom > size.height)
        || crop.width == 0
        || crop.height == 0
    {
        return Err(format!("PipeWire crop is outside the frame: {crop:?}"));
    }
    let (bytes_per_pixel, [red, green, blue]) =
        pixel_order(format).ok_or("unsupported RGB format")?;
    let stride = usize::try_from(stride).map_err(|_| "negative PipeWire stride")?;
    if stride < size.width as usize * bytes_per_pixel {
        return Err(format!("PipeWire stride {stride} is shorter than one row"));
    }
    let start_x = crop.x as usize * bytes_per_pixel;
    let row_bytes = crop.width as usize * bytes_per_pixel;
    let mut rgba = Vec::with_capacity(crop.width as usize * crop.height as usize * 4);
    for y in crop.y as usize..(crop.y + crop.height) as usize {
        let start = y
            .checked_mul(stride)
            .and_then(|row| row.checked_add(start_x))
            .ok_or("PipeWire row offset overflow")?;
        let end = start
            .checked_add(row_bytes)
            .ok_or("PipeWire row length overflow")?;
        let row = bytes.get(start..end).ok_or("truncated PipeWire frame")?;
        for pixel in row.chunks_exact(bytes_per_pixel) {
            rgba.extend_from_slice(&[pixel[red], pixel[green], pixel[blue], 255]);
        }
    }
    Ok(Frame {
        width: crop.width,
        height: crop.height,
        rgba,
        stream_size: (size.width, size.height),
        crop_origin: (crop.x, crop.y),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_padded_cropped_bgrx_rows() {
        let bytes = [
            2, 3, 4, 0, 5, 6, 7, 0, 99, 99, 99, 99, 8, 9, 10, 0, 11, 12, 13, 0, 99, 99, 99, 99,
        ];
        let size = Rectangle {
            width: 2,
            height: 2,
        };
        let crop = Some(Crop {
            x: 1,
            y: 0,
            width: 1,
            height: 2,
        });
        let frame = copy_pixels(&bytes, 12, size, VideoFormat::BGRx, crop).unwrap();
        assert_eq!((frame.width, frame.height), (1, 2));
        assert_eq!(frame.stream_size, (2, 2));
        assert_eq!(frame.crop_origin, (1, 0));
        assert_eq!(frame.rgba, [7, 6, 5, 255, 13, 12, 11, 255]);
        assert!(copy_pixels(&bytes[..18], 12, size, VideoFormat::BGRx, crop).is_err());
    }
}
