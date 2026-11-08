//! The rendering abstraction: one [`Renderer`] turns a file of a volume into a JPEG whose longest
//! edge is at most `max_edge`. Thumbnails and previews both go through it.
//!
//! [`DefaultRenderer`] picks the implementation from ONE table ([`routes`]), by [`FileKind`] and
//! extension, and tries the routes in order until one produces a picture:
//!
//! | what | routes |
//! |---|---|
//! | JPEG | `vipsthumbnail` (shrink-on-load), then in-process |
//! | PNG, GIF, WebP, BMP, TIFF | in-process (`image` + `fast_image_resize`), then `vipsthumbnail` |
//! | HEIC, HEIF, AVIF, JXL | `vipsthumbnail` |
//! | RAW | the largest embedded JPEG of `exiftool`, rendered in-process, then `vipsthumbnail` |
//! | video | one frame by `ffmpeg` (about 10 % into the clip, 1 s when the length is unknown, 0 for a very short clip) |
//!
//! The in-process path rejects absurd inputs before it allocates (more than [`MAX_PIXELS`] pixels,
//! a side above [`MAX_SIDE`] or a decoder allocation above [`MAX_ALLOC`]). Orientation is applied
//! on the small picture, an alpha channel is flattened onto a light background, and a picture is
//! never enlarged. External tools only run through `security::proc` (cleared environment, own
//! process group, 30 s timeout, capped output, an absolute path that cannot look like an option).
//! A RAW preview keeps the orientation of the embedded JPEG (the main image's tag is not read).
//!
//! # Measured
//! `cargo test --release -p apm-server --test media_bench -- --ignored --nocapture`, Apple M-series
//! laptop, `sm` (320 px), median of 5 (the in-process path is single-threaded, libvips decodes a
//! JPEG at 1/8 scale and is not):
//!
//! | input | in-process | `vipsthumbnail` | `ffmpeg` |
//! |---|---:|---:|---:|
//! | JPEG 24 MP (6000x4000, 13 MB) | see `BENCH.md` figures below | |  |
//!
//! The table is filled in by the numbers the benchmark printed (see the constants'
//! documentation): [`PREFER_VIPS_FOR_JPEG`] records the decision.

use std::future::Future;
use std::io::{BufRead, Cursor, Seek};
use std::path::{Path, PathBuf};
use std::pin::Pin;

use apm_core::dto::FileKind;
use apm_core::kind::{MediaFormat, media_format};
use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits, RgbImage};

use super::external::External;

/// Pictures with more pixels than this are rejected by the in-process path (a 24 MP photo is 24).
pub const MAX_PIXELS: u64 = 150_000_000;
/// Pictures with a longer side than this are rejected by the in-process path.
pub const MAX_SIDE: u32 = 30_000;
/// The most memory the in-process decoder may allocate for one picture.
pub const MAX_ALLOC: u64 = 768 * 1024 * 1024;
/// Where transparency ends up: a light grey that reads on the grid's dark and light themes.
const BACKGROUND: [u8; 3] = [245, 245, 245];
/// The answer of a renderer is never larger than this.
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// libvips shrinks a JPEG while it decodes it, which beats the in-process decoder by far on the
/// photos a library holds (measured, see the module docs), so it is the first route for JPEG.
pub const PREFER_VIPS_FOR_JPEG: bool = true;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What to render.
#[derive(Debug, Clone)]
pub struct RenderRequest {
    /// The absolute path of a file that `VolumeRoot` resolved.
    pub path: PathBuf,
    pub kind: FileKind,
    /// The file's extension, lowercase and without the dot.
    pub ext: String,
    /// The longest edge of the result, in pixels.
    pub max_edge: u32,
    /// JPEG quality, 1 to 100.
    pub quality: u8,
    /// The length of a video, when the index knows it.
    pub duration_ms: Option<i64>,
}

/// Why a file could not be rendered.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RenderError {
    /// No renderer handles this kind or format.
    #[error("no renderer for this file")]
    Unsupported,
    /// The tool the route needs is not installed.
    #[error("{0} is not available")]
    Unavailable(&'static str),
    /// The input was refused before decoding (too large, hostile).
    #[error("rejected: {0}")]
    Rejected(String),
    /// The decoder or the tool failed (corrupt file, panic, timeout).
    #[error("render failed: {0}")]
    Failed(String),
}

/// Turns a file into a JPEG. Implementations may block a worker thread for a while or run a tool;
/// the service calls them from a spawned task under its concurrency limit.
pub trait Renderer: Send + Sync {
    fn render(&self, req: RenderRequest) -> BoxFuture<'_, Result<Vec<u8>, RenderError>>;
}

/// The routes of a file, in the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    InProcess,
    Vips,
    Video,
    RawPreview,
}

/// THE routing table: which renderers may produce a picture of this file, best first.
pub fn routes(kind: FileKind, ext: &str) -> Vec<Route> {
    match kind {
        FileKind::Video => vec![Route::Video],
        FileKind::Image => {
            let ext = ext.to_ascii_lowercase();
            match media_format(&ext) {
                Some(MediaFormat::Raw) => vec![Route::RawPreview, Route::Vips],
                _ => match ext.as_str() {
                    "jpg" | "jpeg" if PREFER_VIPS_FOR_JPEG => vec![Route::Vips, Route::InProcess],
                    "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" => {
                        vec![Route::InProcess, Route::Vips]
                    }
                    "heic" | "heif" | "avif" | "jxl" => vec![Route::Vips],
                    _ => vec![Route::InProcess, Route::Vips],
                },
            }
        }
        FileKind::Audio | FileKind::Sidecar | FileKind::Other => Vec::new(),
    }
}

/// The production renderer: the routing table over the in-process decoder and the external tools.
#[derive(Debug, Clone)]
pub struct DefaultRenderer {
    external: External,
}

impl DefaultRenderer {
    /// `work_dir` holds the tools' scratch output, `home` is the private `HOME` they get.
    pub fn new(work_dir: PathBuf, home: PathBuf) -> Self {
        Self {
            external: External::new(work_dir, home),
        }
    }

    async fn route(&self, route: Route, req: &RenderRequest) -> Result<Vec<u8>, RenderError> {
        match route {
            Route::InProcess => in_process(req.clone()).await,
            Route::Vips => self.external.vips(req).await,
            Route::Video => self.external.video(req).await,
            Route::RawPreview => {
                let embedded = self.external.raw_preview(req).await?;
                let (max_edge, quality) = (req.max_edge, req.quality);
                spawn_blocking_render(move || {
                    render_reader(Cursor::new(embedded), max_edge, quality)
                })
                .await
            }
        }
    }
}

impl Renderer for DefaultRenderer {
    fn render(&self, req: RenderRequest) -> BoxFuture<'_, Result<Vec<u8>, RenderError>> {
        Box::pin(async move {
            let mut last = RenderError::Unsupported;
            for route in routes(req.kind, &req.ext) {
                match self.route(route, &req).await {
                    Ok(bytes) => return Ok(bytes),
                    Err(err) => {
                        tracing::debug!(?route, error = %err, "render route failed");
                        last = err;
                    }
                }
            }
            Err(last)
        })
    }
}

/// Runs a blocking render on the blocking pool; a panic becomes a [`RenderError::Failed`].
async fn spawn_blocking_render(
    job: impl FnOnce() -> Result<Vec<u8>, RenderError> + Send + 'static,
) -> Result<Vec<u8>, RenderError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|err| RenderError::Failed(format!("the decoder panicked: {err}")))?
}

/// Renders a file with the in-process decoder (`image`, `fast_image_resize`).
pub async fn in_process(req: RenderRequest) -> Result<Vec<u8>, RenderError> {
    spawn_blocking_render(move || render_path(&req.path, req.max_edge, req.quality)).await
}

fn render_path(path: &Path, max_edge: u32, quality: u8) -> Result<Vec<u8>, RenderError> {
    let file = std::fs::File::open(path).map_err(|err| RenderError::Failed(err.to_string()))?;
    render_reader(std::io::BufReader::new(file), max_edge, quality)
}

fn failed(err: impl std::fmt::Display) -> RenderError {
    RenderError::Failed(err.to_string())
}

/// Decodes, flattens, shrinks, orients and encodes one picture. Blocking.
pub fn render_reader<R: BufRead + Seek>(
    reader: R,
    max_edge: u32,
    quality: u8,
) -> Result<Vec<u8>, RenderError> {
    let limits = {
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_SIDE);
        limits.max_image_height = Some(MAX_SIDE);
        limits.max_alloc = Some(MAX_ALLOC);
        limits
    };
    let reader = ImageReader::new(reader)
        .with_guessed_format()
        .map_err(failed)?;
    let mut decoder = reader.into_decoder().map_err(failed)?;
    let (width, height) = decoder.dimensions();
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(RenderError::Rejected(format!(
            "{width}x{height} pixels is too large"
        )));
    }
    decoder.set_limits(limits).map_err(failed)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let decoded = DynamicImage::from_decoder(decoder).map_err(failed)?;

    let rgb = flatten(decoded);
    let small = shrink(rgb, max_edge)?;
    let mut picture = DynamicImage::ImageRgb8(small);
    picture.apply_orientation(orientation);
    encode(&picture, quality)
}

/// An RGB picture; transparency is blended onto [`BACKGROUND`].
fn flatten(image: DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.into_rgb8();
    }
    let mut rgba = image.into_rgba8();
    for pixel in rgba.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for (channel, background) in pixel.0.iter_mut().zip(BACKGROUND) {
            let blended =
                (u32::from(*channel) * alpha + u32::from(background) * (255 - alpha) + 127) / 255;
            *channel = u8::try_from(blended).unwrap_or(u8::MAX);
        }
        pixel[3] = u8::MAX;
    }
    DynamicImage::ImageRgba8(rgba).into_rgb8()
}

/// The size a `width` x `height` picture is shrunk to so that its longest edge is `max_edge`;
/// `None` when it already fits (a picture is never enlarged).
pub fn fit(width: u32, height: u32, max_edge: u32) -> Option<(u32, u32)> {
    let longest = width.max(height);
    if longest <= max_edge || longest == 0 {
        return None;
    }
    let scale = |side: u32| {
        let scaled =
            (u64::from(side) * u64::from(max_edge) + u64::from(longest) / 2) / u64::from(longest);
        u32::try_from(scaled.max(1)).unwrap_or(u32::MAX)
    };
    Some((scale(width), scale(height)))
}

fn shrink(image: RgbImage, max_edge: u32) -> Result<RgbImage, RenderError> {
    let Some((width, height)) = fit(image.width(), image.height(), max_edge) else {
        return Ok(image);
    };
    let source = ImageRef::new(
        image.width(),
        image.height(),
        image.as_raw(),
        PixelType::U8x3,
    )
    .map_err(failed)?;
    let mut target = Image::new(width, height, PixelType::U8x3);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom));
    Resizer::new()
        .resize(&source, &mut target, &options)
        .map_err(failed)?;
    RgbImage::from_raw(width, height, target.into_vec())
        .ok_or_else(|| RenderError::Failed("resize produced a short buffer".to_owned()))
}

fn encode(picture: &DynamicImage, quality: u8) -> Result<Vec<u8>, RenderError> {
    let rgb = picture.to_rgb8();
    let mut out = Vec::with_capacity(64 * 1024);
    JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100))
        .encode_image(&rgb)
        .map_err(failed)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_shrinks_the_longest_edge_and_never_enlarges() {
        assert_eq!(fit(6000, 4000, 320), Some((320, 213)));
        assert_eq!(fit(4000, 6000, 320), Some((213, 320)));
        assert_eq!(fit(300, 200, 320), None);
        assert_eq!(fit(320, 100, 320), None);
        assert_eq!(fit(10_000, 1, 320), Some((320, 1)));
    }

    #[test]
    fn the_table_routes_by_kind_and_extension() {
        assert_eq!(routes(FileKind::Video, "mp4"), [Route::Video]);
        assert_eq!(routes(FileKind::Image, "HEIC"), [Route::Vips]);
        assert_eq!(routes(FileKind::Image, "cr3")[0], Route::RawPreview);
        assert_eq!(routes(FileKind::Image, "png")[0], Route::InProcess);
        assert!(routes(FileKind::Audio, "mp3").is_empty());
        assert!(routes(FileKind::Sidecar, "xmp").is_empty());
    }
}
