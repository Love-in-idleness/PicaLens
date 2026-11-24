use std::{
    cell::{Cell, RefCell},
    io::{self, Cursor},
    mem::size_of,
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
    ptr::NonNull,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use gtk::{gdk, prelude::*};
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
const DRM_FORMAT_MOD_LINEAR: u64 = 0;
// PipeWire raw RGB names describe byte order. DRM FourCC names describe bit order.
const DMA_FORMATS: &[(VideoFormat, u32)] = &[
    (VideoFormat::BGRx, u32::from_le_bytes(*b"XR24")),
    (VideoFormat::BGRA, u32::from_le_bytes(*b"AR24")),
    (VideoFormat::RGBx, u32::from_le_bytes(*b"XB24")),
    (VideoFormat::RGBA, u32::from_le_bytes(*b"AB24")),
];

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
    download: Option<DmaDownload>,
    stopping: bool,
}

pub struct Capture {
    mailbox: Arc<Mutex<Mailbox>>,
    stop: pw::channel::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn take_frame(&self) -> Option<Frame> {
        let (frame, download) = {
            let mut mailbox = self.mailbox.lock().unwrap();
            (mailbox.frame.take(), mailbox.download.take())
        };
        if let Some(download) = download {
            let frame = download.read();
            let _ = download.ready.send(frame);
        }
        frame
    }

    pub fn take_error(&self) -> Option<String> {
        self.mailbox.lock().unwrap().error.take()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        {
            let mut mailbox = self.mailbox.lock().unwrap();
            mailbox.stopping = true;
            mailbox.download.take();
        }
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
    let dma = if crate::session::portal_desktop() {
        DmaPolicy::importable()
    } else if std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .any(|name| name.eq_ignore_ascii_case("gnome"))
    {
        // Mutter renders MemFd window frames at a different scale from VideoCrop.
        DmaPolicy::Linear
    } else {
        DmaPolicy::Off
    };
    let (stop, stop_rx) = pw::channel::channel();
    let worker_mailbox = Arc::clone(&mailbox);
    let worker = thread::Builder::new()
        .name("picalens-capture".into())
        .spawn(move || {
            let result = run(fd, node_id, stop_rx, &worker_mailbox, dma);
            if let Err(error) = result {
                diagnostic!("PipeWire capture stopped: {error}");
                let mut mailbox = worker_mailbox.lock().unwrap();
                mailbox.frame = None;
                mailbox.error = Some(error.to_string());
            } else {
                diagnostic!("PipeWire capture stopped: requested shutdown");
            }
        })?;
    Ok(Capture {
        mailbox,
        stop,
        worker: Some(worker),
    })
}

enum DmaPolicy {
    Off,
    Linear,
    Import(Vec<(VideoFormat, Vec<u64>)>),
}

impl DmaPolicy {
    fn importable() -> Self {
        let Some(display) = gdk::Display::default() else {
            return Self::Off;
        };
        let formats = display.dmabuf_formats();
        let imported = DMA_FORMATS
            .iter()
            .filter_map(|&(format, fourcc)| {
                let modifiers = (0..formats.n_formats())
                    .map(|index| formats.format(index))
                    .filter_map(|(code, modifier)| (code == fourcc).then_some(modifier))
                    .collect::<Vec<_>>();
                (!modifiers.is_empty()).then_some((format, modifiers))
            })
            .collect::<Vec<_>>();
        diagnostic!("PipeWire DMA-BUF import format count: {}", imported.len());
        Self::Import(imported)
    }

    fn supports(&self, format: VideoFormat, modifier: u64) -> bool {
        match self {
            Self::Off => false,
            Self::Linear => modifier == DRM_FORMAT_MOD_LINEAR,
            Self::Import(formats) => formats
                .iter()
                .any(|(kind, modifiers)| *kind == format && modifiers.contains(&modifier)),
        }
    }
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
    dma: DmaPolicy,
) -> Result<(), Box<dyn std::error::Error>> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let _stop = stop.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |_| mainloop.quit()
    });
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;

    let failure = Rc::new(RefCell::new(None));
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

    let retry = run_stream(&core, &mainloop, &fail, &failure, node_id, mailbox, dma)?;
    if retry && failure.borrow().is_none() {
        run_stream(
            &core,
            &mainloop,
            &fail,
            &failure,
            node_id,
            mailbox,
            DmaPolicy::Off,
        )?;
    }
    let result = failure.borrow_mut().take();
    result.map_or(Ok(()), |message| Err(message.into()))
}

fn run_stream(
    core: &pw::core::Core,
    mainloop: &pw::main_loop::MainLoopRc,
    fail: &Rc<impl Fn(String) + 'static>,
    failure: &RefCell<Option<String>>,
    node_id: u32,
    mailbox: &Arc<Mutex<Mailbox>>,
    dma: DmaPolicy,
) -> Result<bool, Box<dyn std::error::Error>> {
    let retry = Rc::new(Cell::new(false));
    let stream = pw::stream::StreamBox::new(
        core,
        "picalens-probe",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let prefer_linear = matches!(dma, DmaPolicy::Linear);
    let import = matches!(dma, DmaPolicy::Import(_));
    let mem_buffers = serialize(buffer_param(false, true))?;
    let dma_buffers = serialize(buffer_param(true, !import))?;
    let crop_meta = serialize(crop_param())?;
    let mut format_choices = match &dma {
        DmaPolicy::Off => Vec::new(),
        DmaPolicy::Linear => vec![serialize(format_param(true))?],
        DmaPolicy::Import(formats) => formats
            .iter()
            .map(|(format, modifiers)| serialize(dmabuf_format_param(*format, modifiers)))
            .collect::<io::Result<Vec<_>>>()?,
    };
    format_choices.push(serialize(format_param(false))?);
    let listener = stream.add_local_listener_with_user_data(StreamData {
        format: None,
        frames: 0,
        last_report: Instant::now(),
        layout: None,
    })
    .state_changed({
        let fail = Rc::clone(fail);
        move |_, _, old, new| {
            diagnostic!("PipeWire stream state: {old:?} -> {new:?}");
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
        let fail = Rc::clone(fail);
        let mailbox = Arc::clone(mailbox);
        let dma_policy = dma;
        move |stream, state, id, pod| {
            if id != ParamType::Format.as_raw() {
                return;
            }
            state.format = None;
            state.layout = None;
            mailbox.lock().unwrap().frame = None;
            let Some(pod) = pod else { return };
            match negotiated_format(pod).and_then(|format| {
                if format.flags().bits() & sys::SPA_VIDEO_FLAG_MODIFIER_FIXATION_REQUIRED != 0 {
                    // The producer will send a new format after fixing the modifier.
                    return Ok(None);
                }
                let dma = format.flags().bits() & sys::SPA_VIDEO_FLAG_MODIFIER != 0;
                if dma && !dma_policy.supports(format.format(), format.modifier()) {
                    return Err(format!("unsupported DMA-BUF modifier {}", format.modifier()));
                }
                let buffers = if dma { &dma_buffers } else { &mem_buffers };
                stream.update_params(&mut [pod_bytes(buffers), pod_bytes(&crop_meta)])
                    .map_err(|error| format!("negotiate PipeWire buffers: {error}"))?;
                Ok(Some(format))
            }) {
                Ok(Some(format)) => {
                    let Rectangle { width, height } = format.size();
                    diagnostic!("PipeWire format: {:?}, {width}x{height}, {}/{} Hz",
                        format.format(), format.framerate().num, format.framerate().denom);
                    state.format = Some(format);
                }
                Ok(None) => {}
                Err(error) => fail(error),
            }
        }
    })
    .process({
        let fail = Rc::clone(fail);
        let mailbox = Arc::clone(mailbox);
        let mainloop = mainloop.clone();
        let retry = Rc::clone(&retry);
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
            match newest.copy_frame(&format, import.then_some(&mailbox)) {
                Ok((frame, layout)) => {
                    let report = state.frames == 0 || state.last_report.elapsed() >= Duration::from_secs(1);
                    if state.layout.is_none() || (report && state.layout != Some(layout)) {
                        diagnostic!("PipeWire buffer: {:?}, stride={}, offset={}, crop={:?}, cursor=hidden",
                            layout.kind, layout.stride, layout.offset, layout.crop);
                        state.layout = Some(layout);
                    }
                    state.frames += 1;
                    if report {
                        diagnostic!("PipeWire frames received: {}", state.frames);
                        state.last_report = Instant::now();
                    }
                    // No PipeWire memory leaves the worker or remains mapped by the UI.
                    drop(newest);
                    mailbox.lock().unwrap().frame = Some(frame);
                }
                Err(error) if (prefer_linear || import)
                    && newest.is_dmabuf()
                    && !mailbox.lock().unwrap().stopping => {
                    diagnostic!("Could not read DMA-BUF: {error}; retrying with shared memory");
                    retry.set(true);
                    mainloop.quit();
                }
                Err(error) => {
                    if !mailbox.lock().unwrap().stopping {
                        fail(error);
                    }
                }
            }
        }
    })
    .register()?;

    let mut params: Vec<_> = format_choices
        .iter()
        .map(|bytes| pod_bytes(bytes))
        .collect();
    stream.connect(
        Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::DONT_RECONNECT,
        &mut params,
    )?;
    diagnostic!("PipeWire remote capture requested for node {node_id}");
    if failure.borrow().is_none() {
        mainloop.run();
    }
    drop(listener);
    Ok(retry.get())
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

fn format_param(linear: bool) -> Object {
    let formats = if linear {
        vec![VideoFormat::BGRA, VideoFormat::BGRx]
    } else {
        vec![
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
            VideoFormat::xRGB,
            VideoFormat::ARGB,
            VideoFormat::xBGR,
            VideoFormat::ABGR,
            VideoFormat::RGB,
            VideoFormat::BGR,
        ]
    };
    let format = property(
        FormatProperties::VideoFormat.as_raw(),
        Value::Choice(spa::pod::ChoiceValue::Id(spa::utils::Choice(
            spa::utils::ChoiceFlags::empty(),
            spa::utils::ChoiceEnum::Enum {
                default: Id(formats[0].as_raw()),
                alternatives: formats
                    .into_iter()
                    .map(|format| Id(format.as_raw()))
                    .collect(),
            },
        ))),
    );
    let mut param = spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        format,
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
    );
    if linear {
        param.properties.push(Property {
            key: FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY,
            value: Value::Long(DRM_FORMAT_MOD_LINEAR as i64),
        });
    }
    param
}

fn property(key: u32, value: Value) -> Property {
    Property {
        key,
        flags: PropertyFlags::empty(),
        value,
    }
}

fn dmabuf_format_param(format: VideoFormat, modifiers: &[u64]) -> Object {
    let mut param = format_param(false);
    let kind = param
        .properties
        .iter_mut()
        .find(|prop| prop.key == FormatProperties::VideoFormat.as_raw())
        .unwrap();
    kind.value = Value::Id(Id(format.as_raw()));
    param.properties.push(Property {
        key: FormatProperties::VideoModifier.as_raw(),
        flags: PropertyFlags::MANDATORY
            | PropertyFlags::from_bits_retain(sys::SPA_POD_PROP_FLAG_DONT_FIXATE),
        value: Value::Choice(spa::pod::ChoiceValue::Long(spa::utils::Choice(
            spa::utils::ChoiceFlags::empty(),
            spa::utils::ChoiceEnum::Enum {
                default: modifiers[0] as i64,
                alternatives: modifiers.iter().map(|&modifier| modifier as i64).collect(),
            },
        ))),
    });
    param
}

fn buffer_param(dma: bool, single_plane: bool) -> Object {
    let data_types = if dma {
        1 << sys::SPA_DATA_DmaBuf
    } else {
        (1 << sys::SPA_DATA_MemFd) | (1 << sys::SPA_DATA_MemPtr)
    };
    let mut param = Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![property(
            sys::SPA_PARAM_BUFFERS_dataType,
            Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                spa::utils::ChoiceFlags::empty(),
                spa::utils::ChoiceEnum::Flags {
                    default: data_types,
                    flags: vec![],
                },
            ))),
        )],
    };
    if single_plane {
        param
            .properties
            .push(property(sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)));
    }
    param
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

struct DmaPlane {
    fd: OwnedFd,
    stride: u32,
    offset: u32,
}

struct DmaDownload {
    planes: Vec<DmaPlane>,
    modifier: u64,
    fourcc: u32,
    size: Rectangle,
    crop: Option<Crop>,
    ready: mpsc::Sender<Result<Frame, String>>,
}

impl DmaDownload {
    fn read(&self) -> Result<Frame, String> {
        let display = gdk::Display::default().ok_or("missing Wayland display for DMA-BUF")?;
        let mut builder = gdk::DmabufTextureBuilder::new()
            .set_display(&display)
            .set_fourcc(self.fourcc)
            .set_modifier(self.modifier)
            .set_n_planes(self.planes.len() as u32)
            .set_width(self.size.width)
            .set_height(self.size.height);
        for (index, plane) in self.planes.iter().enumerate() {
            let index = index as u32;
            // Plane fds stay open until after the texture and downloader have been dropped.
            builder = unsafe { builder.set_fd(index, plane.fd.as_raw_fd()) }
                .set_offset(index, plane.offset)
                .set_stride(index, plane.stride);
        }
        let texture =
            unsafe { builder.build() }.map_err(|error| format!("import DMA-BUF: {error}"))?;
        let mut download = gdk::TextureDownloader::new(&texture);
        download.set_format(gdk::MemoryFormat::R8g8b8a8Premultiplied);
        let (bytes, stride) = download.download_bytes();
        copy_pixels(
            &bytes,
            stride as i32,
            self.size,
            VideoFormat::RGBA,
            self.crop,
        )
    }
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

    fn planes(&self) -> Option<&[sys::spa_data]> {
        let buffer = self.spa_buffer()?;
        if !(1..=4).contains(&buffer.n_datas) || buffer.datas.is_null() {
            return None;
        }
        // The buffer remains dequeued for the lifetime of this slice.
        Some(unsafe { std::slice::from_raw_parts(buffer.datas, buffer.n_datas as usize) })
    }

    fn is_dmabuf(&self) -> bool {
        self.planes()
            .is_some_and(|planes| DataType::from_raw(planes[0].type_) == DataType::DmaBuf)
    }

    fn has_frame(&self) -> Result<bool, String> {
        let plane = &self.planes().ok_or("missing PipeWire RGB planes")?[0];
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

    fn copy_frame(
        &self,
        format: &VideoInfoRaw,
        import: Option<&Arc<Mutex<Mailbox>>>,
    ) -> Result<(Frame, FrameLayout), String> {
        let planes = self.planes().ok_or("missing PipeWire RGB planes")?;
        let plane = &planes[0];
        let kind = DataType::from_raw(plane.type_);
        let chunk = unsafe { plane.chunk.as_ref() }.ok_or("missing PipeWire chunk")?;
        let crop = self.crop()?;
        let layout = FrameLayout {
            kind,
            stride: chunk.stride,
            offset: chunk.offset,
            crop,
        };
        if kind == DataType::DmaBuf
            && let Some(mailbox) = import
        {
            let frame = download_dmabuf(mailbox, planes, format, crop)?;
            return Ok((frame, layout));
        }
        if planes.len() != 1 {
            return Err("expected one packed RGB plane".into());
        }
        let capacity = plane.maxsize as usize;
        if capacity == 0 || capacity > isize::MAX as usize {
            return Err(format!("invalid PipeWire buffer capacity ({kind:?})"));
        }
        let offset = chunk.offset as usize % capacity;
        let length = (chunk.size as usize).min(capacity);
        if length > capacity - offset {
            return Err("non-contiguous PipeWire frame".into());
        }
        let copy =
            |bytes: &[u8]| copy_pixels(bytes, chunk.stride, format.size(), format.format(), crop);
        let frame = match kind {
            DataType::DmaBuf if format.modifier() == DRM_FORMAT_MOD_LINEAR => {
                let fd = libc::c_int::try_from(plane.fd).map_err(|_| "invalid DMA-BUF fd")?;
                let start = (plane.mapoffset as usize)
                    .checked_add(offset)
                    .ok_or("DMA-BUF offset overflow")?;
                copy_linear_dmabuf(fd, start, length, copy)?
            }
            DataType::MemFd | DataType::MemPtr if !plane.data.is_null() => {
                // MAP_BUFFERS keeps this view mapped until the packet is returned to PipeWire.
                let bytes = unsafe {
                    std::slice::from_raw_parts(plane.data.cast::<u8>().add(offset), length)
                };
                copy(bytes)?
            }
            _ => {
                return Err(format!(
                    "unsupported or unmapped PipeWire buffer ({kind:?})"
                ));
            }
        };
        Ok((frame, layout))
    }
}

fn download_dmabuf(
    mailbox: &Arc<Mutex<Mailbox>>,
    data: &[sys::spa_data],
    format: &VideoInfoRaw,
    crop: Option<Crop>,
) -> Result<Frame, String> {
    let fourcc = DMA_FORMATS
        .iter()
        .find_map(|&(kind, code)| (kind == format.format()).then_some(code))
        .ok_or("unsupported DMA-BUF pixel format")?;
    let mut planes = Vec::with_capacity(data.len());
    for plane in data {
        if DataType::from_raw(plane.type_) != DataType::DmaBuf {
            return Err("expected a DMA-BUF image plane".into());
        }
        let chunk = unsafe { plane.chunk.as_ref() }.ok_or("missing DMA-BUF chunk")?;
        let fd = libc::c_int::try_from(plane.fd).map_err(|_| "invalid DMA-BUF fd")?;
        if fd < 0 {
            return Err("missing DMA-BUF fd".into());
        }
        // Convert the PipeWire-owned fd to a separate reference for the GTK thread.
        let fd = unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        let stride = u32::try_from(chunk.stride)
            .ok()
            .filter(|&stride| stride != 0)
            .ok_or("invalid DMA-BUF stride")?;
        let offset = plane
            .mapoffset
            .checked_add(chunk.offset)
            .ok_or("DMA-BUF offset overflow")?;
        planes.push(DmaPlane { fd, stride, offset });
    }
    let (ready, response) = mpsc::channel();
    {
        let mut mailbox = mailbox.lock().unwrap();
        if mailbox.stopping {
            return Err("capture stopped".into());
        }
        mailbox.download = Some(DmaDownload {
            planes,
            modifier: format.modifier(),
            fourcc,
            size: format.size(),
            crop,
            ready,
        });
    }
    // The producer must not reuse this buffer until GTK has made an independent copy.
    // Dropping Capture on the GTK thread cancels a request it has not started.
    response
        .recv()
        .map_err(|_| "DMA-BUF download cancelled".to_owned())?
}

fn copy_linear_dmabuf(
    fd: libc::c_int,
    offset: usize,
    length: usize,
    copy: impl FnOnce(&[u8]) -> Result<Frame, String>,
) -> Result<Frame, String> {
    let end = offset
        .checked_add(length)
        .filter(|&end| end <= isize::MAX as usize)
        .ok_or("DMA-BUF range overflow")?;
    // DMA-BUFs expose their size via SEEK_END. Avoid a mapping that would fault on read.
    let size = unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
    if usize::try_from(size).is_err() || end > size as usize || length == 0 {
        return Err("DMA-BUF frame is outside the exported buffer".into());
    }
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            end,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if mapping == libc::MAP_FAILED {
        return Err(format!("map DMA-BUF: {}", io::Error::last_os_error()));
    }

    let result = sync_dmabuf(fd, 1).and_then(|()| {
        // The PipeWire packet remains dequeued until this CPU access ends.
        let bytes = unsafe { std::slice::from_raw_parts(mapping.cast::<u8>().add(offset), length) };
        let frame = copy(bytes);
        let sync = sync_dmabuf(fd, 1 | (1 << 2));
        frame.and_then(|frame| sync.map(|()| frame))
    });
    unsafe { libc::munmap(mapping, end) };
    result
}

fn sync_dmabuf(fd: libc::c_int, flags: u64) -> Result<(), String> {
    // DMA_BUF_IOCTL_SYNC from linux/dma-buf.h, with DMA_BUF_SYNC_READ on both calls.
    let request = libc::_IOW::<u64>(b'b' as u32, 0);
    loop {
        if unsafe { libc::ioctl(fd, request, &flags) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) {
            return Err(format!("synchronize DMA-BUF: {error}"));
        }
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

    #[test]
    fn offers_driver_modifiers_and_allows_auxiliary_planes() {
        let layout = [DRM_FORMAT_MOD_LINEAR, 0x0100_0000_0000_0002];
        let video = dmabuf_format_param(VideoFormat::BGRx, &layout);
        let modifier = video
            .properties
            .iter()
            .find(|prop| prop.key == FormatProperties::VideoModifier.as_raw())
            .unwrap();
        assert_ne!(
            modifier.flags.bits() & sys::SPA_POD_PROP_FLAG_DONT_FIXATE,
            0
        );
        let Value::Choice(spa::pod::ChoiceValue::Long(spa::utils::Choice(
            _,
            spa::utils::ChoiceEnum::Enum { alternatives, .. },
        ))) = &modifier.value
        else {
            panic!("expected DMA-BUF modifier choices");
        };
        assert_eq!(alternatives, &layout.map(|modifier| modifier as i64));
        assert!(
            !buffer_param(true, false)
                .properties
                .iter()
                .any(|prop| { prop.key == sys::SPA_PARAM_BUFFERS_blocks })
        );
    }
}
