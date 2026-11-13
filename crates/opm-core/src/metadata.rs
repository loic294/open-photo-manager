//! Pure metadata logic: camera names, keyword flattening, rating and flag, and the field-by-field
//! merge of embedded metadata with XMP sidecars (plan 5.4, A17). No I/O.
//!
//! Owned by todo `extracting-metadata`; the exiftool and XMP readers in `opm-server` only produce
//! [`SourceMetadata`] values and the rules here decide what a file ends up with.
//!
//! # Merge ([`merge`])
//! Every source is a partial [`SourceMetadata`]; each field is taken from the first source that
//! HAS it, in the order exact-kind sidecars, stem-kind sidecars, embedded (the caller orders the
//! sidecars: by `name_norm` inside each kind). Present-but-empty counts as present: a sidecar
//! with an empty keyword list clears the embedded keywords, while an absent field falls through.
//! The camera (make and model) is one field: the first source with either of them supplies both.
//!
//! Dates have two slots (plan 5.4):
//! * a sidecar's `exif:DateTimeOriginal` REPLACES the embedded `DateTimeOriginal` slot (a
//!   Lightroom "Edit capture time" wins), and then the embedded sub-second value and offset, which
//!   belonged to the replaced reading, are dropped; [`Merged::original_from_sidecar`] records it so
//!   the caller can report `capture_src = xmp`;
//! * a sidecar's `xmp:CreateDate` / `photoshop:DateCreated` become [`SourceMetadata::sidecar_date`],
//!   the "sidecar date" slot of `datetime::resolve_capture`.

/// Longest camera name, keyword and tag kept.
pub const MAX_NAME_CHARS: usize = 128;
/// Most keywords kept per file.
pub const MAX_KEYWORDS: usize = 500;

/// What one source (embedded metadata or one sidecar) says about a file. `None` means the source
/// does not have the field; `Some` of an empty value is an explicit "empty".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceMetadata {
    /// `xmp:Rating`, -1 = rejected.
    pub rating: Option<i64>,
    /// `xmpDM:pick`, 1 selected, -1 rejected.
    pub pick: Option<i64>,
    /// Flat keywords (see [`flatten_keywords`]); `Some(vec![])` is an explicit empty list.
    pub keywords: Option<Vec<String>>,
    pub description: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    /// Raw `DateTimeOriginal` text (local wall clock).
    pub date_time_original: Option<String>,
    pub sub_sec_date_time_original: Option<String>,
    pub offset_time_original: Option<String>,
    /// Raw EXIF/XMP `CreateDate` text.
    pub create_date: Option<String>,
    /// Raw `photoshop:DateCreated` text (sidecars).
    pub date_created: Option<String>,
    /// QuickTime `Keys:CreationDate` (carries its offset).
    pub quicktime_creation_date: Option<String>,
    /// QuickTime `CreateDate` (UTC).
    pub quicktime_create_date: Option<String>,
    pub media_create_date: Option<String>,
    /// Only set by [`merge`]: the date of the first sidecar that has a `CreateDate` or `DateCreated`.
    pub sidecar_date: Option<String>,
}

/// The result of [`merge`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Merged {
    pub meta: SourceMetadata,
    /// The `DateTimeOriginal` slot comes from a sidecar.
    pub original_from_sidecar: bool,
}

fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
    {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn starts_with_word(model: &str, make: &str) -> bool {
    let mut model_chars = model.chars();
    for expected in make.chars() {
        match model_chars.next() {
            Some(c) if c.to_lowercase().eq(expected.to_lowercase()) => {}
            _ => return false,
        }
    }
    model_chars
        .next()
        .is_none_or(|next| !next.is_alphanumeric())
}

/// A display name for a camera: `"{make} {model}"`, or the model alone when it already starts
/// with the make or with its first word (`NIKON CORPORATION`), as a whole word, case-insensitive. Control characters and NUL padding are
/// stripped, whitespace collapsed, at most [`MAX_NAME_CHARS`] characters; `None` when empty.
pub fn normalize_camera(make: Option<&str>, model: Option<&str>) -> Option<String> {
    let make = clean(make.unwrap_or_default());
    let model = clean(model.unwrap_or_default());
    let name = match (make.is_empty(), model.is_empty()) {
        (true, true) => return None,
        (false, true) => make,
        (true, false) => model,
        (false, false)
            if starts_with_word(&model, &make)
                || make
                    .split_once(' ')
                    .is_some_and(|(brand, _)| starts_with_word(&model, brand)) =>
        {
            model
        }
        (false, false) => format!("{make} {model}"),
    };
    Some(name.chars().take(MAX_NAME_CHARS).collect())
}

/// Flat, de-duplicated keywords. With `hierarchical` every value (`A|B|C`) is split on `|` into
/// its components. Values are trimmed, empty ones and ones over [`MAX_NAME_CHARS`] dropped,
/// duplicates (case-insensitive) keep the first spelling and first-seen order, at most
/// [`MAX_KEYWORDS`] are returned.
pub fn flatten_keywords<S: AsRef<str>>(values: &[S], hierarchical: bool) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut push = |item: &str| {
        let item = item.trim();
        if item.is_empty() || item.chars().count() > MAX_NAME_CHARS || out.len() >= MAX_KEYWORDS {
            return;
        }
        if seen.insert(item.to_lowercase()) {
            out.push(item.to_owned());
        }
    };
    for value in values {
        let value = value.as_ref();
        if hierarchical {
            value.split('|').for_each(&mut push);
        } else {
            push(value);
        }
    }
    out
}

/// Parses "3", "-1" or "3.0" (a whole number written with a zero fraction); anything else is `None`.
pub fn parse_integer_lenient(text: &str) -> Option<i64> {
    let text = text.trim();
    let whole = match text.split_once('.') {
        Some((whole, frac)) if !frac.is_empty() && frac.bytes().all(|b| b == b'0') => whole,
        Some(_) => return None,
        None => text,
    };
    whole.parse().ok()
}

/// The stored rating (0..=5) and flag (-1, 0, 1). A rating of -1 means rejected (rating 0); an
/// explicit pick wins over that.
pub fn rating_and_flag(rating: Option<i64>, pick: Option<i64>) -> (u8, i8) {
    let rejected = rating == Some(-1);
    let stars = u8::try_from(rating.unwrap_or(0).clamp(0, 5)).unwrap_or(0);
    let flag = match pick {
        Some(1) => 1,
        Some(-1) => -1,
        _ if rejected => -1,
        _ => 0,
    };
    (stars, flag)
}

/// Merges `embedded` with `sidecars` field by field; see the module docs. `sidecars` must already
/// be in precedence order (exact-kind before stem-kind, each ordered by `name_norm`).
pub fn merge(embedded: &SourceMetadata, sidecars: &[&SourceMetadata]) -> Merged {
    fn first<T: Clone>(
        sidecars: &[&SourceMetadata],
        embedded: &SourceMetadata,
        field: impl Fn(&SourceMetadata) -> &Option<T>,
    ) -> Option<T> {
        sidecars
            .iter()
            .copied()
            .chain(std::iter::once(embedded))
            .find_map(|source| field(source).clone())
    }

    let camera = sidecars
        .iter()
        .copied()
        .chain(std::iter::once(embedded))
        .find(|source| source.make.is_some() || source.model.is_some());
    let sidecar_original = sidecars
        .iter()
        .find(|source| source.date_time_original.is_some());
    let sidecar_date = sidecars
        .iter()
        .find_map(|source| source.create_date.clone().or(source.date_created.clone()));

    let mut meta = SourceMetadata {
        rating: first(sidecars, embedded, |s| &s.rating),
        pick: first(sidecars, embedded, |s| &s.pick),
        keywords: first(sidecars, embedded, |s| &s.keywords),
        description: first(sidecars, embedded, |s| &s.description),
        make: camera.and_then(|source| source.make.clone()),
        model: camera.and_then(|source| source.model.clone()),
        date_time_original: embedded.date_time_original.clone(),
        sub_sec_date_time_original: embedded.sub_sec_date_time_original.clone(),
        offset_time_original: embedded.offset_time_original.clone(),
        create_date: embedded.create_date.clone(),
        date_created: embedded.date_created.clone(),
        quicktime_creation_date: embedded.quicktime_creation_date.clone(),
        quicktime_create_date: embedded.quicktime_create_date.clone(),
        media_create_date: embedded.media_create_date.clone(),
        sidecar_date,
    };
    if let Some(source) = sidecar_original {
        meta.date_time_original = source.date_time_original.clone();
        meta.sub_sec_date_time_original = None;
        meta.offset_time_original = None;
    }
    Merged {
        meta,
        original_from_sidecar: sidecar_original.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    #[test]
    fn camera_names() {
        let cases = [
            (Some("Canon"), Some("Canon EOS R5"), Some("Canon EOS R5")),
            (
                Some("NIKON CORPORATION"),
                Some("NIKON D850"),
                Some("NIKON D850"),
            ),
            (Some("SONY"), Some("ILCE-7M3"), Some("SONY ILCE-7M3")),
            (
                Some("Apple"),
                Some("iPhone 15 Pro"),
                Some("Apple iPhone 15 Pro"),
            ),
            (
                Some("Canon"),
                Some("Canonet QL17"),
                Some("Canon Canonet QL17"),
            ),
            (Some("canon"), Some("CANON EOS 5D"), Some("CANON EOS 5D")),
            (Some("Canon"), Some("Canon"), Some("Canon")),
            (
                Some("Canon\0\0  "),
                Some("  Canon  EOS \t R5\0\0\0"),
                Some("Canon EOS R5"),
            ),
            (Some("FUJIFILM"), None, Some("FUJIFILM")),
            (None, Some("X100V"), Some("X100V")),
            (Some(" \0 "), Some(""), None),
            (None, None, None),
        ];
        for (make, model, expected) in cases {
            assert_eq!(
                normalize_camera(make, model).as_deref(),
                expected,
                "{make:?} {model:?}"
            );
        }
    }

    #[test]
    fn camera_names_are_capped() {
        let long = "m".repeat(300);
        assert_eq!(
            normalize_camera(None, Some(&long)).unwrap().chars().count(),
            MAX_NAME_CHARS
        );
    }

    #[test]
    fn keywords_flat_dedupe_and_caps() {
        assert_eq!(
            flatten_keywords(&["Paris", " paris ", "", "Rome", "PARIS"], false),
            ["Paris", "Rome"]
        );
        assert_eq!(
            flatten_keywords(&["Places|France|Paris", "Places|Italy", "france"], true),
            ["Places", "France", "Paris", "Italy"]
        );
        assert_eq!(flatten_keywords(&["a|b"], false), ["a|b"]);
        assert_eq!(
            flatten_keywords(&["|x||", " | "], true),
            ["x"],
            "empty components dropped"
        );
        let long = "k".repeat(MAX_NAME_CHARS + 1);
        let ok = "k".repeat(MAX_NAME_CHARS);
        assert_eq!(
            flatten_keywords(&[long.as_str(), ok.as_str()], false),
            std::slice::from_ref(&ok)
        );
        let many: Vec<String> = (0..800).map(|n| format!("tag{n}")).collect();
        let kept = flatten_keywords(&many, false);
        assert_eq!(kept.len(), MAX_KEYWORDS);
        assert_eq!(kept[0], "tag0");
    }

    #[test]
    fn rating_and_flag_matrix() {
        let cases = [
            (None, None, (0, 0)),
            (Some(3), None, (3, 0)),
            (Some(0), None, (0, 0)),
            (Some(9), None, (5, 0)),
            (Some(-5), None, (0, 0)),
            (Some(-1), None, (0, -1)),
            (Some(4), Some(1), (4, 1)),
            (Some(4), Some(-1), (4, -1)),
            (Some(4), Some(0), (4, 0)),
            (Some(-1), Some(1), (0, 1)),
            (Some(-1), Some(0), (0, -1)),
            (None, Some(1), (0, 1)),
            (None, Some(7), (0, 0)),
        ];
        for (rating, pick, expected) in cases {
            assert_eq!(
                rating_and_flag(rating, pick),
                expected,
                "{rating:?} {pick:?}"
            );
        }
    }

    fn with_rating(rating: i64) -> SourceMetadata {
        SourceMetadata {
            rating: Some(rating),
            ..SourceMetadata::default()
        }
    }

    #[test]
    fn merge_precedence_is_field_by_field() {
        let embedded = SourceMetadata {
            rating: Some(1),
            pick: Some(1),
            keywords: Some(vec!["emb".into()]),
            make: s("Canon"),
            model: s("EOS R5"),
            ..SourceMetadata::default()
        };
        let exact = SourceMetadata {
            rating: Some(5),
            ..SourceMetadata::default()
        };
        let stem = SourceMetadata {
            rating: Some(3),
            keywords: Some(vec!["stem".into()]),
            ..SourceMetadata::default()
        };
        let merged = merge(&embedded, &[&exact, &stem]).meta;
        assert_eq!(merged.rating, Some(5), "exact beats stem and embedded");
        assert_eq!(merged.keywords, Some(vec!["stem".to_owned()]));
        assert_eq!(merged.pick, Some(1), "absent falls through to embedded");
        assert_eq!(merged.model, s("EOS R5"));

        let merged = merge(&embedded, &[&stem, &exact]).meta;
        assert_eq!(
            merged.rating,
            Some(3),
            "the caller's order is the precedence"
        );
        assert_eq!(merge(&embedded, &[]).meta.rating, Some(1));
        assert_eq!(
            merge(&SourceMetadata::default(), &[]).meta,
            SourceMetadata::default()
        );
    }

    #[test]
    fn an_explicitly_empty_value_counts_as_present() {
        let embedded = SourceMetadata {
            keywords: Some(vec!["emb".into()]),
            rating: Some(4),
            ..SourceMetadata::default()
        };
        let cleared = SourceMetadata {
            keywords: Some(Vec::new()),
            rating: Some(0),
            ..SourceMetadata::default()
        };
        let merged = merge(&embedded, &[&cleared]).meta;
        assert_eq!(merged.keywords, Some(Vec::new()));
        assert_eq!(merged.rating, Some(0));
        let silent = SourceMetadata::default();
        assert_eq!(
            merge(&embedded, &[&silent]).meta.keywords,
            Some(vec!["emb".to_owned()])
        );
    }

    #[test]
    fn the_camera_is_one_field() {
        let embedded = SourceMetadata {
            make: s("Canon"),
            model: s("EOS R5"),
            ..SourceMetadata::default()
        };
        let sidecar = SourceMetadata {
            model: s("Other"),
            ..SourceMetadata::default()
        };
        let merged = merge(&embedded, &[&sidecar]).meta;
        assert_eq!((merged.make, merged.model), (None, s("Other")));
    }

    #[test]
    fn a_sidecar_original_date_replaces_the_embedded_slot() {
        let embedded = SourceMetadata {
            date_time_original: s("2020:01:01 10:00:00"),
            sub_sec_date_time_original: s("2020:01:01 10:00:00.123"),
            offset_time_original: s("+01:00"),
            create_date: s("2020:01:02 10:00:00"),
            ..SourceMetadata::default()
        };
        let sidecar = SourceMetadata {
            date_time_original: s("2021-05-05T12:00:00"),
            create_date: s("2021-05-06T12:00:00"),
            date_created: s("2021-05-07"),
            ..SourceMetadata::default()
        };
        let merged = merge(&embedded, &[&sidecar]);
        assert!(merged.original_from_sidecar);
        assert_eq!(merged.meta.date_time_original, s("2021-05-05T12:00:00"));
        assert_eq!(merged.meta.sub_sec_date_time_original, None);
        assert_eq!(merged.meta.offset_time_original, None);
        assert_eq!(merged.meta.sidecar_date, s("2021-05-06T12:00:00"));
        assert_eq!(
            merged.meta.create_date,
            s("2020:01:02 10:00:00"),
            "the embedded CreateDate keeps its own slot"
        );

        let only_created = SourceMetadata {
            date_created: s("2021-05-07"),
            ..SourceMetadata::default()
        };
        let merged = merge(&embedded, &[&only_created]);
        assert!(!merged.original_from_sidecar);
        assert_eq!(merged.meta.date_time_original, s("2020:01:01 10:00:00"));
        assert_eq!(merged.meta.sidecar_date, s("2021-05-07"));
    }

    #[test]
    fn ratings_merge_into_the_stored_columns() {
        let merged = merge(&with_rating(2), &[&with_rating(-1)]).meta;
        assert_eq!(rating_and_flag(merged.rating, merged.pick), (0, -1));
    }
}
