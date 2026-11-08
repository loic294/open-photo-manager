//! Picture fixtures built in code: JPEGs (with an EXIF orientation), PNGs with alpha, and a helper
//! that indexes a tree with a fake extractor so the thumbnail stage runs as in production.

use std::io::Cursor;
use std::sync::Arc;

use apm_core::metadata::SourceMetadata;
use apm_server::indexer::{Embedded, FakeExtractor, RunContext, ScanRequest};
use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

use super::TestApp;

/// A smooth gradient JPEG of `width` x `height`.
pub fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(width, height, |x, y| {
        Rgb([
            (x * 255 / width.max(1)) as u8,
            (y * 255 / height.max(1)) as u8,
            128,
        ])
    });
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

/// A JPEG whose red left half and blue right half make the orientation visible, with EXIF
/// `orientation` (1..=8) stored in an APP1 segment.
pub fn jpeg_with_orientation(width: u32, height: u32, orientation: u16) -> Vec<u8> {
    let img = RgbImage::from_fn(width, height, |x, _| {
        if x < width / 2 {
            Rgb([255, 0, 0])
        } else {
            Rgb([0, 0, 255])
        }
    });
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Jpeg).unwrap();
    let jpeg = out.into_inner();

    let mut exif = b"Exif\0\0II*\0".to_vec();
    exif.extend_from_slice(&8_u32.to_le_bytes());
    exif.extend_from_slice(&1_u16.to_le_bytes());
    exif.extend_from_slice(&0x0112_u16.to_le_bytes());
    exif.extend_from_slice(&3_u16.to_le_bytes());
    exif.extend_from_slice(&1_u32.to_le_bytes());
    exif.extend_from_slice(&orientation.to_le_bytes());
    exif.extend_from_slice(&[0, 0]);
    exif.extend_from_slice(&0_u32.to_le_bytes());
    let mut segment = vec![0xFF, 0xE1];
    segment.extend_from_slice(&u16::try_from(exif.len() + 2).unwrap().to_be_bytes());
    segment.extend_from_slice(&exif);

    let mut with = jpeg[..2].to_vec();
    with.extend_from_slice(&segment);
    with.extend_from_slice(&jpeg[2..]);
    with
}

/// A PNG that is fully transparent except for an opaque dark square in the middle.
pub fn png_with_alpha(width: u32, height: u32) -> Vec<u8> {
    let img = RgbaImage::from_fn(width, height, |x, y| {
        let inside = x > width / 3 && x < 2 * width / 3 && y > height / 3 && y < 2 * height / 3;
        if inside {
            Rgba([10, 10, 10, 255])
        } else {
            Rgba([0, 0, 0, 0])
        }
    });
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Scans and indexes every volume (metadata faked, thumbnails really rendered).
pub async fn index(app: &TestApp) {
    let fake = Arc::new(FakeExtractor::new(|_| {
        Ok(Embedded {
            meta: SourceMetadata::default(),
            width: None,
            height: None,
            duration_ms: None,
            mime: None,
        })
    }));
    app.state
        .indexer
        .with_extractor(fake)
        .with_timezone(chrono_tz::UTC)
        .run(&ScanRequest::default(), &RunContext::noop())
        .await
        .unwrap();
}

/// The id of the file at `dir`/`name` of volume `volume_id`.
pub async fn file_id(app: &TestApp, volume_id: i64, dir: &str, name: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM files WHERE volume_id = ? AND dir_path = ? AND name = ?")
        .bind(volume_id)
        .bind(dir)
        .bind(name)
        .fetch_one(app.state.db.read())
        .await
        .unwrap()
}

/// `thumb_state` of file `id`.
pub async fn thumb_state(app: &TestApp, id: i64) -> i64 {
    sqlx::query_scalar("SELECT thumb_state FROM files WHERE id = ?")
        .bind(id)
        .fetch_one(app.state.db.read())
        .await
        .unwrap()
}

/// Decodes the JPEG in `bytes` and returns its size.
pub fn decoded_size(bytes: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory(bytes).unwrap();
    (img.width(), img.height())
}
