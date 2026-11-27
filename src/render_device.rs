// EGL device selection is independent of GTK's Wayland context.
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    path::{Path, PathBuf},
    ptr,
};

use glow::HasContext;
use libloading::Library;

use crate::session::Result;

type Handle = *mut c_void;
type GetProc = unsafe extern "C" fn(*const c_char) -> *const c_void;
type QueryDevices = unsafe extern "C" fn(c_int, *mut Handle, *mut c_int) -> u32;
type QueryDeviceString = unsafe extern "C" fn(Handle, c_int) -> *const c_char;
type QueryDisplayAttrib = unsafe extern "C" fn(Handle, c_int, *mut isize) -> u32;
type GetCurrentDisplay = unsafe extern "C" fn() -> Handle;
type PlatformDisplay = unsafe extern "C" fn(u32, Handle, *const c_int) -> Handle;
type Initialize = unsafe extern "C" fn(Handle, *mut c_int, *mut c_int) -> u32;
type QueryString = unsafe extern "C" fn(Handle, c_int) -> *const c_char;
type BindApi = unsafe extern "C" fn(u32) -> u32;
type ChooseConfig =
    unsafe extern "C" fn(Handle, *const c_int, *mut Handle, c_int, *mut c_int) -> u32;
type CreateContext = unsafe extern "C" fn(Handle, Handle, Handle, *const c_int) -> Handle;
type CreatePbuffer = unsafe extern "C" fn(Handle, Handle, *const c_int) -> Handle;
type MakeCurrent = unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32;
type Destroy = unsafe extern "C" fn(Handle, Handle) -> u32;
type Terminate = unsafe extern "C" fn(Handle) -> u32;

const EGL_EXTENSIONS: c_int = 0x3055;
const EGL_DEVICE_EXT: c_int = 0x322c;
const EGL_DRM_RENDER_NODE_FILE_EXT: c_int = 0x3377;
const EGL_PLATFORM_DEVICE_EXT: u32 = 0x313f;
const EGL_NONE: c_int = 0x3038;

struct Egl {
    lib: Library,
    get_proc: GetProc,
}

impl Egl {
    fn load() -> Result<Self> {
        unsafe {
            let lib = Library::new("libEGL.so.1")?;
            let get_proc = *lib.get(b"eglGetProcAddress\0")?;
            Ok(Self { lib, get_proc })
        }
    }

    fn core<T: Copy>(&self, name: &CStr) -> Result<T> {
        Ok(unsafe { *self.lib.get(name.to_bytes_with_nul())? })
    }

    fn extension<T: Copy>(&self, name: &CStr) -> Result<T> {
        let address = unsafe { (self.get_proc)(name.as_ptr()) };
        if address.is_null() {
            return Err(format!("{} is unavailable", name.to_string_lossy()).into());
        }
        // EGL's procedure addresses have the same representation as OpenGL's C pointers.
        assert_eq!(size_of::<T>(), size_of::<*const c_void>());
        Ok(unsafe { std::mem::transmute_copy(&address) })
    }

    fn node(&self, device: Handle) -> Option<PathBuf> {
        let query: QueryDeviceString = self.extension(c"eglQueryDeviceStringEXT").ok()?;
        let value = unsafe { query(device, EGL_DRM_RENDER_NODE_FILE_EXT) };
        let path = PathBuf::from(egl_string(value)?);
        path.file_name()?
            .to_str()?
            .strip_prefix("renderD")?
            .parse::<u32>()
            .ok()?;
        (path.parent() == Some(Path::new("/dev/dri"))).then_some(path)
    }

    fn device(&self, display: Handle) -> Result<Handle> {
        let query: QueryDisplayAttrib = self.extension(c"eglQueryDisplayAttribEXT")?;
        let mut value = 0;
        if unsafe { query(display, EGL_DEVICE_EXT, &mut value) } == 0 || value == 0 {
            return Err("EGL could not identify the display device".into());
        }
        Ok(value as Handle)
    }

    fn devices(&self) -> Result<Vec<Device>> {
        let query: QueryDevices = self.extension(c"eglQueryDevicesEXT")?;
        let mut count = 0;
        if unsafe { query(0, ptr::null_mut(), &mut count) } == 0 || !(1..=256).contains(&count) {
            return Ok(Vec::new());
        }
        let mut handles = vec![ptr::null_mut(); count as usize];
        if unsafe { query(count, handles.as_mut_ptr(), &mut count) } == 0 {
            return Err("EGL could not enumerate devices".into());
        }
        handles.truncate(count as usize);
        let mut devices: Vec<_> = handles
            .into_iter()
            .filter(|handle| !handle.is_null())
            .filter_map(|handle| {
                let node = self.node(handle)?;
                Some(Device {
                    id: stable_id(&node)?,
                    node,
                    handle,
                })
            })
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        devices.dedup_by(|a, b| a.id == b.id);
        Ok(devices)
    }
}

fn egl_string(value: *const c_char) -> Option<String> {
    (!value.is_null()).then(|| {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    })
}

fn stable_id(node: &Path) -> Option<String> {
    let device = std::fs::canonicalize(
        Path::new("/sys/class/drm")
            .join(node.file_name()?)
            .join("device"),
    )
    .ok()?;
    // The directory identifies the physical PCI slot or platform device, independently
    // of the order in which renderD nodes were assigned.
    Some(
        device
            .strip_prefix("/sys/devices")
            .ok()?
            .to_str()?
            .to_owned(),
    )
}

struct Device {
    id: String,
    node: PathBuf,
    handle: Handle,
}

pub struct Context {
    gl: Option<glow::Context>,
    display: Handle,
    context: Handle,
    pbuffer: Handle,
    egl: Egl,
    pub node: PathBuf,
    pub label: String,
}

impl Context {
    pub fn new(selection: &str, preferred_node: &str) -> Result<Self> {
        let egl = Egl::load().map_err(|error| report("render_gpu_driver", error))?;
        let devices = egl
            .devices()
            .map_err(|error| report("render_gpu_driver", error))?;
        if !selection.is_empty() {
            let device = devices
                .iter()
                .find(|device| device.id == selection)
                .ok_or("render_gpu_missing")?;
            return Self::open(egl, device)
                .map_err(|error| report("render_gpu_driver", error).into());
        }
        let mut devices = devices;
        if devices.is_empty() {
            return Err("render_gpu_missing".into());
        }
        devices.sort_by_key(|device| device.node.as_os_str() != preferred_node);
        for device in devices {
            match Self::open(Egl::load()?, &device) {
                Ok(context) => return Ok(context),
                Err(error) => diagnostic!("picalens EGL {}: {error}", device.node.display()),
            }
        }
        Err("render_gpu_driver".into())
    }

    fn open(egl: Egl, device: &Device) -> Result<Self> {
        let platform: PlatformDisplay = egl.extension(c"eglGetPlatformDisplayEXT")?;
        let display = unsafe { platform(EGL_PLATFORM_DEVICE_EXT, device.handle, ptr::null()) };
        let instance = Self::open_display(egl, display, device.node.clone())?;
        let actual = instance.egl.device(display)?;
        if actual != device.handle || instance.egl.node(actual).as_ref() != Some(&device.node) {
            return Err("EGL selected a different render node".into());
        }
        let renderer = instance.label.to_ascii_lowercase();
        if ["llvmpipe", "softpipe", "swrast", "software rasterizer"]
            .iter()
            .any(|name| renderer.starts_with(name))
        {
            return Err("selected EGL device uses a software renderer".into());
        }
        Ok(instance)
    }

    fn open_display(egl: Egl, display: Handle, node: PathBuf) -> Result<Self> {
        let initialize: Initialize = egl.core(c"eglInitialize")?;
        let query: QueryString = egl.core(c"eglQueryString")?;
        let bind_api: BindApi = egl.core(c"eglBindAPI")?;
        let choose: ChooseConfig = egl.core(c"eglChooseConfig")?;
        let create: CreateContext = egl.core(c"eglCreateContext")?;
        let create_pbuffer: CreatePbuffer = egl.core(c"eglCreatePbufferSurface")?;
        let make_current: MakeCurrent = egl.core(c"eglMakeCurrent")?;
        if display.is_null()
            || unsafe { initialize(display, ptr::null_mut(), ptr::null_mut()) } == 0
        {
            return Err("could not initialize EGL on this device".into());
        }
        let mut instance = Self {
            gl: None,
            display,
            context: ptr::null_mut(),
            pbuffer: ptr::null_mut(),
            egl,
            node,
            label: String::new(),
        };
        let capabilities =
            egl_string(unsafe { query(display, EGL_EXTENSIONS) }).unwrap_or_default();
        let has = |name| capabilities.split_ascii_whitespace().any(|ext| ext == name);
        if unsafe { bind_api(0x30a2) } == 0 {
            return Err("EGL could not bind desktop OpenGL".into());
        }
        let mut config = ptr::null_mut();
        if !(has("EGL_KHR_no_config_context") && has("EGL_KHR_surfaceless_context")) {
            // EGL_SURFACE_TYPE=PBUFFER_BIT, EGL_RENDERABLE_TYPE=OPENGL_BIT, RGB8.
            let attrs = [
                0x3033, 1, 0x3040, 8, 0x3024, 8, 0x3023, 8, 0x3022, 8, EGL_NONE,
            ];
            let mut count = 0;
            if unsafe { choose(display, attrs.as_ptr(), &mut config, 1, &mut count) } == 0
                || count == 0
            {
                return Err("could not choose an EGL OpenGL configuration".into());
            }
            if !has("EGL_KHR_surfaceless_context") {
                let size = [0x3057, 1, 0x3056, 1, EGL_NONE];
                instance.pbuffer = unsafe { create_pbuffer(display, config, size.as_ptr()) };
                if instance.pbuffer.is_null() {
                    return Err("could not create an EGL pbuffer".into());
                }
            }
        }
        // Request a core OpenGL 4.0 context, matching the existing FSR and SGSR shaders.
        let attrs = [0x3098, 4, 0x30fb, 0, 0x30fd, 1, EGL_NONE];
        instance.context = unsafe { create(display, config, ptr::null_mut(), attrs.as_ptr()) };
        if instance.context.is_null()
            || unsafe {
                make_current(
                    display,
                    instance.pbuffer,
                    instance.pbuffer,
                    instance.context,
                )
            } == 0
        {
            return Err("could not make an EGL OpenGL 4.0 context current".into());
        }
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                let name = CString::new(name).expect("OpenGL function name");
                (instance.egl.get_proc)(name.as_ptr())
            })
        };
        instance.label = unsafe { gl.get_parameter_string(glow::RENDERER) };
        instance.gl = Some(gl);
        Ok(instance)
    }

    pub fn gl(&self) -> &glow::Context {
        self.gl.as_ref().expect("EGL initialized")
    }

    #[cfg(test)]
    pub fn software_test_context() -> Result<Self> {
        let egl = Egl::load()?;
        let platform: PlatformDisplay = egl.extension(c"eglGetPlatformDisplayEXT")?;
        let display = unsafe { platform(0x31dd, ptr::null_mut(), ptr::null()) };
        Self::open_display(egl, display, PathBuf::new())
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.gl.take();
        unsafe {
            let none = ptr::null_mut();
            if !self.context.is_null() {
                if let Ok(current) = self.egl.core::<MakeCurrent>(c"eglMakeCurrent") {
                    current(self.display, none, none, none);
                }
                if let Ok(destroy) = self.egl.core::<Destroy>(c"eglDestroyContext") {
                    destroy(self.display, self.context);
                }
            }
            if !self.pbuffer.is_null()
                && let Ok(destroy) = self.egl.core::<Destroy>(c"eglDestroySurface")
            {
                destroy(self.display, self.pbuffer);
            }
            if let Ok(terminate) = self.egl.core::<Terminate>(c"eglTerminate") {
                terminate(self.display);
            }
        }
    }
}

fn report(key: &'static str, error: impl std::fmt::Display) -> &'static str {
    diagnostic!("picalens EGL: {error}");
    key
}

pub fn list() -> Result<()> {
    let egl = Egl::load()?;
    for device in egl.devices()? {
        if let Ok(context) = Context::open(Egl::load()?, &device) {
            let label = context.label.replace(['\t', '\n', '\r'], " ");
            println!("{}\t{label}", device.id);
        }
    }
    Ok(())
}

pub fn current_node() -> Option<PathBuf> {
    let egl = Egl::load().ok()?;
    let current: GetCurrentDisplay = egl.core(c"eglGetCurrentDisplay").ok()?;
    let display = unsafe { current() };
    if display.is_null() {
        return None;
    }
    egl.node(egl.device(display).ok()?)
}
