//! File-name helpers shared by the scanner, the sidecar matcher, the duplicate-matching modes,
//! the conflict namer and the folder-pattern formatter. Keep every name comparison going through here.

use unicode_normalization::UnicodeNormalization;

/// Splits `name` into `(stem, extension)` at the last dot. The extension has no leading dot and is
/// `None` when the name has no dot, only a leading dot (`.bashrc`) or a trailing dot (`photo.`).
/// `IMG_0001.CR3.xmp` gives `("IMG_0001.CR3", Some("xmp"))`.
pub fn split_name(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], Some(&name[i + 1..])),
        _ => (name, None),
    }
}

/// Canonical form for case-insensitive, normalisation-insensitive name comparison, and the value stored
/// in the `name_norm` / `stem_norm` columns: NFC, lowercased, NFC again (lowercasing can denormalise).
pub fn normalize_name(name: &str) -> String {
    name.nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect()
}

/// Lowercased extension without the dot; empty when the name has none.
pub fn extension_lower(name: &str) -> String {
    split_name(name)
        .1
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn splits_at_the_last_dot() {
        assert_eq!(split_name("IMG_0001.CR3"), ("IMG_0001", Some("CR3")));
        assert_eq!(
            split_name("IMG_0001.CR3.xmp"),
            ("IMG_0001.CR3", Some("xmp"))
        );
        assert_eq!(split_name("archive.tar.gz"), ("archive.tar", Some("gz")));
        assert_eq!(split_name("noext"), ("noext", None));
        assert_eq!(split_name(".bashrc"), (".bashrc", None));
        assert_eq!(split_name("photo."), ("photo.", None));
        assert_eq!(split_name(""), ("", None));
        assert_eq!(split_name("."), (".", None));
        assert_eq!(split_name("..jpg"), (".", Some("jpg")));
        assert_eq!(split_name("été.jpg"), ("été", Some("jpg")));
    }

    #[test]
    fn normalisation_unifies_case_and_unicode_forms() {
        let nfc = "\u{e9}t\u{e9}.JPG";
        let nfd = "e\u{301}te\u{301}.jpg";
        assert_ne!(nfc, nfd);
        assert_eq!(normalize_name(nfc), normalize_name(nfd));
        assert_eq!(normalize_name("Photo.JPG"), normalize_name("photo.jpg"));
        assert_ne!(normalize_name("IMG_0001"), normalize_name("IMG_00012"));
    }

    #[test]
    fn extension_is_lowercased() {
        assert_eq!(extension_lower("A.JPG"), "jpg");
        assert_eq!(extension_lower("A"), "");
        assert_eq!(extension_lower(".hidden"), "");
    }

    proptest! {
        #[test]
        fn split_then_join_restores_the_name(name in "\\PC{0,30}") {
            let (stem, ext) = split_name(&name);
            let rebuilt = match ext {
                Some(e) => format!("{stem}.{e}"),
                None => stem.to_owned(),
            };
            prop_assert_eq!(rebuilt, name);
        }

        #[test]
        fn normalize_is_idempotent(name in "\\PC{0,30}") {
            let once = normalize_name(&name);
            prop_assert_eq!(normalize_name(&once), once);
        }
    }
}
