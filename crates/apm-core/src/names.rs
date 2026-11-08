//! File-name helpers shared by the scanner, the sidecar matcher, the duplicate-matching modes,
//! the conflict namer and the folder-pattern formatter. Keep every name comparison going through here.

use std::cmp::Ordering;

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

/// The value stored in the `stem_norm` column: [`normalize_name`] of the stem of `name` (see
/// [`split_name`]). `IMG_0001.CR3` gives `img_0001`, `IMG_0001.CR3.xmp` gives `img_0001.cr3`.
pub fn stem_norm(name: &str) -> String {
    normalize_name(split_name(name).0)
}

/// Lowercased extension without the dot; empty when the name has none.
pub fn extension_lower(name: &str) -> String {
    split_name(name)
        .1
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Compares names in a natural order: ASCII digit runs compare by numeric value, and other
/// characters compare by Unicode code point. Equal numeric runs use their length as a tie-breaker
/// so leading-zero variants remain totally ordered.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a_char), Some(b_char)) if a_char.is_ascii_digit() && b_char.is_ascii_digit() => {
                let a_run = digit_run(&mut left);
                let b_run = digit_run(&mut right);
                let a_significant = a_run.trim_start_matches('0');
                let b_significant = b_run.trim_start_matches('0');
                let order = a_significant
                    .len()
                    .cmp(&b_significant.len())
                    .then_with(|| a_significant.cmp(b_significant))
                    .then_with(|| a_run.len().cmp(&b_run.len()));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(a_char), Some(b_char)) => {
                left.next();
                right.next();
                let order = a_char.cmp(&b_char);
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

fn digit_run(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut digits = String::new();
    while characters.peek().is_some_and(char::is_ascii_digit) {
        if let Some(digit) = characters.next() {
            digits.push(digit);
        }
    }
    digits
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
    fn stem_norm_normalises_the_stem_only() {
        assert_eq!(stem_norm("IMG_0001.CR3"), "img_0001");
        assert_eq!(stem_norm("IMG_0001.CR3.xmp"), "img_0001.cr3");
        assert_eq!(stem_norm(".Bashrc"), ".bashrc");
        assert_eq!(stem_norm("noext"), "noext");
        assert_eq!(
            stem_norm("e\u{301}te\u{301}.JPG"),
            stem_norm("\u{e9}t\u{e9}.jpg")
        );
    }

    #[test]
    fn extension_is_lowercased() {
        assert_eq!(extension_lower("A.JPG"), "jpg");
        assert_eq!(extension_lower("A"), "");
        assert_eq!(extension_lower(".hidden"), "");
    }

    #[test]
    fn natural_order_compares_digit_runs_and_leading_zeroes() {
        assert_eq!(natural_cmp("IMG_2.jpg", "IMG_10.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("IMG_1.jpg", "IMG_01.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("IMG_01.jpg", "IMG_001.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("A", "a"), Ordering::Less);
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

        #[test]
        fn natural_order_is_antisymmetric(a in "\\PC{0,30}", b in "\\PC{0,30}") {
            prop_assert_eq!(natural_cmp(&a, &b), natural_cmp(&b, &a).reverse());
        }

        #[test]
        fn natural_order_is_transitive(a in "\\PC{0,30}", b in "\\PC{0,30}", c in "\\PC{0,30}") {
            if natural_cmp(&a, &b) != Ordering::Greater && natural_cmp(&b, &c) != Ordering::Greater {
                prop_assert_ne!(natural_cmp(&a, &c), Ordering::Greater);
            }
        }
    }
}
