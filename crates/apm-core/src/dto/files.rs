//! File DTOs: listing items and the detail view.
//!
//! Owned by todo `building-browse-api`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::common::{CaptureSource, FileKind, Overlay};
use crate::paths::{FileId, VolumeId};

/// One file in a listing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileItem {
    pub id: FileId,
    pub volume_id: VolumeId,
    /// Directory relative to the volume root, `/`-separated. `""` is the root.
    pub dir: String,
    /// File name including the extension.
    pub name: String,
    pub kind: FileKind,
    /// Size in bytes.
    pub size: i64,
    /// Capture time as naive-local epoch seconds (the photographer's wall clock).
    pub capture_ts: Option<i64>,
    pub capture_src: Option<CaptureSource>,
    /// Normalised "Make Model".
    pub camera: Option<String>,
    /// Star rating, 0 = unrated.
    pub rating: u8,
    /// -1 rejected, 0 none, 1 selected.
    pub flag: i8,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Duration of videos and audio in milliseconds.
    pub duration_ms: Option<i64>,
    pub overlay: Overlay,
}

/// A sidecar file attached to a media file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SidecarDto {
    pub name: String,
    /// How the sidecar is matched: `exact` (`IMG_1.jpg.xmp`) or `stem` (`IMG_1.xmp`).
    pub kind: String,
}

/// Everything known about one file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileDetail {
    pub item: FileItem,
    /// Lowercase hex SHA-1 of the content, once computed.
    pub hash: Option<String>,
    pub mime: Option<String>,
    /// Modification time, epoch seconds.
    pub mtime: i64,
    /// Birth (creation) time, epoch seconds, when the filesystem reports it.
    pub btime: Option<i64>,
    pub tags: Vec<String>,
    pub sidecars: Vec<SidecarDto>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    fn sample_item() -> FileItem {
        FileItem {
            id: 42,
            volume_id: 1,
            dir: "2024/Trip".to_owned(),
            name: "IMG_0001.jpg".to_owned(),
            kind: FileKind::Image,
            size: 5_242_880,
            capture_ts: Some(1_718_000_000),
            capture_src: Some(CaptureSource::Exif),
            camera: Some("Canon EOS R5".to_owned()),
            rating: 4,
            flag: -1,
            width: Some(8192),
            height: Some(5464),
            duration_ms: None,
            overlay: Overlay {
                matched: true,
                planned: false,
                will_delete: false,
            },
        }
    }

    #[test]
    fn file_item_roundtrips_with_the_documented_wire_shape() {
        let wire = roundtrip(&sample_item());
        assert_eq!(wire["kind"], json!("image"));
        assert_eq!(wire["capture_src"], json!("exif"));
        assert_eq!(wire["flag"], json!(-1));
        assert_eq!(wire["duration_ms"], json!(null));
        assert_eq!(wire["overlay"]["matched"], json!(true));
        roundtrip(&FileItem::default());
    }

    #[test]
    fn file_detail_roundtrips() {
        roundtrip(&FileDetail {
            item: sample_item(),
            hash: Some("da39a3ee5e6b4b0d3255bfef95601890afd80709".to_owned()),
            mime: Some("image/jpeg".to_owned()),
            mtime: 1_718_000_100,
            btime: None,
            tags: vec!["holiday".to_owned(), "family".to_owned()],
            sidecars: vec![SidecarDto {
                name: "IMG_0001.jpg.xmp".to_owned(),
                kind: "exact".to_owned(),
            }],
        });
        roundtrip(&FileDetail::default());
    }
}
