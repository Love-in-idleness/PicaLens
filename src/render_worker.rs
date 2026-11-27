use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{self, BufRead, Write},
    os::{fd::AsRawFd, unix::fs::FileExt},
    path::Path,
};

use glow::HasContext;

use crate::{
    gpu, render_device,
    session::Result,
    settings::Filter,
    upscale::{MAX_EDGE, MAX_PIXELS, Size},
};

pub const WORKER: &str = "/usr/lib/picalens/picalens-render";
pub const FRAME_BYTES: usize = MAX_PIXELS as usize * 4;

const CLAMP: &str = include_str!("shaders/anime4k/clamp.glsl");
const RESTORE: &str = include_str!("shaders/anime4k/restore-s.glsl");
const UPSCALE: &str = include_str!("shaders/anime4k/upscale-x2-s.glsl");
const PRESENT: &str = r#"#version 400 core
uniform sampler2D image;
uniform sampler2D original;
uniform vec2 outputSize;
out vec4 outputColor;
void main() {
    vec2 uv = gl_FragCoord.xy / outputSize;
    outputColor = vec4(texture(image, uv).rgb, texture(original, uv).a);
}
"#;

pub fn run() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [mode] if mode == "--list" => render_device::list(),
        [mode, filter, device, hint, shared, orientation] if mode == "--run" => serve(
            Filter::parse(filter.to_str().unwrap_or_default()),
            device.to_str().unwrap_or_default(),
            hint.to_str().unwrap_or_default(),
            Path::new(shared),
            orientation == "flip",
        ),
        _ => Err("invalid render worker arguments".into()),
    };
    if let Err(error) = result {
        diagnostic!("picalens render worker: {error}");
        let reason = match error.to_string().as_str() {
            "render_gpu_missing" => "render_gpu_missing",
            "render_gpu_driver" => "render_gpu_driver",
            "render_size" => "render_size",
            _ => "render_worker_failed",
        };
        let _ = writeln!(io::stdout().lock(), "error {reason}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn serve(filter: Filter, device: &str, hint: &str, shared: &Path, flip: bool) -> Result<()> {
    let file = OpenOptions::new().read(true).write(true).open(shared)?;
    if file.metadata()?.len() != 2 * FRAME_BYTES as u64 || !filter.uses_gpu() {
        return Err("invalid render worker configuration".into());
    }
    let context = render_device::Context::new(device, hint)?;
    let mut renderer = Renderer::new(context.gl(), filter)?;
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    diagnostic!(
        "picalens render GPU filter={} device={} node={} renderer={}",
        filter.id(),
        if device.is_empty() { "auto" } else { device },
        context.node.display(),
        context.label
    );
    writeln!(stdout, "ready")?;
    stdout.flush()?;
    let mut input = Vec::new();
    let mut output = Vec::new();
    let mut request = String::new();
    let mut control = libc::pollfd {
        fd: stdin.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        if !context.node.exists() {
            return Err("render_gpu_missing".into());
        }
        // Watch for unplug even when the source is idle and submits no frames.
        let readable = unsafe { libc::poll(&mut control, 1, 500) };
        if readable == 0 {
            continue;
        }
        if readable < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        request.clear();
        if stdin.read_line(&mut request)? == 0 {
            break;
        }
        let mut fields = request.split_ascii_whitespace().map(str::parse::<u32>);
        let [
            Some(Ok(w)),
            Some(Ok(h)),
            Some(Ok(out_w)),
            Some(Ok(out_h)),
            None,
        ] = std::array::from_fn(|_| fields.next())
        else {
            return Err("invalid render frame request".into());
        };
        let source = (w, h);
        let target = (out_w, out_h);
        input.resize(frame_len(source)?, 0);
        output.resize(frame_len(target)?, 0);
        file.read_exact_at(&mut input, 0)?;
        if flip && filter == Filter::Anime4k {
            flip_rows(&mut input, source.0);
        }
        renderer.render(source, target, &input, &mut output)?;
        if flip && filter == Filter::Anime4k {
            flip_rows(&mut output, target.0);
        }
        file.write_all_at(&output, FRAME_BYTES as u64)?;
        writeln!(stdout, "frame")?;
        stdout.flush()?;
    }
    Ok(())
}

fn flip_rows(bytes: &mut [u8], width: u32) {
    let stride = width as usize * 4;
    let rows = bytes.len() / stride;
    let (top, bottom) = bytes.split_at_mut(rows.div_ceil(2) * stride);
    for (first, last) in top
        .chunks_exact_mut(stride)
        .zip(bottom.rchunks_exact_mut(stride))
    {
        first.swap_with_slice(last);
    }
}

pub fn frame_len((w, h): Size) -> Result<usize> {
    let pixels = u64::from(w) * u64::from(h);
    if pixels == 0 || pixels > MAX_PIXELS || w.max(h) > MAX_EDGE {
        return Err("render_size".into());
    }
    Ok(pixels as usize * 4)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Format {
    Rgba8,
    R16f,
    Rgba16f,
}

impl Format {
    fn internal(self) -> u32 {
        match self {
            Self::Rgba8 => glow::RGBA8,
            Self::R16f => glow::R16F,
            Self::Rgba16f => glow::RGBA16F,
        }
    }
}

struct Texture {
    id: glow::NativeTexture,
    size: Size,
    format: Format,
}

struct Pass {
    program: glow::NativeProgram,
    bindings: Vec<&'static str>,
    save: &'static str,
    format: Format,
    doubled: bool,
    conditional: bool,
}

struct Renderer<'a> {
    gl: &'a glow::Context,
    passes: Vec<Pass>,
    textures: Vec<Texture>,
    framebuffer: Option<glow::NativeFramebuffer>,
    vao: Option<glow::NativeVertexArray>,
    limit: u32,
    filter: Filter,
}

impl<'a> Renderer<'a> {
    fn new(gl: &'a glow::Context, filter: Filter) -> Result<Self> {
        let mut renderer = Self {
            gl,
            filter,
            passes: Vec::new(),
            textures: Vec::new(),
            framebuffer: None,
            vao: None,
            limit: unsafe { gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) } as u32,
        };
        unsafe {
            renderer.framebuffer = Some(gl.create_framebuffer()?);
            renderer.vao = Some(gl.create_vertex_array()?);
        }
        if filter == Filter::Anime4k {
            // PREKERNEL runs after MAIN in mpv. Measure highlights on the original,
            // then clamp the restored/upscaled image immediately before resampling.
            let clamp: Vec<_> = shader_sections(CLAMP).collect();
            for section in clamp[..2]
                .iter()
                .copied()
                .chain(shader_sections(RESTORE))
                .chain(shader_sections(UPSCALE))
                .chain(clamp[2..].iter().copied())
            {
                renderer.passes.push(anime_pass(gl, section)?);
            }
            renderer.passes.push(Pass {
                program: gpu::program(gl, PRESENT)?,
                bindings: vec!["MAIN", "original"],
                save: "output",
                format: Format::Rgba8,
                doubled: false,
                conditional: false,
            });
        } else {
            for source in gpu::filter_sources(filter) {
                renderer.passes.push(Pass {
                    program: gpu::program(gl, &source)?,
                    bindings: vec!["MAIN"],
                    save: "MAIN",
                    format: Format::Rgba8,
                    doubled: false,
                    conditional: false,
                });
            }
        }
        Ok(renderer)
    }

    fn render(
        &mut self,
        source: Size,
        target: Size,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<()> {
        let anime = self.filter == Filter::Anime4k;
        let double = 5 * u64::from(target.0) > 6 * u64::from(source.0)
            && 5 * u64::from(target.1) > 6 * u64::from(source.1);
        let double_size = (source.0 * 2, source.1 * 2);
        self.check_size(source)?;
        self.check_size(target)?;
        if anime && double {
            self.check_size(double_size)?;
        }
        // Size changes need a fresh pool. In a session, the number of allocated
        // textures is otherwise bounded by the simultaneously live passes.
        if self
            .textures
            .first()
            .is_some_and(|first| first.size != source)
        {
            self.clear_textures();
        }
        let gl = self.gl;
        let input_index = self.target(source, Format::Rgba8, &[])?;
        unsafe {
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.textures[input_index].id));
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                source.0 as i32,
                source.1 as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(input)),
            );
            gl.bind_vertex_array(self.vao);
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
        }
        let mut current = HashMap::from([("MAIN", input_index)]);
        if anime {
            current.insert("original", input_index);
        }
        let mut current_size = source;
        for index in 0..self.passes.len() {
            let pass = &self.passes[index];
            if pass.conditional && !double {
                continue;
            }
            let final_pass = index + 1 == self.passes.len();
            let size = if !anime || final_pass {
                target
            } else if pass.doubled {
                double_size
            } else {
                current_size
            };
            let format = pass.format;
            let mut used = current.values().copied().collect::<Vec<_>>();
            used.push(input_index);
            let next = self.target(size, format, &used)?;
            let pass = &self.passes[index];
            self.draw(pass, &current, next)?;
            current.insert(pass.save, next);
            if pass.save == "MAIN" {
                current_size = size;
            }
            current.retain(|name, _| {
                self.passes[index + 1..]
                    .iter()
                    .any(|next| (!next.conditional || double) && next.bindings.contains(name))
            });
        }
        unsafe {
            gl.read_pixels(
                0,
                0,
                target.0 as i32,
                target.1 as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(output)),
            );
            let err = gl.get_error();
            if err != glow::NO_ERROR {
                return Err(format!("render OpenGL error {err:#x}").into());
            }
        }
        Ok(())
    }

    fn check_size(&self, size: Size) -> Result<()> {
        frame_len(size)?;
        if size.0.max(size.1) > self.limit {
            return Err("render_size".into());
        }
        Ok(())
    }

    fn target(&mut self, size: Size, format: Format, used: &[usize]) -> Result<usize> {
        let available = |(i, _): &(usize, &Texture)| !used.contains(i);
        let exact = self
            .textures
            .iter()
            .enumerate()
            .filter(available)
            .find(|(_, texture)| texture.size == size && texture.format == format)
            .map(|(i, _)| i);
        if let Some(index) = exact {
            return Ok(index);
        }
        let free = self
            .textures
            .iter()
            .enumerate()
            .filter(available)
            .find(|(_, texture)| texture.format == format)
            .map(|(i, _)| i);
        let id = if let Some(index) = free {
            self.textures[index].id
        } else {
            gpu::texture(self.gl)?
        };
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(id));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                format.internal() as i32,
                size.0 as i32,
                size.1 as i32,
                0,
                if format == Format::R16f {
                    glow::RED
                } else {
                    glow::RGBA
                },
                if format == Format::Rgba8 {
                    glow::UNSIGNED_BYTE
                } else {
                    glow::HALF_FLOAT
                },
                glow::PixelUnpackData::Slice(None),
            );
        }
        let index = free.unwrap_or(self.textures.len());
        let texture = Texture { id, size, format };
        if let Some(current) = self.textures.get_mut(index) {
            *current = texture;
        } else {
            self.textures.push(texture);
        }
        Ok(index)
    }

    fn draw(&self, pass: &Pass, inputs: &HashMap<&str, usize>, output: usize) -> Result<()> {
        let gl = self.gl;
        let texture = &self.textures[output];
        unsafe {
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture.id),
                0,
            );
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                return Err("render worker framebuffer is incomplete".into());
            }
            gl.viewport(0, 0, texture.size.0 as i32, texture.size.1 as i32);
            gl.use_program(Some(pass.program));
            let size = gl.get_uniform_location(pass.program, "outputSize");
            gl.uniform_2_f32(size.as_ref(), texture.size.0 as f32, texture.size.1 as f32);
            for (unit, name) in pass.bindings.iter().enumerate() {
                let input = inputs
                    .get(name)
                    .ok_or("render pass requested an unavailable image")?;
                gl.active_texture(glow::TEXTURE0 + unit as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.textures[*input].id));
                let uniform = if pass.save == "output" {
                    if unit == 0 {
                        "image".to_owned()
                    } else {
                        "original".to_owned()
                    }
                } else if self.filter != Filter::Anime4k {
                    "inputImage".to_owned()
                } else {
                    format!("texture{unit}")
                };
                let location = gl.get_uniform_location(pass.program, &uniform);
                gl.uniform_1_i32(location.as_ref(), unit as i32);
            }
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.active_texture(glow::TEXTURE0);
        }
        Ok(())
    }

    fn clear_textures(&mut self) {
        for texture in self.textures.drain(..) {
            unsafe { self.gl.delete_texture(texture.id) };
        }
    }
}

impl Drop for Renderer<'_> {
    fn drop(&mut self) {
        self.clear_textures();
        unsafe {
            for pass in self.passes.drain(..) {
                self.gl.delete_program(pass.program);
            }
            if let Some(fbo) = self.framebuffer {
                self.gl.delete_framebuffer(fbo);
            }
            if let Some(vao) = self.vao {
                self.gl.delete_vertex_array(vao);
            }
        }
    }
}

fn shader_sections(src: &'static str) -> impl Iterator<Item = &'static str> {
    src.split("//!DESC ").skip(1)
}

fn anime_pass(gl: &glow::Context, section: &'static str) -> Result<Pass> {
    let mut shader =
        String::from("#version 400 core\nuniform vec2 outputSize;\nout vec4 outputColor;\n");
    let mut bindings = Vec::new();
    for name in section
        .lines()
        .filter_map(|line| line.strip_prefix("//!BIND "))
    {
        let unit = bindings.len();
        let actual = if name == "HOOKED" { "MAIN" } else { name };
        bindings.push(actual);
        use std::fmt::Write;
        writeln!(shader, "uniform sampler2D texture{unit};")?;
        for alias in std::iter::once(name).chain((name != actual).then_some(actual)) {
            writeln!(shader, "#define {alias}_pos (gl_FragCoord.xy / outputSize)")?;
            writeln!(
                shader,
                "#define {alias}_size vec2(textureSize(texture{unit}, 0))"
            )?;
            writeln!(shader, "#define {alias}_pt (1.0 / {alias}_size)")?;
            writeln!(shader, "#define {alias}_tex(p) texture(texture{unit}, (p))")?;
            writeln!(
                shader,
                "#define {alias}_texOff(p) {alias}_tex({alias}_pos + (p) * {alias}_pt)"
            )?;
        }
    }
    let (_, body) = section
        .split_once('\n')
        .ok_or("missing Anime4K shader body")?;
    shader.push_str(body);
    shader.push_str("\nvoid main() { outputColor = hook(); }\n");
    Ok(Pass {
        program: gpu::program(gl, &shader)?,
        bindings,
        save: section
            .lines()
            .find_map(|line| line.strip_prefix("//!SAVE "))
            .unwrap_or("MAIN"),
        format: if section.lines().any(|line| line == "//!COMPONENTS 1") {
            Format::R16f
        } else {
            Format::Rgba16f
        },
        doubled: section
            .lines()
            .any(|line| line.starts_with("//!WIDTH ") && line.ends_with(" 2 *")),
        conditional: section.lines().any(|line| line.starts_with("//!WHEN ")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_frames_beyond_the_shared_buffer() {
        assert_eq!(frame_len((3840, 2160)).unwrap(), FRAME_BYTES);
        for size in [(0, 480), (1, 8193), (3841, 2160), (u32::MAX, u32::MAX)] {
            assert_eq!(frame_len(size).unwrap_err().to_string(), "render_size");
        }
    }

    #[test]
    #[ignore = "requires Mesa surfaceless OpenGL"]
    fn render_all_filter_passes_on_mesa() {
        let context = render_device::Context::software_test_context().unwrap();
        let source = (16, 12);
        let mut pixels = Vec::with_capacity(frame_len(source).unwrap());
        for y in 0..source.1 {
            for x in 0..source.0 {
                let edge = (x / 2 + y / 3) % 2;
                pixels.extend_from_slice(&[32 + 150 * edge as u8, 32 + 17 * y as u8, 120, 255]);
            }
        }
        let target = (32, 24);
        let mut rendered = Vec::new();
        for filter in [Filter::Fsr, Filter::Sgsr, Filter::Anime4k] {
            let mut renderer = Renderer::new(context.gl(), filter).unwrap();
            let mut output = vec![0; frame_len(target).unwrap()];
            renderer
                .render(source, target, &pixels, &mut output)
                .unwrap();
            assert!(output.chunks_exact(4).all(|pixel| pixel[3] == 255));
            assert!(output.first_chunk::<4>().unwrap()[1] < output.last_chunk::<4>().unwrap()[1]);
            rendered.push(output.clone());
            renderer
                .render(source, target, &pixels, &mut output)
                .unwrap();
            assert_eq!(rendered.last().unwrap(), &output);
            if filter == Filter::Anime4k {
                assert_eq!(renderer.passes.len(), 13);
                if let Some(path) = std::env::var_os("PICALENS_TEST_ANIME4K_RGBA") {
                    std::fs::write(path, &output).unwrap();
                }
                renderer
                    .render(source, source, &pixels, &mut output[..pixels.len()])
                    .unwrap();
                assert_ne!(&output[..pixels.len()], &pixels);
            }
        }
        assert_ne!(rendered[0], rendered[1]);
        assert_ne!(rendered[1], rendered[2]);
    }
}
