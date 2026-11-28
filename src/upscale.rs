use std::{
    io,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
};

use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images};
use gtk::glib;

use crate::{capture::Frame, session::Result, settings::Filter};

pub type Size = (u32, u32);

pub const MAX_PIXELS: u64 = 3840 * 2160;
pub const MAX_EDGE: u32 = 8192;

#[derive(Clone)]
pub struct Pixels {
    pub size: Size,
    pub bytes: glib::Bytes,
}

impl From<Frame> for Pixels {
    fn from(frame: Frame) -> Self {
        Self {
            size: (frame.width, frame.height),
            bytes: glib::Bytes::from_owned(frame.rgba),
        }
    }
}

impl Pixels {
    /// Copies the RGBA rectangle `(x, y, width, height)`.
    pub fn crop(&self, (x, y, width, height): (u32, u32, u32, u32)) -> Option<Self> {
        if (x, y, width, height) == (0, 0, self.size.0, self.size.1) {
            return Some(self.clone());
        }
        if width == 0
            || height == 0
            || x.checked_add(width)? > self.size.0
            || y.checked_add(height)? > self.size.1
        {
            return None;
        }
        let stride = self.size.0 as usize * 4;
        let row = width as usize * 4;
        let mut bytes = Vec::with_capacity(row * height as usize);
        for line in y as usize..(y + height) as usize {
            let start = line * stride + x as usize * 4;
            bytes.extend_from_slice(self.bytes.get(start..start + row)?);
        }
        Some(Self {
            size: (width, height),
            bytes: glib::Bytes::from_owned(bytes),
        })
    }
}

pub struct ScaledFrame {
    pub source: Size,
    pub image: Pixels,
}

struct Job {
    source: Pixels,
    target: Size,
}

pub struct Upscaler {
    jobs: SyncSender<Job>,
    results: Receiver<std::result::Result<ScaledFrame, String>>,
    latest: Option<Pixels>,
    generation: u64,
    submitted: Option<(u64, Size)>,
    busy: bool,
}

impl Upscaler {
    pub fn new(filter: Filter) -> io::Result<Option<Self>> {
        if !matches!(filter, Filter::Lanczos | Filter::Scale2x) {
            return Ok(None);
        }
        let (jobs, work) = mpsc::sync_channel::<Job>(1);
        let (ready, results) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("picalens-upscale".into())
            .spawn(move || {
                let mut resizer = Resizer::new();
                while let Ok(job) = work.recv() {
                    let result = process(filter, &mut resizer, &job);
                    if ready.send(result).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Some(Self {
            jobs,
            results,
            latest: None,
            generation: 0,
            submitted: None,
            busy: false,
        }))
    }

    pub fn set_source(&mut self, pixels: Pixels) {
        self.generation = self.generation.wrapping_add(1);
        self.latest = Some(pixels);
    }

    pub fn update(&mut self, target: Option<Size>) -> Result<Option<ScaledFrame>> {
        let result = match self.results.try_recv() {
            Ok(result) => {
                self.busy = false;
                Some(result?)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => return Err("upscaling worker stopped".into()),
        };
        // Keep one job in flight. New capture frames replace the pending source.
        if let (false, Some(source), Some(target)) = (self.busy, &self.latest, target) {
            let request = (self.generation, target);
            if self.submitted != Some(request) {
                self.jobs.try_send(Job {
                    source: source.clone(),
                    target,
                })?;
                self.submitted = Some(request);
                self.busy = true;
            }
        }
        Ok(result)
    }
}

pub fn processing_size(filter: Filter, source: Size, target: Size) -> Option<Size> {
    let (w, h) = target;
    if w == 0 || h == 0 {
        return None;
    }
    if filter.requires_render_worker() {
        return Some(bounded_size(target, 1.0));
    }
    if source == target {
        return None;
    }
    match filter {
        Filter::Lanczos => Some(bounded_size(target, 1.0)).filter(|&size| size != source),
        Filter::Fsr | Filter::Sgsr => {
            // These shaders expect an upscale. GTK scales further beyond the shader's range.
            let ratio = (2.0 * f64::from(source.0) / f64::from(w))
                .min(2.0 * f64::from(source.1) / f64::from(h));
            let size = bounded_size(target, ratio);
            (size != source && size.0 >= source.0 && size.1 >= source.1).then_some(size)
        }
        Filter::Scale2x if w > source.0 || h > source.1 => {
            let doubled = (source.0.checked_mul(2)?, source.1.checked_mul(2)?);
            (u64::from(doubled.0) * u64::from(doubled.1) <= MAX_PIXELS
                && doubled.0.max(doubled.1) <= MAX_EDGE)
                .then_some(doubled)
        }
        _ => None,
    }
}

fn bounded_size((w, h): Size, ratio: f64) -> Size {
    let ratio = ratio
        .min((MAX_PIXELS as f64 / (f64::from(w) * f64::from(h))).sqrt())
        .min(f64::from(MAX_EDGE) / f64::from(w.max(h)))
        .min(1.0);
    (
        (f64::from(w) * ratio).floor().max(1.0) as u32,
        (f64::from(h) * ratio).floor().max(1.0) as u32,
    )
}

fn process(
    filter: Filter,
    resizer: &mut Resizer,
    job: &Job,
) -> std::result::Result<ScaledFrame, String> {
    let (w, h) = job.source.size;
    let data = if filter == Filter::Scale2x {
        scale2x(&job.source.bytes, w as usize, h as usize)
    } else {
        let input = images::ImageRef::new(w, h, &job.source.bytes, PixelType::U8x4)
            .map_err(|error| error.to_string())?;
        let mut output = images::Image::new(job.target.0, job.target.1, PixelType::U8x4);
        let options = ResizeOptions::new()
            .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
            .use_alpha(false); // Screen capture frames are opaque.
        resizer
            .resize(&input, &mut output, &options)
            .map_err(|error| error.to_string())?;
        output.into_vec()
    };
    Ok(ScaledFrame {
        source: job.source.size,
        image: Pixels {
            size: job.target,
            bytes: glib::Bytes::from_owned(data),
        },
    })
}

fn scale2x(input: &[u8], width: usize, height: usize) -> Vec<u8> {
    // https://www.scale2x.it/algorithm
    let (pixels, _) = input.as_chunks::<4>();
    let mut output = vec![[0; 4]; pixels.len() * 4];
    for (y, rows) in output.chunks_exact_mut(width * 4).enumerate() {
        let row = |index| &pixels[index * width..(index + 1) * width];
        let above = row(y.saturating_sub(1));
        let center = row(y);
        let below = row((y + 1).min(height - 1));
        let (top, bottom) = rows.split_at_mut(width * 2);
        for x in 0..width {
            let up = above[x];
            let down = below[x];
            let left = center[x.saturating_sub(1)];
            let right = center[(x + 1).min(width - 1)];
            let adjacent = up != down && left != right;
            let corner = |edge, side| {
                if adjacent && edge == side {
                    edge
                } else {
                    center[x]
                }
            };
            top[2 * x] = corner(up, left);
            top[2 * x + 1] = corner(up, right);
            bottom[2 * x] = corner(down, left);
            bottom[2 * x + 1] = corner(down, right);
        }
    }
    output.into_flattened()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_rgba_rows() {
        let bytes: Vec<u8> = (0..4 * 3 * 4).map(|value| value as u8).collect();
        let pixels = Pixels {
            size: (4, 3),
            bytes: glib::Bytes::from_owned(bytes),
        };
        let cropped = pixels.crop((1, 1, 2, 2)).unwrap();
        assert_eq!(cropped.size, (2, 2));
        assert_eq!(&cropped.bytes[..8], &[20, 21, 22, 23, 24, 25, 26, 27]);
        assert_eq!(&cropped.bytes[8..], &[36, 37, 38, 39, 40, 41, 42, 43]);
        assert!(pixels.crop((3, 0, 2, 1)).is_none());
        assert_eq!(pixels.crop((0, 0, 4, 3)).unwrap().size, (4, 3));
    }

    #[test]
    fn scale2x_reconstructs_a_diagonal_corner() {
        let black = [0, 0, 0, 255];
        let white = [255; 4];
        let input = [
            black, white, black, white, black, black, black, black, black,
        ]
        .concat();
        let output = scale2x(&input, 3, 3);
        let pixel = |x: usize, y: usize| &output[(y * 6 + x) * 4..(y * 6 + x + 1) * 4];
        assert_eq!(pixel(2, 2), white);
        assert_eq!(pixel(3, 2), black);
        assert_eq!(pixel(2, 3), black);
        assert_eq!(pixel(3, 3), black);
    }

    #[test]
    fn lanczos_preserves_flat_color_and_limits_work_on_large_outputs() {
        let source = Pixels {
            size: (2, 3),
            bytes: glib::Bytes::from_owned([12, 99, 231, 255].repeat(6)),
        };
        let job = Job {
            source,
            target: (7, 9),
        };
        let scaled = process(Filter::Lanczos, &mut Resizer::new(), &job).unwrap();
        assert_eq!(scaled.image.size, (7, 9));
        assert!(
            scaled
                .image
                .bytes
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [12, 99, 231, 255])
        );
        let (w, h) = processing_size(Filter::Lanczos, (1920, 1080), (15360, 8640)).unwrap();
        assert_eq!((w, h), (3840, 2160));
        assert_eq!(
            processing_size(Filter::Scale2x, (3840, 2160), (7680, 4320)),
            None
        );
    }

    #[test]
    fn gpu_filter_size_is_an_upscale_within_the_shader_range() {
        for filter in [Filter::Fsr, Filter::Sgsr] {
            assert_eq!(processing_size(filter, (800, 600), (600, 450)), None);
            assert_eq!(processing_size(filter, (800, 600), (800, 600)), None);
            assert_eq!(
                processing_size(filter, (800, 600), (1200, 900)),
                Some((1200, 900))
            );
            assert_eq!(
                processing_size(filter, (800, 600), (2400, 1800)),
                Some((1600, 1200))
            );
        }
    }
}
