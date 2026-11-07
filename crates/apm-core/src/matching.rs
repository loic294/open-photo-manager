//! Duplicate-matching rules (plan A10 to A12 and section 5.9): the four [`MatchMode`]s, the prefix
//! word-boundary rule and the pair-orientation rule (which side is the source, which the
//! comparison, and what happens when both panes show the same files).
//!
//! This module is the REFERENCE implementation. The plan builders compute the same pairs with SQL
//! joins over the `files` indexes and are tested against [`is_match`] and [`is_valid_pair`]; every
//! rule below is written so that it maps onto one join. The key helpers ([`hash_key`],
//! [`name_key`], [`name_time_key`], [`stem_key`], [`stem_prefix_bounds`], [`stem_prefix_match`])
//! derive exactly the values and bounds those joins compare.
//!
//! # Who takes part
//! - Only media files take part ([`participates`]: image, video, audio), on both sides. Callers
//!   leave sidecars, `other` files and junk out of the candidate sets; [`is_match`] itself never
//!   looks at kinds.
//! - A file never matches itself: a pair needs two different ids. Two rows with identical content
//!   are still two files.
//!
//! # The modes
//! `s` is the source file, `c` a candidate on the comparison side. "Time" is the resolved capture
//! time (`capture_ts`, naive-local epoch SECONDS), so it is compared to the second (A11).
//! - [`MatchMode::Hash`]: both hashes are known and equal (SHA-1 of the content, A12). A file that
//!   is not hashed yet matches nothing. SQL: `c.hash = s.hash`, index `(hash)`.
//! - [`MatchMode::NameAndTime`]: `name_norm` equal AND both times known AND equal; two missing
//!   times do not match. SQL: `c.name_norm = s.name_norm AND c.capture_ts = s.capture_ts`, index
//!   `(name_norm, capture_ts)`.
//! - [`MatchMode::NameDifferentTime`]: `name_norm` equal AND the times differ. A missing time
//!   differs from a present one, two missing times do not differ. SQL: `c.name_norm =
//!   s.name_norm AND c.capture_ts IS NOT s.capture_ts` (`IS NOT` treats NULL as a value), index
//!   `(name_norm)`.
//! - [`MatchMode::NamePrefix`]: DIRECTIONAL, the stem of `c` starts with the stem of `s`, then a
//!   word boundary (below). Extensions play no part. SQL: a range scan on `c.stem_norm` with the
//!   bounds of [`stem_prefix_bounds`] (index `(stem_norm)`), then [`stem_prefix_match`] on the
//!   rows in Rust.
//!
//! Names are compared through [`names::normalize_name`](crate::names::normalize_name) (NFC,
//! lowercase): `IMG_0001.JPG`, `img_0001.jpg` and an NFD spelling are one name. Modes 1 to 3 are
//! symmetric (swapping `s` and `c` never changes the answer), mode 4 is not
//! ([`MatchMode::is_symmetric`]).
//!
//! ## The prefix boundary
//! Let `P` be the normalised stem of `s` ([`stem_key`], `img_0001` for `IMG_0001.CR3`). `c`
//! matches when its normalised stem is `P` itself, or `P` followed by a character that is NOT
//! alphanumeric ([`char::is_alphanumeric`], so `é` and `中` count). `IMG_0001` matches
//! `IMG_0001`, `IMG_0001_edit`, `IMG_0001-1` and `IMG_0001 (1)`, but not `IMG_00012` or
//! `IMG_0001a`. [`MatchOptions::strict_prefix`] drops the boundary rule: every `c` whose stem
//! starts with `P` matches. An empty stem never matches, and only the first character after the
//! prefix is looked at.
//!
//! The boundary cannot be expressed exactly in SQL: `GLOB` and `LIKE` only know ASCII classes, so
//! `NOT substr(...) GLOB '[0-9a-z]'` keeps `é` and `中` where the Rust rule rejects them. Use SQL
//! only as a superset pre-filter and apply [`stem_prefix_match`] to the rows. The bounds are
//! byte-wise UTF-8 comparisons, so `stem_norm` must keep the default BINARY collation.
//!
//! # Orientation: which side is the source
//! Matches are looked up FROM the source side and reported ON the comparison side. A [`Pair`] is
//! `(source file, comparison file)`; the duplicate actions delete one side or move the files of
//! one side. [`PlanSide`] names the sides and [`PlanSide::opposite`] flips one.
//!
//! The scopes of the two sides (what each pane lists) may overlap, most visibly when both panes
//! show the same folder. Unguarded, two duplicates `a` and `b` would give BOTH `(a, b)` and
//! `(b, a)`, and "delete the source side" could remove both copies. [`is_valid_pair`] is the guard
//! (plan 5.9, safety invariants): a candidate pair `(s, c)` is valid when `s != c` and NOT (`s`
//! is also in the comparison scope AND `c` is also in the source scope AND `s.id > c.id`). SQL:
//! `s.id <> c.id AND NOT (s_in_comparison AND c_in_source AND s.id > c.id)`.
//! - Without overlap the guard changes nothing.
//! - Inside the overlap each unordered pair of a symmetric mode keeps exactly one orientation, the
//!   one whose source has the lower id: of a group, "delete comparison" keeps the lowest id and
//!   "delete source" the highest.
//! - For any mode, when the scopes overlap every group of connected candidates keeps at least one
//!   file under either delete action (property-tested).
//! - [`MatchMode::NamePrefix`] is directional, so a prefix pair whose files are both in the
//!   overlap and whose source has the higher id is dropped altogether. That can hide a match,
//!   never invent one.
//! - The guard protects GROUPS, not single files. `NameDifferentTime` is not transitive (`a`
//!   at 1 s, `b` at 2 s, `c` at 1 s: `a`~`b`~`c` but `a`!~`c`), so a file can be deleted while the
//!   only counterpart it matches is deleted too. The plan engine must still check, per deleted
//!   file, that a matched counterpart outside the deletion set survives (plan 5.9: "each item's
//!   survivor is stat'ed and size-checked").
//!
//! ```
//! use apm_core::dto::MatchMode;
//! use apm_core::matching::{is_match, is_valid_pair, MatchFacts, MatchOptions, Overlap, Pair};
//!
//! fn file(id: i64, name: &str) -> MatchFacts<'_> {
//!     MatchFacts { id, name, hash: None, capture_ts: None, size: 0 }
//! }
//!
//! let (source, edited, twelve) =
//!     (file(1, "IMG_0001.CR3"), file(2, "IMG_0001_edit.jpg"), file(3, "IMG_00012.jpg"));
//! let opts = MatchOptions::default();
//! assert!(is_match(MatchMode::NamePrefix, &source, &edited, opts));
//! assert!(!is_match(MatchMode::NamePrefix, &source, &twelve, opts));
//! assert!(!is_match(MatchMode::NamePrefix, &edited, &source, opts));
//!
//! // Both panes show the same folder: only the pair with the lower source id survives.
//! let both = Overlap { source_in_comparison: true, comparison_in_source: true };
//! assert!(is_valid_pair(Pair::new(1, 2), both));
//! assert!(!is_valid_pair(Pair::new(2, 1), both));
//! ```

use crate::dto::{FileKind, MatchMode, PlanSide};
use crate::hash::Sha1Digest;
use crate::names::{normalize_name, stem_norm};
use crate::paths::FileId;

impl MatchMode {
    /// Every mode, in the order the UI lists them.
    pub const ALL: [Self; 4] = [
        Self::Hash,
        Self::NameAndTime,
        Self::NameDifferentTime,
        Self::NamePrefix,
    ];

    /// Whether swapping source and candidate never changes the answer of [`is_match`]. True for
    /// every mode but [`MatchMode::NamePrefix`], which is directional.
    pub const fn is_symmetric(self) -> bool {
        !matches!(self, Self::NamePrefix)
    }
}

/// What the matcher knows about one file: the `files` columns it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchFacts<'a> {
    /// `files.id`.
    pub id: FileId,
    /// File name with its extension (`files.name`). Normalised on the fly, any spelling works.
    pub name: &'a str,
    /// SHA-1 of the content; `None` until the file is hashed.
    pub hash: Option<Sha1Digest>,
    /// Resolved capture time, naive-local epoch seconds (`files.capture_ts`).
    pub capture_ts: Option<i64>,
    /// Size in bytes. Carried for the survivor checks of the plan engine, [`is_match`] does not
    /// read it.
    pub size: i64,
}

/// Options of the matching modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchOptions {
    /// Match on the literal prefix only: [`MatchMode::NamePrefix`] drops its word-boundary rule,
    /// so `IMG_0001` also matches `IMG_00012`. No effect on the other modes.
    pub strict_prefix: bool,
}

/// Whether a file of this kind takes part in matching: images, videos and audio. Sidecars and
/// `other` files never do.
pub const fn participates(kind: FileKind) -> bool {
    kind.is_media()
}

/// Whether `candidate` (comparison side) is a match of `source` in `mode`. See the module docs
/// for the exact rules; a file never matches itself.
pub fn is_match(
    mode: MatchMode,
    source: &MatchFacts<'_>,
    candidate: &MatchFacts<'_>,
    opts: MatchOptions,
) -> bool {
    if source.id == candidate.id {
        return false;
    }
    match mode {
        MatchMode::Hash => hash_key(source).is_some() && hash_key(source) == hash_key(candidate),
        MatchMode::NameAndTime => {
            name_time_key(source).is_some() && name_time_key(source) == name_time_key(candidate)
        }
        MatchMode::NameDifferentTime => {
            name_key(source.name) == name_key(candidate.name)
                && source.capture_ts != candidate.capture_ts
        }
        MatchMode::NamePrefix => stem_prefix_match(
            &stem_key(source.name),
            &stem_key(candidate.name),
            opts.strict_prefix,
        ),
    }
}

/// Key of [`MatchMode::Hash`]: the files sharing a `Some` key match each other; `None` matches
/// nothing.
pub fn hash_key(facts: &MatchFacts<'_>) -> Option<Sha1Digest> {
    facts.hash
}

/// The value of the `name_norm` column for `name`, the key of [`MatchMode::NameDifferentTime`]
/// (the files sharing it match when their times differ).
pub fn name_key(name: &str) -> String {
    normalize_name(name)
}

/// Key of [`MatchMode::NameAndTime`]: `(name_norm, capture_ts)`. The files sharing a `Some` key
/// match each other; `None` (no capture time) matches nothing.
pub fn name_time_key(facts: &MatchFacts<'_>) -> Option<(String, i64)> {
    Some((normalize_name(facts.name), facts.capture_ts?))
}

/// The value of the `stem_norm` column for `name`: the normalised name without its last extension.
pub fn stem_key(name: &str) -> String {
    stem_norm(name)
}

/// Range of `stem_norm` values that start with a given prefix, for a BINARY-collated index scan:
/// `stem_norm >= lower AND stem_norm < upper`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixBounds {
    /// Inclusive lower bound: the prefix itself.
    pub lower: String,
    /// Exclusive upper bound, the smallest string above every string starting with the prefix.
    /// `None` when there is none (a prefix made only of U+10FFFF), then only `lower` applies.
    pub upper: Option<String>,
}

impl PrefixBounds {
    /// Whether `stem_norm` lies inside the range, which is the case exactly when it starts with the
    /// prefix.
    pub fn contains(&self, stem_norm: &str) -> bool {
        stem_norm >= self.lower.as_str() && self.upper.as_deref().is_none_or(|u| stem_norm < u)
    }
}

/// The index range holding every candidate stem of [`MatchMode::NamePrefix`] for a source whose
/// normalised stem is `source_stem_norm`. `None` for an empty stem, which matches nothing. Every
/// stem in the range starts with the prefix, the boundary rule is NOT applied: follow up with
/// [`stem_prefix_match`].
pub fn stem_prefix_bounds(source_stem_norm: &str) -> Option<PrefixBounds> {
    if source_stem_norm.is_empty() {
        return None;
    }
    Some(PrefixBounds {
        lower: source_stem_norm.to_owned(),
        upper: prefix_successor(source_stem_norm),
    })
}

/// The smallest string greater than every string that starts with `prefix`: the prefix with its
/// last character incremented, dropping trailing characters that cannot be.
fn prefix_successor(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        if let Some(next) = next_char(last) {
            chars.push(next);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

/// The next Unicode scalar value, skipping the surrogate gap; `None` after U+10FFFF.
fn next_char(c: char) -> Option<char> {
    let next = u32::from(c).saturating_add(1);
    char::from_u32(if next == 0xD800 { 0xE000 } else { next })
}

/// The rule of [`MatchMode::NamePrefix`] on two NORMALISED stems (the `stem_norm` column, see
/// [`stem_key`]): the candidate stem starts with the source stem and, unless `strict`, is either
/// equal to it or continues with a character that is not alphanumeric. An empty source stem never
/// matches.
pub fn stem_prefix_match(source_stem_norm: &str, candidate_stem_norm: &str, strict: bool) -> bool {
    if source_stem_norm.is_empty() {
        return false;
    }
    match candidate_stem_norm.strip_prefix(source_stem_norm) {
        None => false,
        Some(rest) => strict || rest.chars().next().is_none_or(|c| !c.is_alphanumeric()),
    }
}

impl PlanSide {
    /// The other side: the counterpart of the source is the comparison and vice versa.
    pub const fn opposite(self) -> Self {
        match self {
            Self::Source => Self::Comparison,
            Self::Comparison => Self::Source,
        }
    }
}

/// An oriented match: the file on the source side and its match on the comparison side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pair {
    /// The file on the source side.
    pub source: FileId,
    /// The file on the comparison side.
    pub comparison: FileId,
}

impl Pair {
    /// A pair from its source and comparison file ids.
    pub const fn new(source: FileId, comparison: FileId) -> Self {
        Self { source, comparison }
    }

    /// The file of this pair that sits on `side`.
    pub const fn on_side(self, side: PlanSide) -> FileId {
        match side {
            PlanSide::Source => self.source,
            PlanSide::Comparison => self.comparison,
        }
    }

    /// The match of the file on `side`: the file on the opposite side.
    pub const fn counterpart_of(self, side: PlanSide) -> FileId {
        self.on_side(side.opposite())
    }
}

/// Where the two files of a candidate pair sit relative to the scope of the other side. A scope
/// is what a pane lists: the same folder in both panes, or one folder inside the other, makes the
/// scopes overlap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overlap {
    /// The pair's source file is also in the comparison scope.
    pub source_in_comparison: bool,
    /// The pair's comparison file is also in the source scope.
    pub comparison_in_source: bool,
}

/// The orientation rule: whether the candidate `pair` (source in the source scope, comparison file
/// in the comparison scope, matching) is kept. It is when its two files differ and it is NOT the
/// case that both files are in both scopes and `pair.source > pair.comparison`. Of two duplicates
/// listed on both sides only the pair whose source has the lower id survives. See the module docs
/// for what this guarantees.
pub const fn is_valid_pair(pair: Pair, overlap: Overlap) -> bool {
    pair.source != pair.comparison
        && !(overlap.source_in_comparison
            && overlap.comparison_in_source
            && pair.source > pair.comparison)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use proptest::prelude::*;
    use unicode_normalization::UnicodeNormalization;

    use super::*;

    const H1: Sha1Digest = [1; 20];
    const H2: Sha1Digest = [2; 20];
    const LOOSE: MatchOptions = MatchOptions {
        strict_prefix: false,
    };
    const STRICT: MatchOptions = MatchOptions {
        strict_prefix: true,
    };

    fn file(id: FileId, name: &str) -> MatchFacts<'_> {
        MatchFacts {
            id,
            name,
            hash: None,
            capture_ts: None,
            size: 1,
        }
    }

    fn at(facts: MatchFacts<'_>, ts: i64) -> MatchFacts<'_> {
        MatchFacts {
            capture_ts: Some(ts),
            ..facts
        }
    }

    fn hashed(facts: MatchFacts<'_>, hash: Sha1Digest) -> MatchFacts<'_> {
        MatchFacts {
            hash: Some(hash),
            ..facts
        }
    }

    fn hit(mode: MatchMode, source: &MatchFacts<'_>, candidate: &MatchFacts<'_>) -> bool {
        is_match(mode, source, candidate, LOOSE)
    }

    fn hit_strict(mode: MatchMode, source: &MatchFacts<'_>, candidate: &MatchFacts<'_>) -> bool {
        is_match(mode, source, candidate, STRICT)
    }

    /// A file of a scenario; its id is its position in the list plus one.
    #[derive(Debug, Clone)]
    struct Item {
        name: String,
        hash: Option<Sha1Digest>,
        capture_ts: Option<i64>,
        in_source: bool,
        in_comparison: bool,
    }

    impl Item {
        /// A file listed on both sides.
        fn dup(name: &str) -> Self {
            Self {
                name: name.to_owned(),
                hash: Some(H1),
                capture_ts: None,
                in_source: true,
                in_comparison: true,
            }
        }

        fn facts(&self, index: usize) -> MatchFacts<'_> {
            MatchFacts {
                id: i64::try_from(index).unwrap() + 1,
                name: &self.name,
                hash: self.hash,
                capture_ts: self.capture_ts,
                size: 1,
            }
        }
    }

    /// Brute force: every `(source, comparison)` of the scenario that matches.
    fn candidates(items: &[Item], mode: MatchMode, opts: MatchOptions) -> Vec<(usize, usize)> {
        let mut found = Vec::new();
        for (s, source) in items.iter().enumerate() {
            for (c, candidate) in items.iter().enumerate() {
                if source.in_source
                    && candidate.in_comparison
                    && is_match(mode, &source.facts(s), &candidate.facts(c), opts)
                {
                    found.push((s, c));
                }
            }
        }
        found
    }

    /// The candidates the orientation rule keeps.
    fn valid_pairs(items: &[Item], candidates: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let keep = |&(s, c): &(usize, usize)| {
            let pair = Pair::new(items[s].facts(s).id, items[c].facts(c).id);
            let overlap = Overlap {
                source_in_comparison: items[s].in_comparison,
                comparison_in_source: items[c].in_source,
            };
            is_valid_pair(pair, overlap)
        };
        candidates.iter().copied().filter(keep).collect()
    }

    /// The connected components with at least two members.
    fn groups(count: usize, pairs: &[(usize, usize)]) -> Vec<Vec<usize>> {
        let mut label: Vec<usize> = (0..count).collect();
        loop {
            let mut changed = false;
            for &(a, b) in pairs {
                let low = label[a].min(label[b]);
                for node in [a, b] {
                    if label[node] != low {
                        label[node] = low;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let mut by_label: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (node, root) in label.into_iter().enumerate() {
            by_label.entry(root).or_default().push(node);
        }
        by_label.into_values().filter(|g| g.len() > 1).collect()
    }

    #[test]
    fn hash_needs_both_hashes_and_they_must_be_equal() {
        let source = hashed(file(1, "a.jpg"), H1);
        assert!(hit(MatchMode::Hash, &source, &hashed(file(2, "z.png"), H1)));
        assert!(!hit(
            MatchMode::Hash,
            &source,
            &hashed(file(2, "a.jpg"), H2)
        ));
        assert!(!hit(MatchMode::Hash, &source, &file(2, "a.jpg")));
        assert!(!hit(MatchMode::Hash, &file(1, "a.jpg"), &source));
        assert!(!hit(MatchMode::Hash, &file(1, "a.jpg"), &file(2, "a.jpg")));
    }

    #[test]
    fn name_and_time_needs_the_same_name_and_the_same_known_time() {
        let source = at(file(1, "IMG_0001.JPG"), 100);
        let check = |candidate: MatchFacts<'_>| hit(MatchMode::NameAndTime, &source, &candidate);
        assert!(check(at(file(2, "IMG_0001.JPG"), 100)));
        assert!(check(at(file(2, "img_0001.jpg"), 100)));
        assert!(!check(at(file(2, "IMG_0001.JPG"), 101)));
        assert!(!check(file(2, "IMG_0001.JPG")));
        assert!(!check(at(file(2, "IMG_0001.CR3"), 100)));
        assert!(!check(at(file(2, "IMG_0002.JPG"), 100)));
        let unknown = file(1, "IMG_0001.JPG");
        assert!(!hit(
            MatchMode::NameAndTime,
            &unknown,
            &file(2, "IMG_0001.JPG")
        ));
    }

    #[test]
    fn name_different_time_needs_the_same_name_and_different_times() {
        let source = at(file(1, "IMG_0001.JPG"), 100);
        let check =
            |candidate: MatchFacts<'_>| hit(MatchMode::NameDifferentTime, &source, &candidate);
        assert!(check(at(file(2, "img_0001.jpg"), 101)));
        assert!(check(file(2, "IMG_0001.JPG")));
        assert!(!check(at(file(2, "IMG_0001.JPG"), 100)));
        assert!(!check(at(file(2, "IMG_0002.JPG"), 101)));
        assert!(!check(at(file(2, "IMG_0001.CR3"), 101)));
        let unknown = file(1, "a.jpg");
        assert!(hit(
            MatchMode::NameDifferentTime,
            &unknown,
            &at(file(2, "a.jpg"), 5)
        ));
        assert!(!hit(
            MatchMode::NameDifferentTime,
            &unknown,
            &file(2, "a.jpg")
        ));
    }

    #[test]
    fn modes_two_and_three_split_same_name_files_by_time() {
        let times = [None, Some(1), Some(2)];
        for a in times {
            for b in times {
                let source = MatchFacts {
                    capture_ts: a,
                    ..file(1, "x.jpg")
                };
                let candidate = MatchFacts {
                    capture_ts: b,
                    ..file(2, "X.JPG")
                };
                let same = hit(MatchMode::NameAndTime, &source, &candidate);
                let different = hit(MatchMode::NameDifferentTime, &source, &candidate);
                let expected = match (a, b) {
                    (None, None) => (false, false),
                    (Some(x), Some(y)) if x == y => (true, false),
                    _ => (false, true),
                };
                assert_eq!((same, different), expected, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn names_compare_case_and_normalisation_insensitively() {
        let nfc = "é.jpg";
        let nfd = "e\u{301}.JPG";
        assert_ne!(nfc, nfd);
        let (a, b) = (at(file(1, nfc), 5), at(file(2, nfd), 5));
        assert!(hit(MatchMode::NameAndTime, &a, &b));
        assert!(hit(MatchMode::NameAndTime, &b, &a));
        assert!(hit(MatchMode::NameDifferentTime, &a, &at(file(3, nfd), 6)));
        let edited = file(4, "E\u{301}_edit.png");
        assert!(hit(MatchMode::NamePrefix, &a, &edited));
        assert!(hit(MatchMode::NamePrefix, &b, &edited));
    }

    #[test]
    fn a_file_never_matches_itself() {
        let identical = [
            (MatchMode::Hash, true),
            (MatchMode::NameAndTime, true),
            (MatchMode::NameDifferentTime, false),
            (MatchMode::NamePrefix, true),
        ];
        for (mode, with_other_id) in identical {
            for opts in [LOOSE, STRICT] {
                let a = hashed(at(file(7, "a.jpg"), 1), H1);
                let twin = MatchFacts { id: 8, ..a };
                assert!(!is_match(mode, &a, &a, opts), "{mode:?}");
                assert!(
                    !is_match(mode, &a, &MatchFacts { id: 7, ..twin }, opts),
                    "{mode:?}"
                );
                assert_eq!(is_match(mode, &a, &twin, opts), with_other_id, "{mode:?}");
            }
        }
    }

    #[test]
    fn prefix_boundary_table() {
        // (source stem, candidate stem, default rule, strict_prefix)
        let table = [
            ("img_0001", "img_0001", true, true),
            ("img_0001", "img_0001_edit", true, true),
            ("img_0001", "img_0001-1", true, true),
            ("img_0001", "img_0001 (1)", true, true),
            ("img_0001", "img_0001.edit", true, true),
            ("img_0001", "img_0001😀", true, true),
            ("img_0001", "img_00012", false, true),
            ("img_0001", "img_0001a", false, true),
            ("img_0001", "img_0001é", false, true),
            ("img_0001", "img_0001中", false, true),
            ("img_0001", "img_0001٢", false, true),
            ("img_0001", "img_000", false, false),
            ("img_0001", "xx_img_0001", false, false),
            ("img_0001", "img_0002", false, false),
            ("a", "a_b", true, true),
            ("a", "ab", false, true),
            ("ab", "a", false, false),
            ("img_", "img_0001", false, true),
            ("", "", false, false),
            ("", "anything", false, false),
        ];
        for (source, candidate, default, strict) in table {
            assert_eq!(
                stem_prefix_match(source, candidate, false),
                default,
                "{source:?} vs {candidate:?}"
            );
            assert_eq!(
                stem_prefix_match(source, candidate, true),
                strict,
                "{source:?} vs {candidate:?} (strict)"
            );
        }
    }

    #[test]
    fn prefix_works_on_names_and_ignores_extensions() {
        let source = file(1, "IMG_0001.CR3");
        let names = [
            ("IMG_0001.JPG", true),
            ("img_0001_edit.jpg", true),
            ("IMG_0001-1.jpg", true),
            ("IMG_0001 (1).jpg", true),
            ("IMG_0001.edit.jpg", true),
            ("IMG_0001", true),
            ("IMG_00012.jpg", false),
            ("IMG_0002.jpg", false),
            ("IMG_000.jpg", false),
            ("my_IMG_0001.jpg", false),
        ];
        for (name, expected) in names {
            assert_eq!(
                hit(MatchMode::NamePrefix, &source, &file(2, name)),
                expected,
                "{name}"
            );
        }
        let multi_dot = file(1, "archive.tar.gz");
        assert!(hit(
            MatchMode::NamePrefix,
            &multi_dot,
            &file(2, "archive.tar.bak.gz")
        ));
        assert!(!hit(
            MatchMode::NamePrefix,
            &multi_dot,
            &file(2, "archive.zip")
        ));
    }

    #[test]
    fn prefix_is_directional() {
        let short = file(1, "IMG_0001.jpg");
        let long = file(2, "IMG_0001_edit.jpg");
        assert!(hit(MatchMode::NamePrefix, &short, &long));
        assert!(!hit(MatchMode::NamePrefix, &long, &short));
        let same_stem = file(3, "img_0001.PNG");
        assert!(hit(MatchMode::NamePrefix, &short, &same_stem));
        assert!(hit(MatchMode::NamePrefix, &same_stem, &short));
    }

    #[test]
    fn strict_prefix_drops_only_the_boundary() {
        let source = file(1, "IMG_0001.jpg");
        let twelve = file(2, "IMG_00012.jpg");
        assert!(!hit(MatchMode::NamePrefix, &source, &twelve));
        assert!(hit_strict(MatchMode::NamePrefix, &source, &twelve));
        assert!(!hit_strict(MatchMode::NamePrefix, &twelve, &source));
        assert!(!hit_strict(
            MatchMode::NamePrefix,
            &source,
            &file(3, "IMG_0002.jpg")
        ));
        let nameless = file(4, "");
        assert!(!hit_strict(
            MatchMode::NamePrefix,
            &nameless,
            &file(5, "a.jpg")
        ));
        assert!(!hit_strict(MatchMode::NamePrefix, &nameless, &file(5, "")));
    }

    #[test]
    fn keys_are_the_normalised_column_values() {
        assert_eq!(name_key("IMG_0001.JPG"), "img_0001.jpg");
        assert_eq!(name_key("e\u{301}.jpg"), "é.jpg");
        assert_eq!(stem_key("IMG_0001.CR3"), "img_0001");
        assert_eq!(stem_key("IMG_0001.CR3.xmp"), "img_0001.cr3");
        assert_eq!(stem_key("noext"), "noext");
        let stamped = at(file(1, "IMG.JPG"), 42);
        assert_eq!(name_time_key(&stamped), Some(("img.jpg".to_owned(), 42)));
        assert_eq!(name_time_key(&file(1, "IMG.JPG")), None);
        assert_eq!(hash_key(&hashed(file(1, "a"), H1)), Some(H1));
        assert_eq!(hash_key(&file(1, "a")), None);
    }

    #[test]
    fn prefix_bounds_are_the_prefix_and_its_successor() {
        let bounds = |stem: &str| stem_prefix_bounds(stem).map(|b| (b.lower, b.upper));
        let range =
            |lower: &str, upper: Option<&str>| Some((lower.to_owned(), upper.map(str::to_owned)));
        assert_eq!(bounds("img_0001"), range("img_0001", Some("img_0002")));
        assert_eq!(bounds("abc"), range("abc", Some("abd")));
        assert_eq!(bounds("é"), range("é", Some("ê")));
        assert_eq!(bounds("a\u{d7ff}"), range("a\u{d7ff}", Some("a\u{e000}")));
        assert_eq!(bounds("a\u{10ffff}"), range("a\u{10ffff}", Some("b")));
        assert_eq!(bounds("\u{10ffff}"), range("\u{10ffff}", None));
        assert_eq!(bounds(""), None);
    }

    #[test]
    fn prefix_bounds_contain_the_prefixed_stems() {
        let bounds = stem_prefix_bounds("img_0001").unwrap();
        for stem in [
            "img_0001",
            "img_0001_edit",
            "img_00012",
            "img_0001\u{10ffff}",
        ] {
            assert!(bounds.contains(stem), "{stem}");
        }
        for stem in ["", "img_000", "img_0002", "img_0000z", "IMG_0001", "z"] {
            assert!(!bounds.contains(stem), "{stem}");
        }
        let open_ended = stem_prefix_bounds("\u{10ffff}").unwrap();
        assert!(open_ended.contains("\u{10ffff}z"));
        assert!(!open_ended.contains("z"));
    }

    #[test]
    fn only_media_kinds_take_part() {
        for kind in FileKind::ALL {
            let media = matches!(kind, FileKind::Image | FileKind::Video | FileKind::Audio);
            assert_eq!(participates(kind), media, "{kind:?}");
        }
    }

    #[test]
    fn mode_list_is_complete_and_flags_the_directional_mode() {
        // exhaustive on purpose: a new mode does not compile until ALL knows about it
        fn position(mode: MatchMode) -> usize {
            match mode {
                MatchMode::Hash => 0,
                MatchMode::NameAndTime => 1,
                MatchMode::NameDifferentTime => 2,
                MatchMode::NamePrefix => 3,
            }
        }
        for (index, mode) in MatchMode::ALL.into_iter().enumerate() {
            assert_eq!(position(mode), index);
        }
        let directional: Vec<MatchMode> = MatchMode::ALL
            .into_iter()
            .filter(|mode| !mode.is_symmetric())
            .collect();
        assert_eq!(directional, [MatchMode::NamePrefix]);
    }

    #[test]
    fn sides_flip_and_pairs_expose_both_files() {
        assert_eq!(PlanSide::Source.opposite(), PlanSide::Comparison);
        assert_eq!(PlanSide::Comparison.opposite(), PlanSide::Source);
        let pair = Pair::new(3, 9);
        assert_eq!(pair.on_side(PlanSide::Source), 3);
        assert_eq!(pair.on_side(PlanSide::Comparison), 9);
        assert_eq!(pair.counterpart_of(PlanSide::Source), 9);
        assert_eq!(pair.counterpart_of(PlanSide::Comparison), 3);
    }

    #[test]
    fn orientation_rule_table() {
        for source_in_comparison in [false, true] {
            for comparison_in_source in [false, true] {
                let overlap = Overlap {
                    source_in_comparison,
                    comparison_in_source,
                };
                let both = source_in_comparison && comparison_in_source;
                assert!(!is_valid_pair(Pair::new(4, 4), overlap));
                assert!(is_valid_pair(Pair::new(1, 2), overlap));
                assert_eq!(is_valid_pair(Pair::new(2, 1), overlap), !both);
            }
        }
        assert_eq!(Overlap::default(), Overlap::default());
        assert!(is_valid_pair(Pair::new(9, 1), Overlap::default()));
    }

    #[test]
    fn same_folder_in_both_panes_keeps_the_lowest_and_the_highest_id() {
        let items = [Item::dup("a.jpg"), Item::dup("b.jpg"), Item::dup("c.jpg")];
        let valid = valid_pairs(&items, &candidates(&items, MatchMode::Hash, LOOSE));
        assert_eq!(valid, [(0, 1), (0, 2), (1, 2)]);
        let sources: HashSet<usize> = valid.iter().map(|&(s, _)| s).collect();
        let comparisons: HashSet<usize> = valid.iter().map(|&(_, c)| c).collect();
        // delete the source side keeps the highest id, delete the comparison side the lowest
        assert_eq!(sources, HashSet::from([0, 1]));
        assert_eq!(comparisons, HashSet::from([1, 2]));
    }

    #[test]
    fn nested_folders_only_restrict_the_pairs_inside_the_overlap() {
        // the source pane shows everything, the comparison pane one subfolder (items 1 and 2)
        let mut items = vec![
            Item::dup("a.jpg"),
            Item::dup("b.jpg"),
            Item::dup("c.jpg"),
            Item::dup("d.jpg"),
        ];
        items[0].in_comparison = false;
        items[3].in_comparison = false;
        let valid = valid_pairs(&items, &candidates(&items, MatchMode::Hash, LOOSE));
        assert_eq!(valid, [(0, 1), (0, 2), (1, 2), (3, 1), (3, 2)]);
    }

    #[test]
    fn a_group_survives_but_a_file_can_lose_its_only_counterpart() {
        // a(1s) ~ b(2s) ~ c(1s), but a !~ c: deleting the source side removes a and b although
        // the only match of a is b, which is deleted as well. Only c remains.
        let stamped = |ts| Item {
            capture_ts: Some(ts),
            ..Item::dup("x.jpg")
        };
        let items = [stamped(1), stamped(2), stamped(1)];
        let cands = candidates(&items, MatchMode::NameDifferentTime, LOOSE);
        let valid = valid_pairs(&items, &cands);
        assert_eq!(valid, [(0, 1), (1, 2)]);
        let deleted: HashSet<usize> = valid.iter().map(|&(s, _)| s).collect();
        assert_eq!(deleted, HashSet::from([0, 1]));
        assert_eq!(groups(items.len(), &cands), [vec![0, 1, 2]]);
    }

    fn arb_item() -> impl Strategy<Value = Item> {
        (
            "[abAB1_ é-]{0,4}(\\.(jpg|JPG|png))?",
            prop::option::of(0u8..3),
            prop::option::of(0i64..3),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(|(name, hash, capture_ts, in_source, in_comparison)| Item {
                name,
                hash: hash.map(|byte| [byte; 20]),
                capture_ts,
                in_source,
                in_comparison,
            })
    }

    fn arb_text(
        alphabet: &'static [char],
        min: usize,
        max: usize,
    ) -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(alphabet), min..max)
            .prop_map(|chars| chars.into_iter().collect())
    }

    const STEM_CHARS: &[char] = &['a', '1', '_', ' ', 'é', '中'];
    const EDGE_CHARS: &[char] = &['a', 'é', '\u{d7ff}', '\u{e000}', '\u{10ffff}'];

    proptest! {
        #[test]
        fn symmetric_modes_ignore_the_direction(
            a in arb_item(),
            b in arb_item(),
            strict in any::<bool>(),
        ) {
            let opts = MatchOptions { strict_prefix: strict };
            let (fa, fb) = (a.facts(0), b.facts(1));
            for mode in MatchMode::ALL.into_iter().filter(|mode| mode.is_symmetric()) {
                prop_assert_eq!(is_match(mode, &fa, &fb, opts), is_match(mode, &fb, &fa, opts));
            }
        }

        #[test]
        fn prefix_matches_both_ways_only_for_equal_stems(a in arb_item(), b in arb_item()) {
            let (fa, fb) = (a.facts(0), b.facts(1));
            let forward = is_match(MatchMode::NamePrefix, &fa, &fb, LOOSE);
            let backward = is_match(MatchMode::NamePrefix, &fb, &fa, LOOSE);
            let equal = !stem_key(&a.name).is_empty() && stem_key(&a.name) == stem_key(&b.name);
            prop_assert_eq!(forward && backward, equal);
        }

        #[test]
        fn strict_prefix_only_widens_the_matches(a in arb_item(), b in arb_item()) {
            let (fa, fb) = (a.facts(0), b.facts(1));
            for mode in MatchMode::ALL {
                if is_match(mode, &fa, &fb, LOOSE) {
                    prop_assert!(is_match(mode, &fa, &fb, STRICT));
                }
                if mode != MatchMode::NamePrefix {
                    prop_assert_eq!(
                        is_match(mode, &fa, &fb, LOOSE),
                        is_match(mode, &fa, &fb, STRICT)
                    );
                }
            }
        }

        #[test]
        fn an_id_never_matches_itself(a in arb_item(), b in arb_item(), strict in any::<bool>()) {
            let opts = MatchOptions { strict_prefix: strict };
            let (fa, fb) = (a.facts(3), b.facts(3));
            for mode in MatchMode::ALL {
                prop_assert!(!is_match(mode, &fa, &fa, opts));
                prop_assert!(!is_match(mode, &fa, &fb, opts));
            }
        }

        #[test]
        fn keys_agree_with_the_predicate(a in arb_item(), b in arb_item()) {
            let (fa, fb) = (a.facts(0), b.facts(1));
            let by_hash = hash_key(&fa).is_some() && hash_key(&fa) == hash_key(&fb);
            let by_name_time =
                name_time_key(&fa).is_some() && name_time_key(&fa) == name_time_key(&fb);
            let by_name_other_time =
                name_key(&a.name) == name_key(&b.name) && a.capture_ts != b.capture_ts;
            prop_assert_eq!(is_match(MatchMode::Hash, &fa, &fb, LOOSE), by_hash);
            prop_assert_eq!(is_match(MatchMode::NameAndTime, &fa, &fb, LOOSE), by_name_time);
            prop_assert_eq!(
                is_match(MatchMode::NameDifferentTime, &fa, &fb, LOOSE),
                by_name_other_time
            );
        }

        #[test]
        fn spelling_variants_do_not_change_the_matches(
            a in arb_item(),
            b in arb_item(),
            strict in any::<bool>(),
        ) {
            let opts = MatchOptions { strict_prefix: strict };
            let respelled = a.name.nfd().collect::<String>().to_uppercase();
            let variant = Item { name: respelled, ..a.clone() };
            let fb = b.facts(1);
            for mode in MatchMode::ALL {
                prop_assert_eq!(
                    is_match(mode, &a.facts(0), &fb, opts),
                    is_match(mode, &variant.facts(0), &fb, opts)
                );
                prop_assert_eq!(
                    is_match(mode, &fb, &a.facts(0), opts),
                    is_match(mode, &fb, &variant.facts(0), opts)
                );
            }
        }

        #[test]
        fn prefix_bounds_select_exactly_the_stems_with_that_prefix(
            prefix in arb_text(EDGE_CHARS, 1, 4),
            suffix in arb_text(EDGE_CHARS, 0, 4),
            other in arb_text(EDGE_CHARS, 0, 5),
        ) {
            let bounds = stem_prefix_bounds(&prefix).unwrap();
            let extended = format!("{prefix}{suffix}");
            for text in [&prefix, &extended, &other] {
                prop_assert_eq!(bounds.contains(text), text.starts_with(&prefix));
            }
        }

        #[test]
        fn prefix_match_is_the_bounds_plus_the_boundary(
            source in arb_text(STEM_CHARS, 0, 4),
            tail in arb_text(STEM_CHARS, 0, 3),
            other in arb_text(STEM_CHARS, 0, 5),
        ) {
            let extended = format!("{source}{tail}");
            for candidate in [&source, &extended, &other] {
                let inside = stem_prefix_bounds(&source).is_some_and(|b| b.contains(candidate));
                let after = candidate.chars().nth(source.chars().count());
                let boundary = after.is_none_or(|c| !c.is_alphanumeric());
                prop_assert_eq!(stem_prefix_match(&source, candidate, true), inside);
                prop_assert_eq!(stem_prefix_match(&source, candidate, false), inside && boundary);
            }
        }

        #[test]
        fn disjoint_scopes_keep_every_candidate(
            items in prop::collection::vec(arb_item(), 2..8),
            strict in any::<bool>(),
        ) {
            let opts = MatchOptions { strict_prefix: strict };
            let items: Vec<Item> = items
                .into_iter()
                .map(|item| Item { in_comparison: item.in_comparison && !item.in_source, ..item })
                .collect();
            for mode in MatchMode::ALL {
                let found = candidates(&items, mode, opts);
                prop_assert_eq!(valid_pairs(&items, &found), found);
            }
        }

        #[test]
        fn symmetric_modes_keep_one_orientation_of_each_pair(
            items in prop::collection::vec(arb_item(), 2..8),
            strict in any::<bool>(),
        ) {
            let opts = MatchOptions { strict_prefix: strict };
            for mode in MatchMode::ALL.into_iter().filter(|mode| mode.is_symmetric()) {
                let found = candidates(&items, mode, opts);
                let valid = valid_pairs(&items, &found);
                let unordered: HashSet<(usize, usize)> =
                    found.iter().map(|&(s, c)| (s.min(c), s.max(c))).collect();
                for (low, high) in unordered {
                    let kept = valid
                        .iter()
                        .filter(|&&pair| pair == (low, high) || pair == (high, low))
                        .count();
                    prop_assert_eq!(kept, 1, "mode {:?}, pair {:?}", mode, (low, high));
                }
            }
        }

        #[test]
        fn every_match_group_keeps_a_member_under_either_delete_action(
            items in prop::collection::vec(arb_item(), 2..8),
            strict in any::<bool>(),
        ) {
            let opts = MatchOptions { strict_prefix: strict };
            for mode in MatchMode::ALL {
                let found = candidates(&items, mode, opts);
                let valid = valid_pairs(&items, &found);
                let sources: HashSet<usize> = valid.iter().map(|&(s, _)| s).collect();
                let comparisons: HashSet<usize> = valid.iter().map(|&(_, c)| c).collect();
                for group in groups(items.len(), &found) {
                    let source_survivor = group.iter().any(|member| !sources.contains(member));
                    let comparison_survivor =
                        group.iter().any(|member| !comparisons.contains(member));
                    prop_assert!(
                        source_survivor,
                        "mode {:?}: delete source empties {:?} in {:?}",
                        mode, group, items
                    );
                    prop_assert!(
                        comparison_survivor,
                        "mode {:?}: delete comparison empties {:?} in {:?}",
                        mode, group, items
                    );
                }
            }
        }
    }
}
