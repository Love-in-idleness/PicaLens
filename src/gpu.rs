use std::{ffi::CString, ptr, rc::Rc};

use glow::HasContext;
use gtk::{gdk, glib, prelude::*};

use crate::{
    session::Result,
    settings::Filter,
    upscale::{Pixels, Size},
};

const VERTEX: &str = r#"#version 400 core
void main() {
    vec2 corner = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 400 core
uniform sampler2D inputImage;
uniform vec2 outputSize;
out vec4 outputColor;
"#;

const FSR: &str = include_str!("shaders/fsr1.glsl");
const SGSR: &str = include_str!("shaders/sgsr1.glsl");

const EASU_PASS: &str = include_str!("shaders/easu-pass.glsl");

const RCAS_PASS: &str = include_str!("shaders/rcas-pass.glsl");

const SGSR_PASS: &str = include_str!("shaders/sgsr-pass.glsl");

pub(crate) fn filter_sources(filter: Filter) -> Vec<String> {
    match filter {
        Filter::Fsr => vec![
            [FRAGMENT, "#define FSR_EASU_F\n", FSR, EASU_PASS].concat(),
            [FRAGMENT, "#define FSR_RCAS_F\n", FSR, RCAS_PASS].concat(),
        ],
        Filter::Sgsr => vec![[FRAGMENT, SGSR, SGSR_PASS].concat()],
        _ => Vec::new(),
    }
}

pub struct GpuUpscaler {
    filter: Filter,
    pipeline: Option<Pipeline>,
    latest: Option<Pixels>,
    rendered: Option<Size>,
}

impl GpuUpscaler {
    pub fn new(filter: Filter) -> Self {
        Self {
            filter,
            pipeline: None,
            latest: None,
            rendered: None,
        }
    }

    pub fn set_source(&mut self, pixels: Pixels) {
        self.latest = Some(pixels);
        self.rendered = None;
    }

    pub fn update(
        &mut self,
        display: &gdk::Display,
        target: Option<Size>,
    ) -> Result<Option<(Size, gdk::Texture)>> {
        let (Some(source), Some(target)) = (&self.latest, target) else {
            return Ok(None);
        };
        if self.rendered == Some(target) {
            return Ok(None);
        }
        if self.pipeline.is_none() {
            self.pipeline = Some(Pipeline::new(display, self.filter)?);
        }
        let output = self.pipeline.as_mut().unwrap().render(source, target)?;
        self.rendered = Some(target);
        Ok(Some((source.size, output)))
    }
}

struct Gl {
    context: gdk::GLContext,
    functions: glow::Context,
    _egl: libloading::Library,
}

struct Current(Option<gdk::GLContext>);

impl Current {
    fn make(context: &gdk::GLContext) -> Self {
        let previous = gdk::GLContext::current();
        context.make_current();
        Self(previous)
    }
}

impl Drop for Current {
    fn drop(&mut self) {
        if let Some(context) = &self.0 {
            context.make_current();
        } else {
            gdk::GLContext::clear_current();
        }
    }
}

pub fn current_render_node(display: &gdk::Display) -> Option<std::path::PathBuf> {
    let context = display.create_gl_context().ok()?;
    context.realize().ok()?;
    let _current = Current::make(&context);
    crate::render_device::current_node()
}

struct Pipeline {
    gl: Rc<Gl>,
    programs: Vec<glow::NativeProgram>,
    framebuffer: Option<glow::NativeFramebuffer>,
    vertex_array: Option<glow::NativeVertexArray>,
    input: Option<glow::NativeTexture>,
    input_size: Option<Size>,
    intermediate: Option<glow::NativeTexture>,
    intermediate_size: Option<Size>,
    limit: u32,
}

impl Pipeline {
    fn new(display: &gdk::Display, filter: Filter) -> Result<Self> {
        let context = display.create_gl_context()?;
        context.set_allowed_apis(gdk::GLAPI::GL);
        context.set_required_version(4, 0);
        context.realize()?;
        let _current = Current::make(&context);
        if gdk::GLContext::current().as_ref() != Some(&context) {
            return Err("could not make the preview OpenGL context current".into());
        }

        // Wayland GDK uses EGL. Keep the library loaded for the lifetime of its pointers.
        let (egl, functions) = unsafe {
            type GetProc =
                unsafe extern "C" fn(*const libc::c_char) -> Option<unsafe extern "C" fn()>;
            let egl = libloading::Library::new("libEGL.so.1")?;
            let get_proc = egl.get::<GetProc>(b"eglGetProcAddress\0")?;
            let functions = glow::Context::from_loader_function(|name| {
                let name = CString::new(name).expect("OpenGL function name");
                get_proc(name.as_ptr()).map_or(ptr::null(), |proc| proc as *const _)
            });
            (egl, functions)
        };
        let limit = unsafe { functions.get_parameter_i32(glow::MAX_TEXTURE_SIZE) } as u32;
        let mut pipeline = Self {
            gl: Rc::new(Gl {
                context,
                functions,
                _egl: egl,
            }),
            programs: Vec::new(),
            framebuffer: None,
            vertex_array: None,
            input: None,
            input_size: None,
            intermediate: None,
            intermediate_size: None,
            limit,
        };
        let gl = &pipeline.gl.functions;
        for source in filter_sources(filter) {
            pipeline.programs.push(program(gl, &source)?);
        }
        if pipeline.programs.is_empty() {
            return Err("no GPU shader for this filter".into());
        }
        // All calls use the private, current context. GTK/GSK only sees the finished output.
        unsafe {
            pipeline.framebuffer = Some(gl.create_framebuffer()?);
            pipeline.vertex_array = Some(gl.create_vertex_array()?);
        }
        pipeline.input = Some(texture(gl)?);
        if pipeline.programs.len() == 2 {
            pipeline.intermediate = Some(texture(gl)?);
        }
        diagnostic!(
            "picalens preview GPU filter={} renderer={}",
            filter.id(),
            unsafe { gl.get_parameter_string(glow::RENDERER) }
        );
        Ok(pipeline)
    }

    fn render(&mut self, source: &Pixels, target: Size) -> Result<gdk::Texture> {
        let (w, h) = target;
        if [w, h, source.size.0, source.size.1]
            .into_iter()
            .any(|edge| edge > self.limit)
        {
            return Err("preview texture exceeds the OpenGL size limit".into());
        }
        let _current = Current::make(&self.gl.context);
        let gl = &self.gl.functions;
        let input = self.input.unwrap();
        let mut output = Output {
            gl: self.gl.clone(),
            id: texture(gl)?,
            fence: None,
        };
        // The output is a fresh texture because GTK can still be sampling a previous frame.
        unsafe {
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(input));
            let (sw, sh) = (source.size.0 as i32, source.size.1 as i32);
            let pixels = glow::PixelUnpackData::Slice(Some(&source.bytes));
            if self.input_size == Some(source.size) {
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    sw,
                    sh,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    pixels,
                );
            } else {
                storage(gl, source.size, Some(&source.bytes));
                self.input_size = Some(source.size);
            }

            gl.bind_texture(glow::TEXTURE_2D, Some(output.id));
            storage(gl, target, None);
            gl.bind_vertex_array(self.vertex_array);
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            gl.viewport(0, 0, w as i32, h as i32);
            let rendered = if let Some(intermediate) = self.intermediate {
                if self.intermediate_size != Some(target) {
                    gl.bind_texture(glow::TEXTURE_2D, Some(intermediate));
                    storage(gl, target, None);
                    self.intermediate_size = Some(target);
                }
                pass(gl, self.programs[0], input, intermediate, target)
                    .and_then(|()| pass(gl, self.programs[1], intermediate, output.id, target))
            } else {
                pass(gl, self.programs[0], input, output.id, target)
            };
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                None,
                0,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            rendered?;
            let error = gl.get_error();
            if error != glow::NO_ERROR {
                return Err(format!("OpenGL upscaling error {error:#x}").into());
            }
            let fence = gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0)?;
            output.fence = Some(fence);
            gl.flush();

            let builder = gdk::GLTextureBuilder::new()
                .set_context(Some(&self.gl.context))
                .set_id(output.id.0.get())
                .set_sync(Some(fence.0.cast()))
                .set_width(w as i32)
                .set_height(h as i32)
                .set_format(gdk::MemoryFormat::R8g8b8a8);
            let output = glib::thread_guard::ThreadGuard::new(output);
            Ok(builder.build_with_release_func(move || {
                // A render thread can drop the texture. Delete its GL objects on the GTK thread.
                glib::idle_add_once(move || drop(output.into_inner()));
            }))
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _current = Current::make(&self.gl.context);
        let gl = &self.gl.functions;
        unsafe {
            for program in self.programs.drain(..) {
                gl.delete_program(program);
            }
            for texture in [self.input, self.intermediate].into_iter().flatten() {
                gl.delete_texture(texture);
            }
            if let Some(framebuffer) = self.framebuffer {
                gl.delete_framebuffer(framebuffer);
            }
            if let Some(vao) = self.vertex_array {
                gl.delete_vertex_array(vao);
            }
        }
    }
}

struct Output {
    gl: Rc<Gl>,
    id: glow::NativeTexture,
    fence: Option<glow::NativeFence>,
}

impl Drop for Output {
    fn drop(&mut self) {
        let _current = Current::make(&self.gl.context);
        let gl = &self.gl.functions;
        unsafe {
            if let Some(fence) = self.fence {
                gl.delete_sync(fence);
            }
            gl.delete_texture(self.id);
        }
    }
}

pub(crate) fn program(gl: &glow::Context, fragment: &str) -> Result<glow::NativeProgram> {
    unsafe {
        let program = gl.create_program()?;
        let result = (|| -> Result<()> {
            for (kind, source) in [
                (glow::VERTEX_SHADER, VERTEX),
                (glow::FRAGMENT_SHADER, fragment),
            ] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let info = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    return Err(info.into());
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program).into());
            }
            Ok(())
        })();
        if let Err(error) = result {
            gl.delete_program(program);
            return Err(error);
        }
        Ok(program)
    }
}

pub(crate) fn texture(gl: &glow::Context) -> Result<glow::NativeTexture> {
    unsafe {
        let id = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(id));
        for (param, value) in [
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, param, value as i32);
        }
        Ok(id)
    }
}

unsafe fn storage(gl: &glow::Context, (w, h): Size, pixels: Option<&[u8]>) {
    unsafe {
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(pixels),
        );
    }
}

unsafe fn pass(
    gl: &glow::Context,
    program: glow::NativeProgram,
    source: glow::NativeTexture,
    target: glow::NativeTexture,
    (w, h): Size,
) -> Result<()> {
    unsafe {
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(target),
            0,
        );
        if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            return Err("incomplete upscaler framebuffer".into());
        }
        gl.use_program(Some(program));
        let size = gl.get_uniform_location(program, "outputSize");
        gl.uniform_2_f32(size.as_ref(), w as f32, h as f32);
        gl.bind_texture(glow::TEXTURE_2D, Some(source));
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
        Ok(())
    }
}
