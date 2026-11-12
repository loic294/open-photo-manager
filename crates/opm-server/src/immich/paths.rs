//! The one translation between the paths Immich knows and the files this application indexes.
//!
//! Immich indexes an external library by the paths *inside the Immich container*, for example
//! `/mnt/photo/2019/IMG_1.jpg`. This application sees the same file as `2019/IMG_1.jpg` of a volume.
//! A mapping pairs a volume with the path prefix Immich uses for the root of that volume
//! (`/mnt/photo`). [`PathMap`] is built from the stored mappings and is THE translation: the asset
//! sync (Immich path to file), the outbox (file to Immich path), the badges and the prefix detection
//! all use it, and none of them joins or splits such strings itself.
//!
//! # Rules
//!
//! * A prefix is an absolute path with no trailing `/`, no `//` and no `.` or `..` component, or `/`
//!   alone, which is only allowed as the sole mapping. [`validate`] enforces it, together with the
//!   rule that no two prefixes are equal or inside one another: the reverse lookup is a longest
//!   prefix match, and an ambiguity there would send a delete to the wrong file.
//! * Matching happens on path-segment boundaries: `/mnt/photo` contains `/mnt/photo/a.jpg` and is
//!   the root itself, but does not contain `/mnt/photos/a.jpg`.
//! * Nothing is normalised. Immich compares paths byte for byte and macOS file names are NFD, so a
//!   name is carried through exactly as it is (the same rule as `RelPath`).
//! * A path coming from Immich is untrusted: [`PathMap::from_immich`] refuses relative paths, NUL
//!   bytes and empty, `.` or `..` segments instead of guessing, and tolerates one trailing `/`.

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;

use opm_core::dto::{FieldError, ImmichMappingEntry};
use opm_core::paths::{RelPath, VolumeId};

use super::client::is_safe_id;

/// The longest prefix accepted, in bytes (Linux `PATH_MAX`).
const MAX_PREFIX_BYTES: usize = 4096;

/// At most this many mappings are examined: there is one per volume, and nobody has hundreds.
const MAX_MAPPINGS: usize = 256;

/// The mapping of volumes to the paths Immich uses for them. Cheap to build and to clone; obtain it
/// from `ImmichService::path_map`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathMap {
    /// Longest prefix first. A valid set has no nesting, so at most one entry contains a path; the
    /// order keeps the lookup correct even if that rule were ever bypassed.
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    volume: VolumeId,
    /// `/` or an absolute path without a trailing slash.
    prefix: String,
    library: Option<String>,
}

impl Entry {
    /// What is left of `body` (an Immich path without its slashes at both ends) below this prefix:
    /// `""` for the root of the volume, `None` when `body` is not inside it.
    fn rest_of<'a>(&self, body: &'a str) -> Option<&'a str> {
        let own = &self.prefix[1..];
        if own.is_empty() {
            Some(body)
        } else if body == own {
            Some("")
        } else {
            body.strip_prefix(own)?.strip_prefix('/')
        }
    }
}

impl PathMap {
    /// Builds the map of `mappings`. A set that [`validate`] refuses (without insisting on a
    /// library) gives the problems instead.
    pub fn new(mappings: &[ImmichMappingEntry]) -> Result<Self, Vec<FieldError>> {
        let problems = validate(mappings, false);
        if !problems.is_empty() {
            return Err(problems);
        }
        let mut entries: Vec<Entry> = mappings
            .iter()
            .map(|mapping| Entry {
                volume: mapping.volume_id,
                prefix: mapping.path_prefix.clone(),
                library: mapping.library_id.clone(),
            })
            .collect();
        entries.sort_by(|a, b| {
            b.prefix
                .len()
                .cmp(&a.prefix.len())
                .then_with(|| a.prefix.cmp(&b.prefix))
        });
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The path Immich uses for `rel` of `volume`: the prefix, `/` and `rel`; the root of the
    /// volume is the prefix itself. `None` when the volume is not mapped.
    pub fn to_immich(&self, volume: VolumeId, rel: &RelPath) -> Option<String> {
        let entry = self.entry_of(volume)?;
        Some(if rel.is_root() {
            entry.prefix.clone()
        } else if entry.prefix == "/" {
            format!("/{rel}")
        } else {
            format!("{}/{rel}", entry.prefix)
        })
    }

    /// The volume and the path inside it that an Immich path stands for: the longest prefix that
    /// contains it on a path-segment boundary. `None` when no mapped volume contains it or when
    /// the path is not one Immich would produce (see the module docs).
    pub fn from_immich(&self, immich_path: &str) -> Option<(VolumeId, RelPath)> {
        let body = segments_of(immich_path)?;
        self.entries.iter().find_map(|entry| {
            let rel = RelPath::new(entry.rest_of(body)?).ok()?;
            Some((entry.volume, rel))
        })
    }

    /// The Immich library chosen for `volume`; `None` when the volume is not mapped or (limited
    /// mode) no library was chosen.
    pub fn library_for(&self, volume: VolumeId) -> Option<&str> {
        self.entry_of(volume)?.library.as_deref()
    }

    /// The path prefix Immich uses for the root of `volume`.
    pub fn prefix_for(&self, volume: VolumeId) -> Option<&str> {
        self.entry_of(volume).map(|entry| entry.prefix.as_str())
    }

    fn entry_of(&self, volume: VolumeId) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.volume == volume)
    }
}

/// The part of an absolute Immich path between its first and its (optional single) last slash:
/// empty for `/`. `None` for a path that is not absolute, contains a NUL, or has an empty, `.` or
/// `..` segment.
fn segments_of(path: &str) -> Option<&str> {
    if path.contains('\0') {
        return None;
    }
    let body = path.strip_prefix('/')?;
    if body.is_empty() {
        return Some(body);
    }
    let body = body.strip_suffix('/').unwrap_or(body);
    body.split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        .then_some(body)
}

/// Why `prefix` is not a valid path prefix, in words for the field message; `Ok` when it is.
pub(super) fn check_prefix(prefix: &str) -> Result<(), &'static str> {
    if prefix.is_empty() {
        return Err("is required: an absolute path such as /mnt/photo");
    }
    if prefix.len() > MAX_PREFIX_BYTES {
        return Err("is too long");
    }
    if prefix.chars().any(char::is_control) {
        return Err("must not contain control characters (such as NUL)");
    }
    if !prefix.starts_with('/') {
        return Err("must be an absolute path starting with `/`, such as /mnt/photo");
    }
    if prefix == "/" {
        return Ok(());
    }
    if prefix.ends_with('/') {
        return Err("must not end with `/`");
    }
    for segment in prefix[1..].split('/') {
        match segment {
            "" => return Err("must not contain `//`"),
            "." | ".." => return Err("must not contain `.` or `..` components"),
            _ => {}
        }
    }
    Ok(())
}

/// Checks a complete set of mappings as `PUT /api/immich/mappings` receives it and returns one
/// entry per problem, with the path of the field in the request (`mappings[1].path_prefix`).
///
/// Only what can be decided from the set itself is checked here; the service adds that the volumes
/// exist and (in full mode) that the libraries exist in Immich. `require_library` is true in full
/// mode, where every mapping needs a library.
pub fn validate(entries: &[ImmichMappingEntry], require_library: bool) -> Vec<FieldError> {
    if entries.len() > MAX_MAPPINGS {
        return vec![FieldError::new(
            "mappings",
            format!("at most {MAX_MAPPINGS} mappings are allowed"),
        )];
    }
    let mut problems = Vec::new();
    let mut first_of_volume: HashMap<VolumeId, usize> = HashMap::new();
    // Valid prefixes (not `/`), each with the index of its first user.
    let mut first_of_prefix: HashMap<&str, usize> = HashMap::new();

    for (index, entry) in entries.iter().enumerate() {
        let prefix_field = || format!("mappings[{index}].path_prefix");
        match check_prefix(&entry.path_prefix) {
            Err(reason) => problems.push(FieldError::new(prefix_field(), reason)),
            Ok(()) if entry.path_prefix == "/" => {
                if entries.len() > 1 {
                    problems.push(FieldError::new(
                        prefix_field(),
                        "`/` is only allowed when it is the only mapping",
                    ));
                }
            }
            Ok(()) => match first_of_prefix.entry(&entry.path_prefix) {
                MapEntry::Occupied(first) => problems.push(FieldError::new(
                    prefix_field(),
                    format!(
                        "is the same as mappings[{}].path_prefix: every volume needs its own path",
                        first.get()
                    ),
                )),
                MapEntry::Vacant(slot) => {
                    slot.insert(index);
                }
            },
        }

        match &entry.library_id {
            Some(id) if !is_safe_id(id) => problems.push(FieldError::new(
                format!("mappings[{index}].library_id"),
                "is not a valid Immich library id",
            )),
            None if require_library => problems.push(FieldError::new(
                format!("mappings[{index}].library_id"),
                "is required: choose the Immich library that holds this volume",
            )),
            _ => {}
        }

        match first_of_volume.entry(entry.volume_id) {
            MapEntry::Occupied(first) => problems.push(FieldError::new(
                format!("mappings[{index}].volume_id"),
                format!("volume is already mapped by mappings[{}]", first.get()),
            )),
            MapEntry::Vacant(slot) => {
                slot.insert(index);
            }
        }
    }

    // A prefix inside another one: look the ancestors of each prefix up instead of comparing all
    // pairs.
    for (index, entry) in entries.iter().enumerate() {
        let prefix = entry.path_prefix.as_str();
        if prefix == "/" || check_prefix(prefix).is_err() {
            continue;
        }
        let containing = prefix
            .match_indices('/')
            .skip(1)
            .find_map(|(cut, _)| first_of_prefix.get(&prefix[..cut]));
        if let Some(outer) = containing {
            problems.push(FieldError::new(
                format!("mappings[{index}].path_prefix"),
                format!(
                    "is inside mappings[{outer}].path_prefix: a path must not contain or be \
                     contained in another"
                ),
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn entry(volume: VolumeId, library: Option<&str>, prefix: &str) -> ImmichMappingEntry {
        ImmichMappingEntry {
            volume_id: volume,
            library_id: library.map(str::to_owned),
            path_prefix: prefix.to_owned(),
        }
    }

    fn map(entries: &[(VolumeId, &str)]) -> PathMap {
        let entries: Vec<_> = entries
            .iter()
            .map(|(volume, prefix)| entry(*volume, Some("lib"), prefix))
            .collect();
        PathMap::new(&entries).unwrap()
    }

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    fn fields(problems: &[FieldError]) -> Vec<&str> {
        problems.iter().map(|p| p.field.as_str()).collect()
    }

    #[test]
    fn a_file_maps_to_the_prefix_and_its_relative_path() {
        let map = map(&[(1, "/mnt/photo"), (2, "/mnt/video")]);
        assert_eq!(
            map.to_immich(1, &rel("2019/IMG_1.jpg")).as_deref(),
            Some("/mnt/photo/2019/IMG_1.jpg")
        );
        assert_eq!(
            map.to_immich(2, &rel("a.mp4")).as_deref(),
            Some("/mnt/video/a.mp4")
        );
        assert_eq!(
            map.to_immich(3, &rel("a.jpg")),
            None,
            "volume 3 is unmapped"
        );
    }

    #[test]
    fn the_root_of_a_volume_is_the_prefix_itself() {
        let map = map(&[(1, "/mnt/photo")]);
        assert_eq!(
            map.to_immich(1, &RelPath::root()).as_deref(),
            Some("/mnt/photo")
        );
        assert_eq!(
            map.from_immich("/mnt/photo"),
            Some((1, RelPath::root())),
            "the prefix is the root"
        );
        assert_eq!(map.from_immich("/mnt/photo/"), Some((1, RelPath::root())));
    }

    #[test]
    fn a_lone_slash_prefix_does_not_double_the_slash() {
        let map = map(&[(7, "/")]);
        assert_eq!(
            map.to_immich(7, &rel("a/b.jpg")).as_deref(),
            Some("/a/b.jpg")
        );
        assert_eq!(map.to_immich(7, &RelPath::root()).as_deref(), Some("/"));
        assert_eq!(map.from_immich("/a/b.jpg"), Some((7, rel("a/b.jpg"))));
        assert_eq!(map.from_immich("/"), Some((7, RelPath::root())));
    }

    #[test]
    fn matching_happens_on_segment_boundaries() {
        let map = map(&[(1, "/mnt/photo")]);
        assert_eq!(map.from_immich("/mnt/photos/x.jpg"), None);
        assert_eq!(map.from_immich("/mnt/photo-old/x.jpg"), None);
        assert_eq!(map.from_immich("/mnt/phot"), None);
        assert_eq!(map.from_immich("/mnt"), None);
        assert_eq!(map.from_immich("/other/mnt/photo/x.jpg"), None);
        assert_eq!(
            map.from_immich("/mnt/photo/photos/x.jpg"),
            Some((1, rel("photos/x.jpg")))
        );
    }

    #[test]
    fn sibling_prefixes_with_a_shared_start_stay_apart() {
        let map = map(&[(1, "/mnt/photo"), (2, "/mnt/photos")]);
        assert_eq!(
            map.from_immich("/mnt/photos/x.jpg"),
            Some((2, rel("x.jpg")))
        );
        assert_eq!(map.from_immich("/mnt/photo/x.jpg"), Some((1, rel("x.jpg"))));
    }

    #[test]
    fn a_trailing_slash_is_tolerated_on_input_but_empty_segments_are_not() {
        let map = map(&[(1, "/mnt/photo")]);
        assert_eq!(map.from_immich("/mnt/photo/a/"), Some((1, rel("a"))));
        for bad in [
            "/mnt/photo//a",
            "//mnt/photo/a",
            "/mnt/photo/a//",
            "//",
            "/mnt//photo",
        ] {
            assert_eq!(map.from_immich(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn dangerous_or_relative_input_is_refused() {
        let map = map(&[(1, "/mnt/photo")]);
        for bad in [
            "",
            "mnt/photo/a.jpg",
            "./mnt/photo/a.jpg",
            "/mnt/photo/../photo/a.jpg",
            "/mnt/photo/./a.jpg",
            "/mnt/photo/a/..",
            "/mnt/photo/a\0.jpg",
            "/mnt/photo/..",
        ] {
            assert_eq!(map.from_immich(bad), None, "{bad:?}");
        }
        assert_eq!(
            map.from_immich("/mnt/photo/..a/b.jpg"),
            Some((1, rel("..a/b.jpg")))
        );
        assert_eq!(map.from_immich("/mnt/photo/a..b"), Some((1, rel("a..b"))));
    }

    #[test]
    fn names_are_carried_byte_for_byte() {
        let map = map(&[(1, "/mnt/photo")]);
        // The same visible name in both Unicode normal forms: two different paths.
        let nfc = "caf\u{e9}/\u{e9}t\u{e9}.jpg";
        let nfd = "cafe\u{301}/e\u{301}te\u{301}.jpg";
        assert_ne!(nfc, nfd);
        for name in [nfc, nfd, "日本語/写真 1.jpg", "a b/ c .jpg", "Ünï/CASE.JPG"] {
            let immich = map.to_immich(1, &rel(name)).unwrap();
            assert_eq!(immich, format!("/mnt/photo/{name}"));
            let (volume, back) = map.from_immich(&immich).unwrap();
            assert_eq!((volume, back.as_str()), (1, name));
        }
        // Immich paths are case sensitive too.
        assert_eq!(map.from_immich("/MNT/photo/a.jpg"), None);
    }

    #[test]
    fn the_library_and_the_prefix_are_looked_up_by_volume() {
        let map =
            PathMap::new(&[entry(1, Some("lib-a"), "/mnt/a"), entry(2, None, "/mnt/b")]).unwrap();
        assert_eq!(map.library_for(1), Some("lib-a"));
        assert_eq!(map.library_for(2), None, "limited mode has no library");
        assert_eq!(map.library_for(3), None);
        assert_eq!(map.prefix_for(2), Some("/mnt/b"));
        assert_eq!(map.prefix_for(3), None);
        assert_eq!(map.len(), 2);
        assert!(!map.is_empty());
        assert!(PathMap::default().is_empty());
        assert_eq!(PathMap::default().from_immich("/x"), None);
        assert_eq!(PathMap::default().to_immich(1, &rel("x")), None);
    }

    #[test]
    fn the_order_of_the_mappings_does_not_matter() {
        let forward = PathMap::new(&[entry(1, None, "/a"), entry(2, None, "/b/c")]).unwrap();
        let backward = PathMap::new(&[entry(2, None, "/b/c"), entry(1, None, "/a")]).unwrap();
        for map in [forward, backward] {
            assert_eq!(map.from_immich("/a/x"), Some((1, rel("x"))));
            assert_eq!(map.from_immich("/b/c/x"), Some((2, rel("x"))));
            assert_eq!(map.from_immich("/b/x"), None);
        }
    }

    #[test]
    fn a_valid_set_is_accepted_and_an_empty_one_clears_the_mappings() {
        assert!(validate(&[], true).is_empty());
        assert!(
            validate(
                &[
                    entry(1, Some("lib-1"), "/mnt/photo"),
                    entry(2, Some("lib-2"), "/mnt/video"),
                ],
                true
            )
            .is_empty()
        );
        assert!(validate(&[entry(1, Some("lib"), "/")], true).is_empty());
        assert!(validate(&[entry(1, None, "/mnt/photo")], false).is_empty());
    }

    #[test]
    fn prefixes_must_be_absolute_and_normalised() {
        let cases = [
            ("", "is required"),
            ("mnt/photo", "absolute"),
            ("./photo", "absolute"),
            (" /mnt/photo", "absolute"),
            ("/mnt/photo/", "must not end with"),
            ("/mnt//photo", "`//`"),
            ("//mnt", "`//`"),
            ("/mnt/./photo", "`.` or `..`"),
            ("/mnt/photo/..", "`.` or `..`"),
            ("/../mnt", "`.` or `..`"),
            ("/mnt/pho\0to", "control characters"),
            ("/mnt/pho\nto", "control characters"),
        ];
        for (prefix, expected) in cases {
            let problems = validate(&[entry(1, Some("lib"), prefix)], true);
            assert_eq!(fields(&problems), ["mappings[0].path_prefix"], "{prefix:?}");
            assert!(
                problems[0].message.contains(expected),
                "{prefix:?}: {}",
                problems[0].message
            );
        }
        let long = format!("/{}", "a".repeat(MAX_PREFIX_BYTES));
        assert_eq!(
            fields(&validate(&[entry(1, Some("lib"), &long)], true)),
            ["mappings[0].path_prefix"]
        );
        // Dots inside a name are fine.
        assert!(validate(&[entry(1, Some("lib"), "/mnt/.photo/a..b/c.")], true).is_empty());
    }

    #[test]
    fn equal_prefixes_are_refused_on_the_later_mapping() {
        let problems = validate(
            &[
                entry(1, Some("a"), "/mnt/photo"),
                entry(2, Some("b"), "/mnt/photo"),
            ],
            true,
        );
        assert_eq!(fields(&problems), ["mappings[1].path_prefix"]);
        assert!(problems[0].message.contains("mappings[0]"));
    }

    #[test]
    fn nested_prefixes_are_refused_on_the_inner_mapping_in_any_order() {
        for entries in [
            [
                entry(1, Some("a"), "/mnt/photo"),
                entry(2, Some("b"), "/mnt/photo/2019"),
            ],
            [
                entry(2, Some("b"), "/mnt/photo/2019"),
                entry(1, Some("a"), "/mnt/photo"),
            ],
        ] {
            let problems = validate(&entries, true);
            assert_eq!(problems.len(), 1, "{problems:?}");
            let inner = entries
                .iter()
                .position(|e| e.path_prefix == "/mnt/photo/2019")
                .unwrap();
            assert_eq!(problems[0].field, format!("mappings[{inner}].path_prefix"));
            assert!(problems[0].message.contains("is inside mappings["));
        }
        // A shared start without a boundary is not nesting.
        assert!(
            validate(
                &[
                    entry(1, Some("a"), "/mnt/photo"),
                    entry(2, Some("b"), "/mnt/photos"),
                    entry(3, Some("c"), "/mnt/photo-old"),
                ],
                true
            )
            .is_empty()
        );
        // Deep nesting is found through the intermediate levels too.
        let problems = validate(
            &[entry(1, Some("a"), "/a"), entry(2, Some("b"), "/a/b/c/d")],
            true,
        );
        assert_eq!(fields(&problems), ["mappings[1].path_prefix"]);
    }

    #[test]
    fn a_lone_slash_is_only_allowed_alone() {
        let problems = validate(
            &[entry(1, Some("a"), "/"), entry(2, Some("b"), "/mnt/photo")],
            true,
        );
        assert_eq!(fields(&problems), ["mappings[0].path_prefix"]);
        assert!(problems[0].message.contains("only mapping"));
    }

    #[test]
    fn a_volume_can_be_mapped_once() {
        let problems = validate(
            &[
                entry(1, Some("a"), "/mnt/a"),
                entry(1, Some("a"), "/mnt/b"),
                entry(1, Some("a"), "/mnt/c"),
            ],
            true,
        );
        assert_eq!(
            fields(&problems),
            ["mappings[1].volume_id", "mappings[2].volume_id"]
        );
        assert!(problems.iter().all(|p| p.message.contains("mappings[0]")));
    }

    #[test]
    fn libraries_are_required_in_full_mode_and_must_look_like_ids() {
        let problems = validate(&[entry(1, None, "/mnt/a")], true);
        assert_eq!(fields(&problems), ["mappings[0].library_id"]);
        assert!(validate(&[entry(1, None, "/mnt/a")], false).is_empty());
        for bad in ["", "a/b", "a b", "../x", "x?y", &"a".repeat(65)] {
            let problems = validate(&[entry(1, Some(bad), "/mnt/a")], false);
            assert_eq!(fields(&problems), ["mappings[0].library_id"], "{bad:?}");
        }
        assert!(
            validate(
                &[entry(
                    1,
                    Some("0b1f3c1e-3f4a-4c55-9a52-1a2b3c4d5e6f"),
                    "/mnt/a"
                )],
                true
            )
            .is_empty()
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let problems = validate(
            &[
                entry(1, None, "relative"),
                entry(1, Some("a/b"), "/mnt/x"),
                entry(3, Some("ok"), "/mnt/x/y"),
            ],
            true,
        );
        assert_eq!(
            fields(&problems),
            [
                "mappings[0].path_prefix",
                "mappings[0].library_id",
                "mappings[1].library_id",
                "mappings[1].volume_id",
                "mappings[2].path_prefix",
            ]
        );
    }

    #[test]
    fn a_huge_request_is_refused_without_the_pairwise_work() {
        let entries: Vec<_> = (0..=MAX_MAPPINGS as i64)
            .map(|i| entry(i, Some("lib"), &format!("/mnt/{i}")))
            .collect();
        assert_eq!(fields(&validate(&entries, true)), ["mappings"]);
    }

    #[test]
    fn a_set_the_validation_refuses_builds_no_map() {
        let problems = PathMap::new(&[entry(1, None, "/a"), entry(2, None, "/a/b")]).unwrap_err();
        assert_eq!(fields(&problems), ["mappings[1].path_prefix"]);
    }

    fn segment() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-zA-Z0-9 _.-]{1,8}",
            "[^/\0]{1,6}",
            Just("é".to_owned()),
            Just("e\u{301}".to_owned()),
        ]
        .prop_filter("not a dot component", |s| s != "." && s != "..")
    }

    fn relative_path() -> impl Strategy<Value = RelPath> {
        proptest::collection::vec(segment(), 0..6)
            .prop_map(|segments| RelPath::new(&segments.join("/")).unwrap())
    }

    proptest! {
        #[test]
        fn a_file_makes_the_round_trip_through_immich(
            path in relative_path(),
            use_root_prefix in any::<bool>(),
            volume in 1_i64..4,
        ) {
            let map = if use_root_prefix {
                PathMap::new(&[entry(volume, None, "/")]).unwrap()
            } else {
                PathMap::new(&[
                    entry(1, None, "/mnt/one"),
                    entry(2, None, "/mnt/two"),
                    entry(3, None, "/srv/three/photos"),
                ])
                .unwrap()
            };
            let immich = map.to_immich(volume, &path);
            if use_root_prefix || (1..=3).contains(&volume) {
                let immich = immich.unwrap();
                prop_assert!(immich.starts_with('/'));
                prop_assert_eq!(map.from_immich(&immich), Some((volume, path.clone())));
                if !path.is_root() {
                    // One trailing slash makes no difference on input.
                    prop_assert_eq!(
                        map.from_immich(&format!("{immich}/")),
                        Some((volume, path))
                    );
                }
            }
        }

        #[test]
        fn what_the_map_accepts_never_escapes_the_volume(raw in "[/a-c.\0]{0,12}") {
            let map = PathMap::new(&[entry(1, None, "/a/b"), entry(2, None, "/c")]).unwrap();
            if let Some((volume, back)) = map.from_immich(&raw) {
                prop_assert!(back.components().all(|part| part != ".." && part != "."));
                prop_assert!(!back.as_str().contains('\0'));
                // The answer maps straight back to the same Immich path (without a trailing slash).
                let again = map.to_immich(volume, &back).unwrap();
                prop_assert_eq!(again.trim_end_matches('/'), raw.trim_end_matches('/'));
            }
        }
    }
}
