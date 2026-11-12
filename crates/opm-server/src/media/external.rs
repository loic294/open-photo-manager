//! The external renderers: `vipsthumbnail`, `ffmpeg` and `exiftool` (embedded RAW previews).
//!
//! Every call goes through `security::proc`: no shell, a cleared environment (`PATH`, `TZ`,
//! `TMPDIR` forwarded, a private empty `HOME`), a 30 s timeout, capped output and an absolute path
//! argument that no tool can read as an option.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::render::{MAX_OUTPUT_BYTES, RenderError, RenderRequest};
use crate::security::proc::{self, ProcError, ProcSpec};

const TIMEOUT: Duration = Duration::from_secs(30);
/// Tags of the embedded pictures of a RAW file, the usually largest first.
const RAW_PREVIEW_TAGS: [&str; 4] = ["JpgFromRaw", "PreviewImage", "OtherImage", "ThumbnailImage"];
/// The start of every JPEG stream.
const JPEG_MAGIC: [u8; 3] = [0xff, 0xd8, 0xff];

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub(super) struct External {
    work_dir: PathBuf,
    home: PathBuf,
    vips: &'static str,
    ffmpeg: &'static str,
    exiftool: &'static str,
}

impl External {
    pub(super) fn new(work_dir: PathBuf, home: PathBuf) -> Self {
        Self {
            work_dir,
            home,
            vips: "vipsthumbnail",
            ffmpeg: "ffmpeg",
            exiftool: "exiftool",
        }
    }

    fn spec(&self, program: &str) -> ProcSpec {
        ProcSpec::new(program)
            .timeout(TIMEOUT)
            .home(&self.home)
            .inherit_env("TZ")
            .inherit_env("TMPDIR")
    }

    /// `vipsthumbnail`: shrink-on-load, orientation applied, transparency flattened.
    pub(super) async fn vips(&self, req: &RenderRequest) -> Result<Vec<u8>, RenderError> {
        let out = self.work_dir.join(format!(
            "vips-{}-{}.jpg",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut target = OsString::from(&out);
        target.push(format!(
            "[Q={},strip,background=245 245 245]",
            req.quality.clamp(1, 100)
        ));
        let spec = self
            .spec(self.vips)
            .env("VIPS_BLOCK_UNTRUSTED", "1")
            .arg("--size")
            .arg(format!("{0}x{0}>", req.max_edge))
            .arg("-o")
            .arg(target)
            .arg("--")
            .path_arg(&req.path);
        let ran = proc::run(spec).await;
        let bytes = read_output(&out);
        let _ = std::fs::remove_file(&out);
        ran.map_err(|err| tool_error("vipsthumbnail", &err))?;
        bytes
    }

    /// One frame of a video, scaled by `ffmpeg` and encoded as JPEG.
    pub(super) async fn video(&self, req: &RenderRequest) -> Result<Vec<u8>, RenderError> {
        let at = seek_seconds(req.duration_ms);
        let first = self.frame(req, at).await;
        match first {
            Ok(bytes) if !bytes.is_empty() => Ok(bytes),
            Ok(_) | Err(_) if at > 0.0 => {
                let retry = self.frame(req, 0.0).await?;
                if retry.is_empty() {
                    Err(RenderError::Failed("the video has no picture".to_owned()))
                } else {
                    Ok(retry)
                }
            }
            Ok(_) => Err(RenderError::Failed("the video has no picture".to_owned())),
            Err(err) => Err(err),
        }
    }

    async fn frame(&self, req: &RenderRequest, at: f64) -> Result<Vec<u8>, RenderError> {
        let edge = req.max_edge;
        let quality = jpeg_qscale(req.quality);
        let spec = self
            .spec(self.ffmpeg)
            .max_stdout(MAX_OUTPUT_BYTES)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
            .args(["-protocol_whitelist", "file"])
            .arg("-ss")
            .arg(format!("{at:.3}"))
            .arg("-i")
            .path_arg(&req.path)
            .args(["-frames:v", "1", "-an", "-sn", "-dn"])
            .arg("-vf")
            .arg(format!(
                "scale=w='min({edge},iw)':h='min({edge},ih)':force_original_aspect_ratio=decrease,format=yuvj420p"
            ))
            .args(["-f", "image2pipe", "-c:v", "mjpeg", "-q:v"])
            .arg(quality.to_string())
            .arg("pipe:1");
        match proc::run(spec).await {
            Ok(output) => Ok(output.stdout),
            Err(err) => Err(tool_error("ffmpeg", &err)),
        }
    }

    /// The embedded JPEG of a RAW file: the first of [`RAW_PREVIEW_TAGS`] that holds one.
    pub(super) async fn raw_preview(&self, req: &RenderRequest) -> Result<Vec<u8>, RenderError> {
        let mut last = RenderError::Failed("the file has no embedded preview".to_owned());
        for tag in RAW_PREVIEW_TAGS {
            let spec = self
                .spec(self.exiftool)
                .max_stdout(MAX_OUTPUT_BYTES)
                .args(["-b", "-m", "-q"])
                .arg(format!("-{tag}"))
                .arg("--")
                .path_arg(&req.path);
            match proc::run(spec).await {
                Ok(output) if output.stdout.starts_with(&JPEG_MAGIC) => return Ok(output.stdout),
                Ok(_) => {}
                Err(err) if err.is_not_found() => return Err(tool_error("exiftool", &err)),
                Err(err) => last = tool_error("exiftool", &err),
            }
        }
        Err(last)
    }
}

/// Where in a video the picture is taken: 10 % in, 1 s when the length is unknown, the start of a
/// very short clip.
fn seek_seconds(duration_ms: Option<i64>) -> f64 {
    match duration_ms {
        Some(ms) if ms < 2_000 => 0.0,
        #[allow(clippy::cast_precision_loss)]
        Some(ms) => ms as f64 / 10_000.0,
        None => 1.0,
    }
}

/// The `-q:v` scale of ffmpeg's MJPEG encoder (2 best to 31 worst) for a JPEG quality.
fn jpeg_qscale(quality: u8) -> u8 {
    let quality = u32::from(quality.clamp(1, 100));
    u8::try_from(2 + (100 - quality) * 29 / 99).unwrap_or(31)
}

fn tool_error(tool: &'static str, err: &ProcError) -> RenderError {
    if err.is_not_found() {
        RenderError::Unavailable(tool)
    } else {
        RenderError::Failed(err.to_string())
    }
}

fn read_output(path: &Path) -> Result<Vec<u8>, RenderError> {
    let meta = std::fs::metadata(path).map_err(|err| RenderError::Failed(err.to_string()))?;
    if usize::try_from(meta.len()).map_or(true, |len| len > MAX_OUTPUT_BYTES) {
        return Err(RenderError::Rejected(
            "the rendered picture is too large".to_owned(),
        ));
    }
    std::fs::read(path).map_err(|err| RenderError::Failed(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_video_frame_is_taken_a_tenth_in() {
        assert!((seek_seconds(Some(10_000)) - 1.0).abs() < 1e-9);
        assert!((seek_seconds(Some(100_000)) - 10.0).abs() < 1e-9);
        assert!((seek_seconds(Some(1_500))).abs() < 1e-9);
        assert!((seek_seconds(None) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_quality_maps_onto_the_encoder_scale() {
        assert_eq!(jpeg_qscale(100), 2);
        assert_eq!(jpeg_qscale(1), 31);
        assert!(jpeg_qscale(80) < jpeg_qscale(50));
    }
}
