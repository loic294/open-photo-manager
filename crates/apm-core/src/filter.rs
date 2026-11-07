//! Filter and scope logic: validation, canonical form and the reference matching
//! predicate.
//!
//! The wire types [`Filter`] and [`Scope`] live in `dto/common.rs`; this module adds
//! the pure logic around them as inherent impls.
//!
//! # Filter semantics
//!
//! [`Filter::matches`] is the executable reference the SQL query builder is tested
//! against, so these rules are exact:
//!
//! - **AND across fields, OR within a field.** An item passes when it satisfies
//!   every non-empty field, and a field is satisfied when the item matches *any*
//!   of its entries. An empty field is no constraint, so the default filter
//!   matches everything.
//! - **Cameras and tags** compare with [`normalize_name`]: Unicode NFC and
//!   case-insensitive, so `Canon EOS R5` and `canon eos r5` (or the decomposed
//!   spelling of an accented name) are the same value. Entries are trimmed, inner
//!   spaces are significant and blank entries are ignored (a list of only blanks
//!   is an empty list). An item without a camera never matches a non-empty camera
//!   list. An item matches a tag list when it carries at least one of the tags.
//!   The item's own values are compared as stored (the extractor trims them).
//! - **Ratings** run from 0 (unrated) to 5 and an item matches when its rating is
//!   listed. Values above 5 are invalid ([`Filter::normalise`] rejects them) and
//!   never match.
//! - **Flags** are `Selected` or `Rejected`. An unflagged item matches no
//!   non-empty flag list: v1 cannot ask for "unflagged only".
//! - **`selection_id`** restricts the result to the members of a server-side
//!   selection, which needs the database. [`Filter::matches`] ignores it.
//!
//! # Canonical form
//!
//! [`Filter::normalise`] validates a filter received from a client and returns the
//! canonical copy that queries and caches use: entries trimmed and NFC-normalised,
//! blanks dropped, duplicates removed (case-insensitively, keeping the smallest
//! spelling), lists sorted. Limits are rejected instead of truncated: at most
//! [`MAX_LIST_ENTRIES`] entries per list (counted before blanks and duplicates are
//! dropped, which bounds the work) and [`MAX_ENTRY_CHARS`] characters per entry.
//! [`Filter::cache_key`] is a stable string that is equal for filters that select
//! the same files, whatever the order, duplicates, case, Unicode form, spacing and
//! blanks of their lists. The typical server flow is: deserialise, `normalise()`
//! (reject on error), then query with the result.
//!
//! # Scope
//!
//! `Scope.dir` is a volume-relative `/`-separated directory (`""` is the volume
//! root). [`Scope::normalise`] validates and canonicalises it, rejecting absolute
//! paths, `..` components and NUL bytes. [`Scope::contains`] says whether a file's
//! directory is inside the scope: a scope with a `volume_id` only covers that
//! volume (`None` covers every volume, with the same directory in each), and
//! `recursive` includes the subdirectories, otherwise the directory must be equal.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use unicode_normalization::UnicodeNormalization;

use crate::dto::{Filter, FlagFilter, Scope};
use crate::names::normalize_name;
use crate::paths::{PathError, RelPath, VolumeId};

/// The highest star rating (0 means unrated).
pub const MAX_RATING: u8 = 5;

/// Most entries accepted in one filter list.
pub const MAX_LIST_ENTRIES: usize = 200;

/// Longest accepted camera or tag name, in characters, after trimming.
pub const MAX_ENTRY_CHARS: usize = 200;

/// Why a filter was rejected by [`Filter::normalise`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    /// A rating other than 0 (unrated) to 5.
    #[error("rating {0} is out of range (0 means unrated, 1 to 5 are stars)")]
    RatingOutOfRange(u8),
    /// A list with more than [`MAX_LIST_ENTRIES`] entries.
    #[error("too many {field} (at most {max})")]
    TooManyEntries { field: &'static str, max: usize },
    /// An entry longer than [`MAX_ENTRY_CHARS`] characters.
    #[error("an entry in {field} is longer than {max} characters")]
    EntryTooLong { field: &'static str, max: usize },
}

/// What a filter looks at on one file: a plain value for [`Filter::matches`].
///
/// The default is an item with no camera, no tags, no rating and no flag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFacts {
    /// Camera name ("Make Model"); `None` when the file has none.
    pub camera: Option<String>,
    /// Star rating, 0 when unrated.
    pub rating: u8,
    /// The flag; `None` when the file is unflagged.
    pub flag: Option<FlagFilter>,
    /// Tags carried by the file.
    pub tags: Vec<String>,
}

impl Filter {
    /// Validates the filter and returns its canonical form (see the module docs).
    ///
    /// Idempotent: normalising the result changes nothing.
    pub fn normalise(&self) -> Result<Filter, FilterError> {
        Ok(Filter {
            cameras: normalise_texts("cameras", &self.cameras)?,
            ratings: normalise_ratings(&self.ratings)?,
            flags: normalise_flags(&self.flags)?,
            tags: normalise_texts("tags", &self.tags)?,
            selection_id: self.selection_id,
        })
    }

    /// Whether the filter constrains nothing: every list is empty (or holds only
    /// blank entries) and no selection is set.
    pub fn is_empty(&self) -> bool {
        self.selection_id.is_none()
            && self.ratings.is_empty()
            && self.flags.is_empty()
            && self.cameras.iter().all(|camera| camera.trim().is_empty())
            && self.tags.iter().all(|tag| tag.trim().is_empty())
    }

    /// Whether `facts` passes the filter, with the semantics in the module docs.
    /// Works on any filter, normalised or not, and ignores `selection_id`.
    pub fn matches(&self, facts: &FileFacts) -> bool {
        let cameras = text_keys(&self.cameras);
        let camera_ok = cameras.is_empty()
            || facts
                .camera
                .as_deref()
                .is_some_and(|camera| cameras.contains(&normalize_name(camera)));
        let rating_ok = self.ratings.is_empty()
            || (facts.rating <= MAX_RATING && self.ratings.contains(&facts.rating));
        let flag_ok =
            self.flags.is_empty() || facts.flag.is_some_and(|flag| self.flags.contains(&flag));
        let tags = text_keys(&self.tags);
        let tags_ok = tags.is_empty()
            || facts
                .tags
                .iter()
                .any(|tag| tags.contains(&normalize_name(tag)));
        camera_ok && rating_ok && flag_ok && tags_ok
    }

    /// A stable string identifying what the filter selects, for cache keys and
    /// ETags. Equal for filters that match the same way (order, duplicates, case,
    /// Unicode form, spacing and blank entries do not matter) and different
    /// otherwise. Infallible: it does not validate. The `v1` prefix is the format
    /// version.
    pub fn cache_key(&self) -> String {
        let ratings: BTreeSet<u8> = self.ratings.iter().copied().collect();
        let ratings: Vec<String> = ratings.iter().map(u8::to_string).collect();
        let flags: Vec<&str> = canonical_flags(&self.flags)
            .into_iter()
            .map(flag_code)
            .collect();
        let selection = self.selection_id.map(|id| id.to_string());
        format!(
            "v1;c={};r={};f={};t={};s={}",
            length_prefixed(&text_keys(&self.cameras)),
            ratings.join(","),
            flags.join(","),
            length_prefixed(&text_keys(&self.tags)),
            selection.unwrap_or_default(),
        )
    }
}

impl Scope {
    /// The directory as a validated path.
    pub fn dir_rel(&self) -> Result<RelPath, PathError> {
        RelPath::new(&self.dir)
    }

    /// The scope with `dir` in canonical form (no leading, trailing or repeated
    /// slashes, no `.` components); an error for absolute paths, `..` and NUL.
    pub fn normalise(&self) -> Result<Scope, PathError> {
        Ok(Scope {
            dir: self.dir_rel()?.into(),
            ..self.clone()
        })
    }

    /// Whether a file in directory `dir` of volume `volume_id` is inside the
    /// scope. A scope whose own directory is invalid contains nothing.
    pub fn contains(&self, volume_id: VolumeId, dir: &RelPath) -> bool {
        if self.volume_id.is_some_and(|scoped| scoped != volume_id) {
            return false;
        }
        let Ok(scope_dir) = self.dir_rel() else {
            return false;
        };
        if self.recursive {
            dir.starts_with(&scope_dir)
        } else {
            *dir == scope_dir
        }
    }
}

/// Trimmed, NFC-normalised, de-duplicated and sorted names (see the module docs).
fn normalise_texts(field: &'static str, list: &[String]) -> Result<Vec<String>, FilterError> {
    check_list_length(field, list.len())?;
    let mut by_key: BTreeMap<String, String> = BTreeMap::new();
    for raw in list {
        let trimmed = raw.trim();
        if trimmed.chars().nth(MAX_ENTRY_CHARS).is_some() {
            return Err(FilterError::EntryTooLong {
                field,
                max: MAX_ENTRY_CHARS,
            });
        }
        if trimmed.is_empty() {
            continue;
        }
        let spelling: String = trimmed.nfc().collect();
        match by_key.entry(normalize_name(&spelling)) {
            Entry::Vacant(slot) => {
                slot.insert(spelling);
            }
            Entry::Occupied(mut slot) => {
                if spelling < *slot.get() {
                    *slot.get_mut() = spelling;
                }
            }
        }
    }
    Ok(by_key.into_values().collect())
}

fn normalise_ratings(list: &[u8]) -> Result<Vec<u8>, FilterError> {
    check_list_length("ratings", list.len())?;
    if let Some(&bad) = list.iter().find(|&&rating| rating > MAX_RATING) {
        return Err(FilterError::RatingOutOfRange(bad));
    }
    let distinct: BTreeSet<u8> = list.iter().copied().collect();
    Ok(distinct.into_iter().collect())
}

fn normalise_flags(list: &[FlagFilter]) -> Result<Vec<FlagFilter>, FilterError> {
    check_list_length("flags", list.len())?;
    Ok(canonical_flags(list))
}

fn check_list_length(field: &'static str, len: usize) -> Result<(), FilterError> {
    if len > MAX_LIST_ENTRIES {
        return Err(FilterError::TooManyEntries {
            field,
            max: MAX_LIST_ENTRIES,
        });
    }
    Ok(())
}

/// The distinct comparison keys of the non-blank entries.
fn text_keys(list: &[String]) -> BTreeSet<String> {
    list.iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .map(normalize_name)
        .collect()
}

/// Distinct flags in the canonical order (`Selected` before `Rejected`).
fn canonical_flags(list: &[FlagFilter]) -> Vec<FlagFilter> {
    let mut flags = list.to_vec();
    flags.sort_by_key(|flag| flag_rank(*flag));
    flags.dedup();
    flags
}

// The exhaustive matches make adding a flag a compile error here.
fn flag_rank(flag: FlagFilter) -> u8 {
    match flag {
        FlagFilter::Selected => 0,
        FlagFilter::Rejected => 1,
    }
}

fn flag_code(flag: FlagFilter) -> &'static str {
    match flag {
        FlagFilter::Selected => "selected",
        FlagFilter::Rejected => "rejected",
    }
}

/// `len:text` pairs joined by commas; the byte length makes the encoding
/// unambiguous whatever characters the texts contain.
fn length_prefixed(texts: &BTreeSet<String>) -> String {
    let pairs: Vec<String> = texts
        .iter()
        .map(|text| format!("{}:{text}", text.len()))
        .collect();
    pairs.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn facts(
        camera: Option<&str>,
        rating: u8,
        flag: Option<FlagFilter>,
        tags: &[&str],
    ) -> FileFacts {
        FileFacts {
            camera: camera.map(str::to_owned),
            rating,
            flag,
            tags: strings(tags),
        }
    }

    #[test]
    fn the_default_filter_is_empty_and_matches_everything() {
        let filter = Filter::default();
        assert!(filter.is_empty());
        assert!(filter.matches(&FileFacts::default()));
        assert!(filter.matches(&facts(Some("Canon"), 5, Some(FlagFilter::Rejected), &["a"])));
        assert_eq!(filter.normalise().unwrap(), filter);
    }

    #[test]
    fn any_constraint_makes_a_filter_non_empty() {
        let constraints = [
            Filter {
                cameras: strings(&["Canon"]),
                ..Filter::default()
            },
            Filter {
                ratings: vec![0],
                ..Filter::default()
            },
            Filter {
                flags: vec![FlagFilter::Selected],
                ..Filter::default()
            },
            Filter {
                tags: strings(&["x"]),
                ..Filter::default()
            },
            Filter {
                selection_id: Some(1),
                ..Filter::default()
            },
        ];
        for filter in constraints {
            assert!(!filter.is_empty(), "{filter:?}");
        }
    }

    #[test]
    fn blank_entries_are_no_constraint() {
        let filter = Filter {
            cameras: strings(&["", "  "]),
            tags: strings(&["\t", ""]),
            ..Filter::default()
        };
        assert!(filter.is_empty());
        assert!(filter.matches(&FileFacts::default()));
        assert_eq!(filter.normalise().unwrap(), Filter::default());
    }

    #[test]
    fn normalise_trims_dedupes_and_sorts() {
        let filter = Filter {
            cameras: strings(&[" Sony A7 ", "canon eos r5", "Canon EOS R5", "", "SONY a7"]),
            ratings: vec![5, 0, 3, 5, 0],
            flags: vec![
                FlagFilter::Rejected,
                FlagFilter::Selected,
                FlagFilter::Rejected,
            ],
            tags: strings(&["Zebra", "apple", "ZEBRA", " Apple"]),
            selection_id: Some(9),
        };
        let normal = filter.normalise().unwrap();
        // The smallest spelling of each name survives, ordered by comparison key.
        assert_eq!(normal.cameras, strings(&["Canon EOS R5", "SONY a7"]));
        assert_eq!(normal.ratings, vec![0, 3, 5]);
        assert_eq!(
            normal.flags,
            vec![FlagFilter::Selected, FlagFilter::Rejected]
        );
        assert_eq!(normal.tags, strings(&["Apple", "ZEBRA"]));
        assert_eq!(normal.selection_id, Some(9));
        assert_eq!(normal.normalise().unwrap(), normal);
    }

    #[test]
    fn normalise_composes_unicode_but_keeps_case() {
        let filter = Filter {
            tags: strings(&["Cafe\u{301}", "caf\u{e9}"]),
            cameras: strings(&["e\u{301}te\u{301}"]),
            ..Filter::default()
        };
        let normal = filter.normalise().unwrap();
        assert_eq!(normal.tags, strings(&["Caf\u{e9}"]));
        assert_eq!(normal.cameras, strings(&["\u{e9}t\u{e9}"]));
    }

    #[test]
    fn normalise_rejects_ratings_above_five() {
        for bad in [6u8, 7, 100, 255] {
            let filter = Filter {
                ratings: vec![1, bad],
                ..Filter::default()
            };
            assert_eq!(filter.normalise(), Err(FilterError::RatingOutOfRange(bad)));
        }
        let top = Filter {
            ratings: vec![5],
            ..Filter::default()
        };
        assert!(top.normalise().is_ok());
    }

    #[test]
    fn normalise_enforces_the_list_and_entry_limits() {
        let names = |count: usize| (0..count).map(|i| format!("n{i}")).collect::<Vec<_>>();
        let at_limit = Filter {
            cameras: names(MAX_LIST_ENTRIES),
            tags: names(MAX_LIST_ENTRIES),
            ratings: vec![1; MAX_LIST_ENTRIES],
            flags: vec![FlagFilter::Selected; MAX_LIST_ENTRIES],
            ..Filter::default()
        };
        let normal = at_limit.normalise().unwrap();
        assert_eq!(normal.cameras.len(), MAX_LIST_ENTRIES);
        assert_eq!(normal.ratings, vec![1]);

        let over = |filter: Filter| filter.normalise().unwrap_err();
        let too_many = |field| FilterError::TooManyEntries {
            field,
            max: MAX_LIST_ENTRIES,
        };
        let cameras = names(MAX_LIST_ENTRIES + 1);
        let filter = Filter {
            cameras,
            ..Filter::default()
        };
        assert_eq!(over(filter), too_many("cameras"));
        let filter = Filter {
            tags: names(MAX_LIST_ENTRIES + 1),
            ..Filter::default()
        };
        assert_eq!(over(filter), too_many("tags"));
        let filter = Filter {
            ratings: vec![1; MAX_LIST_ENTRIES + 1],
            ..Filter::default()
        };
        assert_eq!(over(filter), too_many("ratings"));
        let filter = Filter {
            flags: vec![FlagFilter::Selected; MAX_LIST_ENTRIES + 1],
            ..Filter::default()
        };
        assert_eq!(over(filter), too_many("flags"));

        // Blanks count towards the cap: it bounds the work done on a request.
        let filter = Filter {
            tags: vec![String::new(); MAX_LIST_ENTRIES + 1],
            ..Filter::default()
        };
        assert_eq!(over(filter), too_many("tags"));
    }

    #[test]
    fn entries_may_have_at_most_the_maximum_number_of_characters() {
        let exact = "\u{e9}".repeat(MAX_ENTRY_CHARS);
        let long = "\u{e9}".repeat(MAX_ENTRY_CHARS + 1);
        let ok = Filter {
            cameras: vec![format!("  {exact}  ")],
            ..Filter::default()
        };
        assert_eq!(ok.normalise().unwrap().cameras, vec![exact]);

        let too_long = |field| FilterError::EntryTooLong {
            field,
            max: MAX_ENTRY_CHARS,
        };
        let bad = Filter {
            cameras: vec![long.clone()],
            ..Filter::default()
        };
        assert_eq!(bad.normalise(), Err(too_long("cameras")));
        let bad = Filter {
            tags: strings(&["fine"]).into_iter().chain([long]).collect(),
            ..Filter::default()
        };
        assert_eq!(bad.normalise(), Err(too_long("tags")));
    }

    #[test]
    fn error_messages_are_readable() {
        assert_eq!(
            FilterError::RatingOutOfRange(9).to_string(),
            "rating 9 is out of range (0 means unrated, 1 to 5 are stars)"
        );
        assert_eq!(
            FilterError::TooManyEntries {
                field: "tags",
                max: 200
            }
            .to_string(),
            "too many tags (at most 200)"
        );
        assert_eq!(
            FilterError::EntryTooLong {
                field: "cameras",
                max: 200
            }
            .to_string(),
            "an entry in cameras is longer than 200 characters"
        );
    }

    #[test]
    fn cameras_match_by_normalised_name() {
        let filter = Filter {
            cameras: strings(&["Canon EOS R5", "  sony a7  "]),
            ..Filter::default()
        };
        assert!(filter.matches(&facts(Some("canon eos r5"), 0, None, &[])));
        assert!(filter.matches(&facts(Some("SONY A7"), 0, None, &[])));
        assert!(!filter.matches(&facts(Some("Canon EOS R6"), 0, None, &[])));
        assert!(!filter.matches(&facts(Some("Canon EOS R5 "), 0, None, &[])));
        assert!(!filter.matches(&facts(Some(""), 0, None, &[])));
        // An item without a camera never matches a camera constraint.
        assert!(!filter.matches(&facts(None, 0, None, &[])));
    }

    #[test]
    fn unicode_forms_are_the_same_name() {
        let filter = Filter {
            cameras: strings(&["\u{c9}t\u{e9}"]),
            tags: strings(&["Cafe\u{301}"]),
            ..Filter::default()
        };
        assert!(filter.matches(&facts(Some("e\u{301}te\u{301}"), 0, None, &["CAF\u{c9}"])));
    }

    #[test]
    fn ratings_match_exactly_and_zero_means_unrated() {
        let filter = Filter {
            ratings: vec![0, 4],
            ..Filter::default()
        };
        for rating in 0..=5 {
            let expected = rating == 0 || rating == 4;
            assert_eq!(filter.matches(&facts(None, rating, None, &[])), expected);
        }
        // Out of range values never match, even when the filter lists them.
        let invalid = Filter {
            ratings: vec![0, 9],
            ..Filter::default()
        };
        assert!(!invalid.matches(&facts(None, 9, None, &[])));
        assert!(invalid.matches(&facts(None, 0, None, &[])));
    }

    #[test]
    fn flags_match_and_unflagged_items_match_no_flag_constraint() {
        let selected = Filter {
            flags: vec![FlagFilter::Selected],
            ..Filter::default()
        };
        assert!(selected.matches(&facts(None, 0, Some(FlagFilter::Selected), &[])));
        assert!(!selected.matches(&facts(None, 0, Some(FlagFilter::Rejected), &[])));
        assert!(!selected.matches(&facts(None, 0, None, &[])));
        let both = Filter {
            flags: vec![FlagFilter::Rejected, FlagFilter::Selected],
            ..Filter::default()
        };
        assert!(both.matches(&facts(None, 0, Some(FlagFilter::Selected), &[])));
        assert!(both.matches(&facts(None, 0, Some(FlagFilter::Rejected), &[])));
        assert!(!both.matches(&facts(None, 0, None, &[])));
    }

    #[test]
    fn tags_match_when_any_of_them_is_present() {
        let filter = Filter {
            tags: strings(&["Family", "travel"]),
            ..Filter::default()
        };
        assert!(filter.matches(&facts(None, 0, None, &["x", "TRAVEL"])));
        assert!(filter.matches(&facts(None, 0, None, &["family", "travel"])));
        assert!(!filter.matches(&facts(None, 0, None, &["fam"])));
        assert!(!filter.matches(&facts(None, 0, None, &[])));
    }

    #[test]
    fn fields_combine_with_and() {
        let filter = Filter {
            cameras: strings(&["Canon"]),
            ratings: vec![3, 4],
            flags: vec![FlagFilter::Selected],
            tags: strings(&["trip"]),
            selection_id: None,
        };
        let good = facts(Some("canon"), 4, Some(FlagFilter::Selected), &["trip"]);
        assert!(filter.matches(&good));
        let wrong_camera = FileFacts {
            camera: Some("Nikon".to_owned()),
            ..good.clone()
        };
        let wrong_rating = FileFacts {
            rating: 5,
            ..good.clone()
        };
        let wrong_flag = FileFacts {
            flag: Some(FlagFilter::Rejected),
            ..good.clone()
        };
        let wrong_tags = FileFacts {
            tags: strings(&["work"]),
            ..good
        };
        for bad in [wrong_camera, wrong_rating, wrong_flag, wrong_tags] {
            assert!(!filter.matches(&bad), "{bad:?}");
        }
    }

    #[test]
    fn the_selection_is_ignored_by_the_pure_predicate() {
        let filter = Filter {
            selection_id: Some(12),
            ..Filter::default()
        };
        assert!(filter.matches(&FileFacts::default()));
        assert!(!filter.is_empty());
    }

    #[test]
    fn the_cache_key_has_a_pinned_format() {
        assert_eq!(Filter::default().cache_key(), "v1;c=;r=;f=;t=;s=");
        let filter = Filter {
            cameras: strings(&["Canon EOS R5"]),
            ratings: vec![4, 1, 4],
            flags: vec![FlagFilter::Rejected, FlagFilter::Selected],
            tags: strings(&["Trip", " trip ", "ab"]),
            selection_id: Some(7),
        };
        assert_eq!(
            filter.cache_key(),
            "v1;c=12:canon eos r5;r=1,4;f=selected,rejected;t=2:ab,4:trip;s=7"
        );
    }

    #[test]
    fn the_cache_key_ignores_order_duplicates_case_unicode_form_and_blanks() {
        let a = Filter {
            cameras: strings(&["Canon", "Sony"]),
            ratings: vec![1, 2],
            flags: vec![FlagFilter::Selected, FlagFilter::Rejected],
            tags: strings(&["\u{c9}t\u{e9}", "b"]),
            selection_id: Some(3),
        };
        let b = Filter {
            cameras: strings(&["", " SONY", "canon ", "Sony"]),
            ratings: vec![2, 1, 2],
            flags: vec![
                FlagFilter::Rejected,
                FlagFilter::Selected,
                FlagFilter::Rejected,
            ],
            tags: strings(&["B", "e\u{301}te\u{301}", "  "]),
            selection_id: Some(3),
        };
        assert_eq!(a.cache_key(), b.cache_key());
        assert_eq!(a.normalise().unwrap().cache_key(), a.cache_key());
    }

    #[test]
    fn the_cache_key_tells_different_filters_apart() {
        let key = |filter: Filter| filter.cache_key();
        let base = Filter {
            cameras: strings(&["a"]),
            ..Filter::default()
        };
        let variants = [
            Filter::default(),
            base.clone(),
            Filter {
                cameras: strings(&["b"]),
                ..Filter::default()
            },
            Filter {
                tags: strings(&["a"]),
                ..Filter::default()
            },
            Filter {
                cameras: strings(&["a", "b"]),
                ..Filter::default()
            },
            Filter {
                ratings: vec![0],
                ..Filter::default()
            },
            Filter {
                flags: vec![FlagFilter::Selected],
                ..Filter::default()
            },
            Filter {
                flags: vec![FlagFilter::Rejected],
                ..Filter::default()
            },
            Filter {
                selection_id: Some(0),
                ..Filter::default()
            },
            // Separators inside a value cannot forge another filter.
            Filter {
                cameras: strings(&["a,1:b"]),
                ..Filter::default()
            },
            Filter {
                cameras: strings(&["a;t=1:b"]),
                ..Filter::default()
            },
        ];
        let keys: BTreeSet<String> = variants.iter().cloned().map(key).collect();
        assert_eq!(keys.len(), variants.len());
    }

    #[test]
    fn scope_normalise_canonicalises_the_directory() {
        let scope = Scope {
            volume_id: Some(3),
            dir: "a//b/./c/".to_owned(),
            recursive: true,
        };
        let normal = scope.normalise().unwrap();
        assert_eq!(normal.dir, "a/b/c");
        assert_eq!((normal.volume_id, normal.recursive), (Some(3), true));
        assert_eq!(Scope::default().normalise().unwrap(), Scope::default());
        assert_eq!(scope.dir_rel().unwrap().as_str(), "a/b/c");
    }

    #[test]
    fn scope_normalise_rejects_absolute_parent_and_nul_directories() {
        let bad = |dir: &str| Scope {
            dir: dir.to_owned(),
            ..Scope::default()
        };
        assert_eq!(bad("/etc").normalise(), Err(PathError::Absolute));
        assert_eq!(bad("../x").normalise(), Err(PathError::ParentTraversal));
        assert_eq!(bad("a/../b").normalise(), Err(PathError::ParentTraversal));
        assert_eq!(bad("a\0b").normalise(), Err(PathError::Nul));
        assert!(bad("..x").normalise().is_ok());
    }

    #[test]
    fn scope_contains_follows_volume_directory_and_recursion() {
        let rel = |path: &str| RelPath::new(path).unwrap();
        let scope = |volume_id, dir: &str, recursive| Scope {
            volume_id,
            dir: dir.to_owned(),
            recursive,
        };

        let flat = scope(Some(1), "2024/Trip", false);
        assert!(flat.contains(1, &rel("2024/Trip")));
        assert!(!flat.contains(1, &rel("2024/Trip/Day1")));
        assert!(!flat.contains(1, &rel("2024")));
        assert!(!flat.contains(2, &rel("2024/Trip")));

        let deep = scope(Some(1), "2024/Trip", true);
        assert!(deep.contains(1, &rel("2024/Trip")));
        assert!(deep.contains(1, &rel("2024/Trip/Day1/raw")));
        assert!(!deep.contains(1, &rel("2024/Trips")));
        assert!(!deep.contains(1, &rel("2024")));
        assert!(!deep.contains(2, &rel("2024/Trip")));

        let every_volume = scope(None, "a", true);
        assert!(every_volume.contains(1, &rel("a/b")));
        assert!(every_volume.contains(99, &rel("a")));
        assert!(!every_volume.contains(1, &rel("b")));

        let root_flat = scope(Some(1), "", false);
        assert!(root_flat.contains(1, &RelPath::root()));
        assert!(!root_flat.contains(1, &rel("a")));
        let root_deep = scope(Some(1), "", true);
        assert!(root_deep.contains(1, &rel("a/b/c")));
        assert!(!root_deep.contains(2, &rel("a")));

        assert!(!scope(Some(1), "../x", true).contains(1, &rel("x")));
        assert!(!scope(None, "/abs", true).contains(1, &rel("abs")));
    }

    /// Short alphabet so that collisions (case, spacing, duplicates) are common.
    fn name() -> impl Strategy<Value = String> {
        "[a-cA-C ]{0,4}"
    }

    fn any_flag() -> impl Strategy<Value = FlagFilter> {
        prop_oneof![Just(FlagFilter::Selected), Just(FlagFilter::Rejected)]
    }

    fn any_filter() -> impl Strategy<Value = Filter> {
        (
            proptest::collection::vec(name(), 0..5),
            proptest::collection::vec(0u8..=5, 0..5),
            proptest::collection::vec(any_flag(), 0..4),
            proptest::collection::vec(name(), 0..5),
            proptest::option::of(0i64..4),
        )
            .prop_map(|(cameras, ratings, flags, tags, selection_id)| Filter {
                cameras,
                ratings,
                flags,
                tags,
                selection_id,
            })
    }

    fn any_facts() -> impl Strategy<Value = FileFacts> {
        (
            proptest::option::of(name()),
            0u8..=5,
            proptest::option::of(any_flag()),
            proptest::collection::vec(name(), 0..4),
        )
            .prop_map(|(camera, rating, flag, tags)| FileFacts {
                camera,
                rating,
                flag,
                tags,
            })
    }

    /// The semantics written independently for ASCII names.
    fn oracle(filter: &Filter, facts: &FileFacts) -> bool {
        let listed = |list: &[String]| {
            list.iter()
                .map(|entry| entry.trim().to_owned())
                .filter(|entry| !entry.is_empty())
                .collect::<Vec<_>>()
        };
        let cameras = listed(&filter.cameras);
        let tags = listed(&filter.tags);
        let camera_ok = cameras.is_empty()
            || facts
                .camera
                .as_ref()
                .is_some_and(|camera| cameras.iter().any(|c| c.eq_ignore_ascii_case(camera)));
        let rating_ok = filter.ratings.is_empty() || filter.ratings.contains(&facts.rating);
        let flag_ok =
            filter.flags.is_empty() || facts.flag.is_some_and(|flag| filter.flags.contains(&flag));
        let tags_ok = tags.is_empty()
            || facts
                .tags
                .iter()
                .any(|tag| tags.iter().any(|t| t.eq_ignore_ascii_case(tag)));
        camera_ok && rating_ok && flag_ok && tags_ok
    }

    /// Reorders the lists, repeats and pads entries, and blanks some: the same
    /// filter in a messier form.
    fn messy(filter: &Filter, rotate: usize) -> Filter {
        let scramble = |list: &[String]| {
            let mut out: Vec<String> = list
                .iter()
                .flat_map(|entry| [format!(" {entry} "), entry.clone(), String::new()])
                .collect();
            if !out.is_empty() {
                let by = rotate % out.len();
                out.rotate_left(by);
            }
            out.reverse();
            out
        };
        let mut ratings = filter.ratings.clone();
        ratings.extend(filter.ratings.iter().rev());
        let mut flags = filter.flags.clone();
        flags.reverse();
        flags.extend(filter.flags.iter());
        Filter {
            cameras: scramble(&filter.cameras),
            ratings,
            flags,
            tags: scramble(&filter.tags),
            selection_id: filter.selection_id,
        }
    }

    proptest! {
        #[test]
        fn matches_follows_the_documented_semantics(
            filter in any_filter(),
            facts in any_facts(),
        ) {
            prop_assert_eq!(filter.matches(&facts), oracle(&filter, &facts));
        }

        #[test]
        fn normalising_does_not_change_what_matches(
            filter in any_filter(),
            facts in any_facts(),
        ) {
            let normal = filter.normalise().unwrap();
            prop_assert_eq!(normal.matches(&facts), filter.matches(&facts));
            prop_assert_eq!(normal.is_empty(), filter.is_empty());
        }

        #[test]
        fn normalise_is_idempotent(filter in any_filter()) {
            let once = filter.normalise().unwrap();
            prop_assert_eq!(once.normalise().unwrap(), once);
        }

        #[test]
        fn messy_forms_of_a_filter_normalise_and_key_the_same(
            filter in any_filter(),
            rotate in 0usize..8,
        ) {
            let messy = messy(&filter, rotate);
            prop_assert_eq!(messy.normalise().unwrap().cameras.len() <= 4, true);
            prop_assert_eq!(messy.cache_key(), filter.cache_key());
            prop_assert_eq!(messy.is_empty(), filter.is_empty());
            let (a, b) = (filter.normalise().unwrap(), messy.normalise().unwrap());
            prop_assert_eq!(a.ratings, b.ratings);
            prop_assert_eq!(a.flags, b.flags);
            prop_assert_eq!(a.selection_id, b.selection_id);
            // The spelling kept for a name is the smallest one, whatever the order.
            prop_assert_eq!(a.cameras, b.cameras);
            prop_assert_eq!(a.tags, b.tags);
        }

        #[test]
        fn case_changes_do_not_change_the_cache_key(
            filter in any_filter(),
        ) {
            let upper = Filter {
                cameras: filter.cameras.iter().map(|c| c.to_uppercase()).collect(),
                tags: filter.tags.iter().map(|t| t.to_lowercase()).collect(),
                ..filter.clone()
            };
            prop_assert_eq!(upper.cache_key(), filter.cache_key());
        }

        #[test]
        fn filters_that_match_differently_get_different_keys(
            a in any_filter(),
            b in any_filter(),
            probes in proptest::collection::vec(any_facts(), 40),
        ) {
            if a.cache_key() == b.cache_key() {
                for probe in &probes {
                    prop_assert_eq!(a.matches(probe), b.matches(probe));
                }
                prop_assert_eq!(a.selection_id, b.selection_id);
            }
        }

        #[test]
        fn wider_lists_never_match_less(
            filter in any_filter(),
            extra_camera in name(),
            extra_rating in 0u8..=5,
            extra_tag in name(),
            facts in any_facts(),
        ) {
            let mut wider = filter.clone();
            let constrains = |list: &[String]| list.iter().any(|entry| !entry.trim().is_empty());
            if constrains(&filter.cameras) {
                wider.cameras.push(extra_camera);
            }
            if !filter.ratings.is_empty() {
                wider.ratings.push(extra_rating);
            }
            if constrains(&filter.tags) {
                wider.tags.push(extra_tag);
            }
            if filter.matches(&facts) {
                prop_assert!(wider.matches(&facts));
            }
        }

        #[test]
        fn arbitrary_text_never_panics_and_stays_canonical(
            cameras in proptest::collection::vec("\\PC{0,30}", 0..6),
            tags in proptest::collection::vec("\\PC{0,30}", 0..6),
            probe in "\\PC{0,30}",
        ) {
            let filter = Filter { cameras, tags, ..Filter::default() };
            let normal = filter.normalise().unwrap();
            prop_assert_eq!(normal.normalise().unwrap(), normal.clone());
            prop_assert_eq!(normal.cache_key(), filter.cache_key());
            let probe = facts(Some(&probe), 0, None, &[&probe]);
            prop_assert_eq!(normal.matches(&probe), filter.matches(&probe));
        }
    }
}
