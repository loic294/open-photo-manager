//! Which images a browser shows as they are and which need a JPEG preview.

use opm_core::dto::FileKind;

/// What `GET /api/files/{id}/preview` answers for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewPlan {
    /// The original, streamed like `content` (so `Range` works): browser-native images, audio and
    /// video. A video the browser cannot play is not transcoded; the viewer offers the original.
    Original,
    /// A cached JPEG of at most 2560 px (HEIC, TIFF, RAW, ...).
    Render,
    /// Sidecars and unknown files have no preview.
    None,
}

/// Image formats every browser decodes.
pub fn is_browser_native(ext: &str) -> bool {
    ["jpg", "jpeg", "png", "gif", "webp", "avif", "bmp"]
        .iter()
        .any(|native| native.eq_ignore_ascii_case(ext))
}

/// The preview of a file of `kind` with extension `ext`.
pub fn plan(kind: FileKind, ext: &str) -> PreviewPlan {
    match kind {
        FileKind::Video | FileKind::Audio => PreviewPlan::Original,
        FileKind::Image if is_browser_native(ext) => PreviewPlan::Original,
        FileKind::Image => PreviewPlan::Render,
        FileKind::Sidecar | FileKind::Other => PreviewPlan::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_images_and_media_are_served_as_they_are() {
        assert_eq!(plan(FileKind::Image, "JPG"), PreviewPlan::Original);
        assert_eq!(plan(FileKind::Image, "avif"), PreviewPlan::Original);
        assert_eq!(plan(FileKind::Video, "mov"), PreviewPlan::Original);
        assert_eq!(plan(FileKind::Audio, "mp3"), PreviewPlan::Original);
        assert_eq!(plan(FileKind::Image, "heic"), PreviewPlan::Render);
        assert_eq!(plan(FileKind::Image, "cr3"), PreviewPlan::Render);
        assert_eq!(plan(FileKind::Image, "tif"), PreviewPlan::Render);
        assert_eq!(plan(FileKind::Sidecar, "xmp"), PreviewPlan::None);
        assert_eq!(plan(FileKind::Other, "txt"), PreviewPlan::None);
    }
}
