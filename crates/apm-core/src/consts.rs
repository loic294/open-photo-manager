//! Constants that more than one component must agree on. Define them here, nowhere else.

/// Per-volume trash folder, `<volume root>/.apm-trash/<batch>/<original relative path>`.
/// Always excluded from scanning, listing and empty-folder detection; not user-configurable.
pub const TRASH_DIR: &str = ".apm-trash";

/// Suffix of in-flight cross-volume copies (`name.apm-part`). Always ignored by the scanner.
pub const PART_SUFFIX: &str = ".apm-part";

/// Default value of the `sidecar_extensions` setting (lowercase, no dot).
pub const DEFAULT_SIDECAR_EXTENSIONS: &[&str] = &["xmp", "json", "aae", "dop", "pp3", "xml"];

/// Default value of the `junk_names` setting: file or folder names (compared case-insensitively) that
/// are never indexed and that do not count as content when deciding whether a folder is empty.
pub const DEFAULT_JUNK_NAMES: &[&str] = &[
    "@eaDir",
    "#recycle",
    "#snapshot",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
    "$RECYCLE.BIN",
    "System Volume Information",
];

/// Default value of the `junk_prefixes` setting (AppleDouble resource forks).
pub const DEFAULT_JUNK_PREFIXES: &[&str] = &["._"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_defaults_are_lowercase_without_dots() {
        for ext in DEFAULT_SIDECAR_EXTENSIONS {
            assert!(!ext.is_empty() && !ext.starts_with('.'), "{ext}");
            assert_eq!(*ext, ext.to_lowercase(), "{ext}");
        }
    }

    #[test]
    fn junk_defaults_are_single_path_components() {
        for name in DEFAULT_JUNK_NAMES.iter().chain(DEFAULT_JUNK_PREFIXES) {
            assert!(!name.is_empty() && !name.contains('/'), "{name}");
        }
    }

    #[test]
    fn trash_and_part_markers_are_distinct_hidden_names() {
        assert!(TRASH_DIR.starts_with('.'));
        assert!(PART_SUFFIX.starts_with('.'));
        assert_ne!(TRASH_DIR, PART_SUFFIX);
    }
}
