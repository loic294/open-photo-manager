//! File classification: maps a file name or extension to a [`FileKind`].
//!
//! # Contract
//! - [`FileKind`] has exactly the values of the `files.kind` column (`image`, `video`, `audio`,
//!   `sidecar`, `other`; [`FileKind::as_str`] and [`FromStr`] convert to and from that column).
//!   RAW formats are `Image`; there is no separate RAW kind, [`is_raw`] tells them apart.
//! - [`EXTENSIONS`] is the ONLY place where media extensions are listed. Do not copy it.
//! - Precedence in [`classify`]: a media extension always wins, then an extension of the configured
//!   sidecar list gives `Sidecar`, everything else (including files without an extension) is
//!   `Other`. So `xml` is a sidecar by default but never an image, and putting `jpg` in the sidecar
//!   list does not turn photos into sidecars.
//! - Extension comparison is ASCII case-insensitive and never allocates. Names go through
//!   [`names::split_name`](crate::names::split_name), so dotfiles (`.jpg`) and trailing-dot names
//!   have no extension.
//!
//! Whether a file is junk (`.DS_Store`, `._IMG.jpg`) is decided by [`junk`](crate::junk) BEFORE
//! classification; this module only looks at the extension.

use std::fmt;
use std::str::FromStr;

use crate::dto::FileKind;
use crate::names::split_name;

/// What a known media extension is. RAW is `FileKind::Image` on the wire; the distinction only
/// matters for decoding and thumbnails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaFormat {
    /// A regular still-image format (JPEG, PNG, HEIC, ...).
    Image,
    /// A camera RAW format (CR3, NEF, ARW, DNG, ...).
    Raw,
    Video,
    Audio,
}

impl MediaFormat {
    /// The `FileKind` stored for this format (RAW is an image).
    pub const fn kind(self) -> FileKind {
        match self {
            Self::Image | Self::Raw => FileKind::Image,
            Self::Video => FileKind::Video,
            Self::Audio => FileKind::Audio,
        }
    }
}

/// The single extension table: lowercase extension without the dot, and its format.
pub const EXTENSIONS: &[(&str, MediaFormat)] = &[
    ("jpg", MediaFormat::Image),
    ("jpeg", MediaFormat::Image),
    ("png", MediaFormat::Image),
    ("gif", MediaFormat::Image),
    ("webp", MediaFormat::Image),
    ("tif", MediaFormat::Image),
    ("tiff", MediaFormat::Image),
    ("bmp", MediaFormat::Image),
    ("heic", MediaFormat::Image),
    ("heif", MediaFormat::Image),
    ("avif", MediaFormat::Image),
    ("jxl", MediaFormat::Image),
    ("cr2", MediaFormat::Raw),
    ("cr3", MediaFormat::Raw),
    ("crw", MediaFormat::Raw),
    ("nef", MediaFormat::Raw),
    ("nrw", MediaFormat::Raw),
    ("arw", MediaFormat::Raw),
    ("srf", MediaFormat::Raw),
    ("sr2", MediaFormat::Raw),
    ("dng", MediaFormat::Raw),
    ("raf", MediaFormat::Raw),
    ("orf", MediaFormat::Raw),
    ("rw2", MediaFormat::Raw),
    ("pef", MediaFormat::Raw),
    ("srw", MediaFormat::Raw),
    ("x3f", MediaFormat::Raw),
    ("3fr", MediaFormat::Raw),
    ("erf", MediaFormat::Raw),
    ("mrw", MediaFormat::Raw),
    ("dcr", MediaFormat::Raw),
    ("kdc", MediaFormat::Raw),
    ("raw", MediaFormat::Raw),
    ("mp4", MediaFormat::Video),
    ("m4v", MediaFormat::Video),
    ("mov", MediaFormat::Video),
    ("avi", MediaFormat::Video),
    ("mkv", MediaFormat::Video),
    ("webm", MediaFormat::Video),
    ("mts", MediaFormat::Video),
    ("m2ts", MediaFormat::Video),
    ("mpg", MediaFormat::Video),
    ("mpeg", MediaFormat::Video),
    ("wmv", MediaFormat::Video),
    ("3gp", MediaFormat::Video),
    ("mxf", MediaFormat::Video),
    ("mp3", MediaFormat::Audio),
    ("m4a", MediaFormat::Audio),
    ("aac", MediaFormat::Audio),
    ("wav", MediaFormat::Audio),
    ("flac", MediaFormat::Audio),
    ("ogg", MediaFormat::Audio),
    ("opus", MediaFormat::Audio),
];

/// The media format of `ext` (extension without the dot, any ASCII case), or `None` when it is not
/// a known media extension.
pub fn media_format(ext: &str) -> Option<MediaFormat> {
    EXTENSIONS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(ext))
        .map(|&(_, format)| format)
}

/// Whether `ext` (without the dot, any ASCII case) is a camera RAW extension.
pub fn is_raw(ext: &str) -> bool {
    media_format(ext) == Some(MediaFormat::Raw)
}

/// Classifies an extension (without the dot; ASCII case does not matter, the `files.ext` column is
/// lowercase) against the media tables and the configured sidecar extensions. See the module docs
/// for the precedence rules.
pub fn classify<S: AsRef<str>>(ext_lower: &str, sidecar_exts: &[S]) -> FileKind {
    if let Some(format) = media_format(ext_lower) {
        return format.kind();
    }
    let is_sidecar = !ext_lower.is_empty()
        && sidecar_exts
            .iter()
            .any(|ext| ext.as_ref().eq_ignore_ascii_case(ext_lower));
    if is_sidecar {
        FileKind::Sidecar
    } else {
        FileKind::Other
    }
}

/// Classifies a file name by its extension (the part after the last dot, see
/// [`names::split_name`](crate::names::split_name)). A name without an extension is `Other`.
pub fn classify_name<S: AsRef<str>>(name: &str, sidecar_exts: &[S]) -> FileKind {
    match split_name(name).1 {
        Some(ext) => classify(ext, sidecar_exts),
        None => FileKind::Other,
    }
}

/// Error returned when a string is not a `files.kind` value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown file kind `{0}`")]
pub struct ParseFileKindError(pub String);

impl FileKind {
    /// Every kind, in the order the columns and the UI list them.
    pub const ALL: [FileKind; 5] = [
        FileKind::Image,
        FileKind::Video,
        FileKind::Audio,
        FileKind::Sidecar,
        FileKind::Other,
    ];

    /// The `files.kind` column value; identical to the JSON wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Sidecar => "sidecar",
            Self::Other => "other",
        }
    }

    /// Whether the file is a photo, a video or an audio file (what the duplicate finder and the
    /// browse grids work on); sidecars and other files are not media.
    pub const fn is_media(self) -> bool {
        matches!(self, Self::Image | Self::Video | Self::Audio)
    }
}

impl fmt::Display for FileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FileKind {
    type Err = ParseFileKindError;

    /// Parses exactly the strings produced by [`FileKind::as_str`] (lowercase).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        FileKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| ParseFileKindError(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::consts::DEFAULT_SIDECAR_EXTENSIONS;
    use crate::names::extension_lower;

    const NONE: &[&str] = &[];

    fn exts_of(format: MediaFormat) -> Vec<&'static str> {
        EXTENSIONS
            .iter()
            .filter(|&&(_, f)| f == format)
            .map(|&(ext, _)| ext)
            .collect()
    }

    #[test]
    fn table_entries_are_lowercase_ascii_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (ext, _) in EXTENSIONS {
            assert!(!ext.is_empty(), "empty extension");
            assert!(
                ext.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
                "{ext} must be lowercase ASCII without a dot"
            );
            assert!(seen.insert(*ext), "{ext} is listed twice");
        }
    }

    #[test]
    fn table_holds_exactly_the_documented_extensions() {
        assert_eq!(
            exts_of(MediaFormat::Image),
            "jpg jpeg png gif webp tif tiff bmp heic heif avif jxl"
                .split(' ')
                .collect::<Vec<_>>()
        );
        assert_eq!(
            exts_of(MediaFormat::Raw),
            "cr2 cr3 crw nef nrw arw srf sr2 dng raf orf rw2 pef srw x3f 3fr erf mrw dcr kdc raw"
                .split(' ')
                .collect::<Vec<_>>()
        );
        assert_eq!(
            exts_of(MediaFormat::Video),
            "mp4 m4v mov avi mkv webm mts m2ts mpg mpeg wmv 3gp mxf"
                .split(' ')
                .collect::<Vec<_>>()
        );
        assert_eq!(
            exts_of(MediaFormat::Audio),
            "mp3 m4a aac wav flac ogg opus"
                .split(' ')
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn images_raw_video_and_audio_are_classified() {
        for ext in ["jpg", "jpeg", "png", "heic", "avif", "jxl", "tiff"] {
            assert_eq!(classify(ext, NONE), FileKind::Image, "{ext}");
        }
        for ext in [
            "cr2", "cr3", "nef", "arw", "dng", "raf", "x3f", "3fr", "raw",
        ] {
            assert_eq!(classify(ext, NONE), FileKind::Image, "{ext}");
            assert!(is_raw(ext), "{ext}");
        }
        for ext in ["mp4", "mov", "mkv", "m2ts", "3gp", "mxf"] {
            assert_eq!(classify(ext, NONE), FileKind::Video, "{ext}");
        }
        for ext in ["mp3", "flac", "opus", "m4a"] {
            assert_eq!(classify(ext, NONE), FileKind::Audio, "{ext}");
        }
    }

    #[test]
    fn only_raw_formats_are_raw() {
        assert!(is_raw("CR3"));
        assert!(!is_raw("jpg"));
        assert!(!is_raw("mp4"));
        assert!(!is_raw("xmp"));
        assert!(!is_raw(""));
        assert_eq!(media_format("DNG"), Some(MediaFormat::Raw));
        assert_eq!(media_format("png"), Some(MediaFormat::Image));
        assert_eq!(media_format("xmp"), None);
    }

    #[test]
    fn matching_ignores_ascii_case() {
        assert_eq!(classify("JPG", NONE), FileKind::Image);
        assert_eq!(classify("Mp4", NONE), FileKind::Video);
        assert_eq!(
            classify("XMP", DEFAULT_SIDECAR_EXTENSIONS),
            FileKind::Sidecar
        );
        assert_eq!(classify("xmp", &["XMP".to_owned()]), FileKind::Sidecar);
    }

    #[test]
    fn configured_sidecar_extensions_are_sidecars() {
        for ext in DEFAULT_SIDECAR_EXTENSIONS {
            assert_eq!(classify(ext, DEFAULT_SIDECAR_EXTENSIONS), FileKind::Sidecar);
        }
        let custom = vec!["thm".to_owned(), "xmp".to_owned()];
        assert_eq!(classify("thm", &custom), FileKind::Sidecar);
        assert_eq!(classify("json", &custom), FileKind::Other);
        assert_eq!(classify("json", NONE), FileKind::Other);
    }

    #[test]
    fn media_extensions_win_over_the_sidecar_list() {
        let list = ["jpg", "mp4", "mp3", "cr3", "xml"];
        assert_eq!(classify("jpg", &list), FileKind::Image);
        assert_eq!(classify("cr3", &list), FileKind::Image);
        assert_eq!(classify("mp4", &list), FileKind::Video);
        assert_eq!(classify("mp3", &list), FileKind::Audio);
        assert_eq!(classify("xml", &list), FileKind::Sidecar);
    }

    #[test]
    fn unknown_and_empty_extensions_are_other() {
        assert_eq!(classify("", NONE), FileKind::Other);
        assert_eq!(classify("zip", DEFAULT_SIDECAR_EXTENSIONS), FileKind::Other);
        assert_eq!(classify("\u{e9}t\u{e9}", NONE), FileKind::Other);
        // A sloppy list with an empty entry must not turn extension-less files into sidecars.
        assert_eq!(classify("", &[""]), FileKind::Other);
    }

    #[test]
    fn names_are_classified_by_their_last_extension() {
        let ext = DEFAULT_SIDECAR_EXTENSIONS;
        assert_eq!(classify_name("IMG_0001.CR3", ext), FileKind::Image);
        assert_eq!(classify_name("IMG_0001.JPG", ext), FileKind::Image);
        assert_eq!(classify_name("IMG_0001.CR3.xmp", ext), FileKind::Sidecar);
        assert_eq!(classify_name("photo.jpg.json", ext), FileKind::Sidecar);
        assert_eq!(classify_name("clip.MOV", ext), FileKind::Video);
        assert_eq!(classify_name("song.flac", ext), FileKind::Audio);
        assert_eq!(classify_name("notes.txt", ext), FileKind::Other);
        assert_eq!(classify_name("archive.tar.gz", ext), FileKind::Other);
    }

    #[test]
    fn names_without_a_real_extension_are_other() {
        let ext = DEFAULT_SIDECAR_EXTENSIONS;
        assert_eq!(classify_name("README", ext), FileKind::Other);
        assert_eq!(classify_name(".jpg", ext), FileKind::Other);
        assert_eq!(classify_name(".xmp", ext), FileKind::Other);
        assert_eq!(classify_name("photo.", ext), FileKind::Other);
        assert_eq!(classify_name("", ext), FileKind::Other);
        assert_eq!(classify_name("IMG.jpg.apm-part", ext), FileKind::Other);
    }

    #[test]
    fn as_str_and_from_str_round_trip() {
        for kind in FileKind::ALL {
            assert_eq!(kind.as_str().parse::<FileKind>(), Ok(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(FileKind::Sidecar.as_str(), "sidecar");
    }

    #[test]
    fn from_str_is_strict() {
        for bad in ["", "Image", "IMAGE", " image", "image ", "raw", "photo"] {
            assert_eq!(
                bad.parse::<FileKind>(),
                Err(ParseFileKindError(bad.to_owned())),
                "{bad:?}"
            );
        }
        assert_eq!(
            "raw".parse::<FileKind>().unwrap_err().to_string(),
            "unknown file kind `raw`"
        );
    }

    #[test]
    fn db_strings_equal_the_wire_strings() {
        for kind in FileKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
        }
    }

    #[test]
    fn all_lists_every_variant_once() {
        for (index, kind) in FileKind::ALL.into_iter().enumerate() {
            let expected = match kind {
                FileKind::Image => 0,
                FileKind::Video => 1,
                FileKind::Audio => 2,
                FileKind::Sidecar => 3,
                FileKind::Other => 4,
            };
            assert_eq!(index, expected, "{kind}");
        }
    }

    #[test]
    fn only_image_video_and_audio_are_media() {
        let media: Vec<_> = FileKind::ALL.into_iter().filter(|k| k.is_media()).collect();
        assert_eq!(media, [FileKind::Image, FileKind::Video, FileKind::Audio]);
    }

    proptest! {
        #[test]
        fn classify_follows_the_precedence_rules(
            ext in "[a-zA-Z0-9]{0,6}",
            list in proptest::collection::vec("[a-z0-9]{1,5}", 0..6),
        ) {
            let kind = classify(&ext, &list);
            let in_list = list.iter().any(|e| e.eq_ignore_ascii_case(&ext));
            let expected = match media_format(&ext) {
                Some(format) => format.kind(),
                None if in_list && !ext.is_empty() => FileKind::Sidecar,
                None => FileKind::Other,
            };
            prop_assert_eq!(kind, expected);
        }

        #[test]
        fn classify_name_uses_the_lowercased_extension(name in "\\PC{0,20}") {
            let by_name = classify_name(&name, DEFAULT_SIDECAR_EXTENSIONS);
            let by_ext = classify(&extension_lower(&name), DEFAULT_SIDECAR_EXTENSIONS);
            prop_assert_eq!(by_name, by_ext);
        }
    }
}
