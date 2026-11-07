//! Destination-name conflicts (plan 5.8 and 5.9): numbered names, the decision behind each
//! [`ConflictPolicy`], and the names sidecars take when their owner is renamed.
//!
//! Pure like the rest of the crate: the planner says what exists (a closure over its index lookups
//! and the names the plan has already claimed), the executor applies the answers, and nothing here
//! touches a disk. Names are resolved at plan time so the preview is exact, and `decide` is asked
//! again at execution time with fresh facts.
//!
//! # Numbered names
//!
//! [`numbered`] inserts `_n` before the *last* extension: `IMG_0001.jpg` becomes `IMG_0001_1.jpg`,
//! `archive.tar.gz` becomes `archive.tar_1.gz`, and a name without an extension (`README`) or with
//! only a leading dot (`.bashrc`) gets the number at the end. Numbers are always appended, never
//! incremented: `IMG_0001_1.jpg` becomes `IMG_0001_1_1.jpg`. The stem is shortened when the result
//! would pass the length cap (255 bytes, or the 143 of an encrypted Synology share), never the
//! extension.
//!
//! [`unique`] returns the name itself when it is free, otherwise the first free numbered name, and
//! gives up with [`ConflictError::NoFreeName`] after [`MAX_ATTEMPTS`] numbers. The caller owns the
//! meaning of "taken", so case sensitivity stays its decision; [`unique_case_insensitive`] hands the
//! closure [`normalize_name`] keys instead, for plans that must avoid case-only and
//! normalisation-only collisions (`IMG.JPG` against `img.jpg`) between files of the same plan.
//!
//! # Policies
//!
//! [`decide`] turns a policy and the facts about both files into a [`ConflictDecision`]. Nothing is
//! ever overwritten in place: replacing means the existing file goes to the trash first.
//!
//! | Policy | Nothing at the destination | A file is there |
//! |---|---|---|
//! | Smart | `Proceed` | same size and same SHA-1: `Skip(Identical)`; anything else: `Rename` |
//! | Skip | `Proceed` | `Skip(Policy)` |
//! | Rename | `Proceed` | `Rename` |
//! | Replace | `Proceed` | `ReplaceExisting` |
//!
//! Smart only calls two files identical when it can prove it. A missing hash is not a match, so the
//! planner should hash both files first; [`needs_hashes`] says when that is worth the I/O (only for a
//! same-size collision under Smart).
//!
//! # Sidecars
//!
//! A sidecar keeps its link to its owner by taking a name derived from the owner's *new* name
//! ([`sidecar_destination`]): `IMG_0001.CR3.xmp` follows `IMG_0001.CR3` (exact link, the owner's full
//! name plus the sidecar's extension) and `IMG_0001.xmp` follows the owner's stem (stem link).
//!
//! To number an owner together with its sidecars, make the closure given to [`unique`] check the
//! sidecar names too:
//!
//! ```
//! use apm_core::conflict::{SidecarNaming, sidecar_destination, unique};
//!
//! let taken = ["IMG_0001.CR3", "IMG_0001_1.CR3.xmp"];
//! let name = unique("IMG_0001.CR3", |candidate| {
//!     let sidecar =
//!         sidecar_destination(SidecarNaming::Exact, "IMG_0001.CR3", candidate, "IMG_0001.CR3.xmp");
//!     taken.contains(&candidate) || taken.contains(&sidecar.as_str())
//! });
//! assert_eq!(name.as_deref(), Ok("IMG_0001_2.CR3"));
//! ```
//!
//! Owners that must share one number (a `.CR3` and a `.JPG` behind one stem-linked `.xmp`) can use
//! [`first_free_number`] with [`numbered`], and an owner whose exact sidecars add a tail to its name
//! should pass a `max_bytes` smaller by the longest tail to the `*_within` functions.

use crate::dto::ConflictPolicy;
use crate::names::{normalize_name, split_name};

/// Longest file name, in bytes, accepted by common filesystems. Encrypted Synology shares only allow
/// 143; callers with such a share pass their own limit to the `*_within` functions and to
/// `pattern::RenderOptions`.
pub const DEFAULT_MAX_NAME_BYTES: usize = 255;

/// How many numbers [`unique`] tries (`_1` to `_10000`) before giving up.
pub const MAX_ATTEMPTS: u32 = 10_000;

/// Smallest cap the `*_within` functions honour, so a number always has room next to some stem.
const MIN_NAME_BYTES: usize = 16;

/// Why no free name could be found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConflictError {
    /// Every numbered variant of the name is taken.
    #[error("no free name for {name:?} after {attempts} attempts")]
    NoFreeName {
        /// The name that was being numbered.
        name: String,
        /// How many numbers were tried.
        attempts: u32,
    },
}

/// `name` with `_n` inserted before its last extension, within [`DEFAULT_MAX_NAME_BYTES`].
/// `n == 0` returns the name unchanged. See [`numbered_within`] for the length cap.
pub fn numbered(name: &str, n: u32) -> String {
    numbered_within(name, n, DEFAULT_MAX_NAME_BYTES)
}

/// [`numbered`] with an explicit length cap in bytes. The stem is cut on a character boundary to
/// make the result fit; the extension is kept. Caps below 16 bytes are raised to 16. An extension too
/// long to fit is cut as well, but the number always survives.
pub fn numbered_within(name: &str, n: u32, max_bytes: usize) -> String {
    if n == 0 {
        return name.to_owned();
    }
    let max_bytes = max_bytes.max(MIN_NAME_BYTES);
    let (stem, extension) = split_name(name);
    let suffix = format!("_{n}");
    let tail = extension.map(|ext| format!(".{ext}")).unwrap_or_default();
    let fixed = suffix.len() + tail.len();
    if fixed >= max_bytes {
        return cut_to_bytes(&format!("{suffix}{tail}"), max_bytes).to_owned();
    }
    format!("{}{suffix}{tail}", cut_to_bytes(stem, max_bytes - fixed))
}

/// The longest prefix of `text` that is at most `max_bytes` long and ends on a character boundary.
pub(crate) fn cut_to_bytes(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

/// The smallest number in `1..=`[`MAX_ATTEMPTS`] for which `is_free` holds; `None` when there is
/// none. The building block of [`unique`], for callers that number several names in lockstep.
pub fn first_free_number(mut is_free: impl FnMut(u32) -> bool) -> Option<u32> {
    (1..=MAX_ATTEMPTS).find(|&n| is_free(n))
}

/// The first free name: `name` itself when `exists` says it is free, else the first free
/// [`numbered`] one. `exists` receives candidate names exactly as they would be created, so the
/// check is case-sensitive unless the closure says otherwise.
pub fn unique(name: &str, exists: impl FnMut(&str) -> bool) -> Result<String, ConflictError> {
    unique_within(name, DEFAULT_MAX_NAME_BYTES, exists)
}

/// [`unique`] with an explicit length cap for the numbered names (see [`numbered_within`]). A free
/// `name` is returned as it is, whatever its length.
pub fn unique_within(
    name: &str,
    max_bytes: usize,
    exists: impl FnMut(&str) -> bool,
) -> Result<String, ConflictError> {
    search(name, max_bytes, exists)
}

/// [`unique`] for planners that must avoid collisions that differ only by case or Unicode
/// normalisation. `exists` receives [`normalize_name`] keys, so a set of normalised names (such as
/// the `name_norm` column) can answer it; the returned name keeps its original case.
pub fn unique_case_insensitive(
    name: &str,
    exists: impl FnMut(&str) -> bool,
) -> Result<String, ConflictError> {
    unique_case_insensitive_within(name, DEFAULT_MAX_NAME_BYTES, exists)
}

/// [`unique_case_insensitive`] with an explicit length cap for the numbered names.
pub fn unique_case_insensitive_within(
    name: &str,
    max_bytes: usize,
    mut exists: impl FnMut(&str) -> bool,
) -> Result<String, ConflictError> {
    search(name, max_bytes, |candidate| {
        exists(&normalize_name(candidate))
    })
}

fn search(
    name: &str,
    max_bytes: usize,
    mut taken: impl FnMut(&str) -> bool,
) -> Result<String, ConflictError> {
    if !taken(name) {
        return Ok(name.to_owned());
    }
    first_free_number(|n| !taken(&numbered_within(name, n, max_bytes)))
        .map(|n| numbered_within(name, n, max_bytes))
        .ok_or_else(|| ConflictError::NoFreeName {
            name: name.to_owned(),
            attempts: MAX_ATTEMPTS,
        })
}

/// SHA-1 of a file's bytes (plan assumption 12), the same 20 bytes as the `files.hash` column.
pub type ContentHash = [u8; 20];

/// What [`decide`] needs to know about one side of a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentInfo {
    /// Size in bytes.
    pub size: u64,
    /// SHA-1, when it is known. Without it two files are never considered identical.
    pub hash: Option<ContentHash>,
}

/// The file already at the destination. Never the incoming file itself: a file that is already in
/// place is not a conflict, and the planner must filter it out before asking.
pub type ExistingInfo = ContentInfo;

/// The file being moved or copied to the destination.
pub type IncomingInfo = ContentInfo;

impl ContentInfo {
    /// Whether both sizes and both hashes are known to be equal.
    fn is_provably_identical(&self, other: &Self) -> bool {
        self.size == other.size && matches!((self.hash, other.hash), (Some(a), Some(b)) if a == b)
    }
}

/// What to do with the incoming file. See the module docs for the table behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictDecision {
    /// Nothing is at the destination: transfer under the planned name.
    Proceed,
    /// Leave both files as they are and do not transfer the incoming one.
    Skip(SkipReason),
    /// Transfer under the first free numbered name (see [`unique`]).
    Rename,
    /// Move the existing file to the trash, then transfer under the planned name. Never an
    /// in-place overwrite.
    ReplaceExisting,
}

/// Why a file is skipped, so the preview can tell the two cases apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The smart policy found the same content at the destination.
    Identical,
    /// The skip policy was chosen.
    Policy,
}

/// Decides what happens to `incoming` given the policy and the file already at its destination
/// (`None` when the name is free).
pub fn decide(
    policy: ConflictPolicy,
    existing: Option<ExistingInfo>,
    incoming: IncomingInfo,
) -> ConflictDecision {
    let Some(existing) = existing else {
        return ConflictDecision::Proceed;
    };
    match policy {
        ConflictPolicy::Smart if existing.is_provably_identical(&incoming) => {
            ConflictDecision::Skip(SkipReason::Identical)
        }
        ConflictPolicy::Smart | ConflictPolicy::Rename => ConflictDecision::Rename,
        ConflictPolicy::Skip => ConflictDecision::Skip(SkipReason::Policy),
        ConflictPolicy::Replace => ConflictDecision::ReplaceExisting,
    }
}

/// Whether [`decide`] would give a better answer with hashes it has not been given: only for the
/// smart policy, on a same-size collision where a hash is missing. Hash the two files and ask again
/// when this is true; for every other case the hashes cannot change the outcome.
pub fn needs_hashes(
    policy: ConflictPolicy,
    existing: Option<ExistingInfo>,
    incoming: IncomingInfo,
) -> bool {
    matches!(policy, ConflictPolicy::Smart)
        && existing.is_some_and(|existing| {
            existing.size == incoming.size && (existing.hash.is_none() || incoming.hash.is_none())
        })
}

/// How a sidecar's name is built from its owner's, matching the two link kinds of the sidecar
/// matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarNaming {
    /// The owner's full name plus the sidecar extension: `IMG_0001.CR3.xmp`, `photo.jpg.json`.
    Exact,
    /// The owner's stem plus the sidecar extension: `IMG_0001.xmp` for `IMG_0001.CR3` and
    /// `IMG_0001.JPG` alike.
    Stem,
}

/// The name a sidecar takes when its owner goes from `old_owner_name` to `new_owner_name`.
///
/// `sidecar_name` is the sidecar's current name. What follows the owner's old name (exact link) or
/// old stem (stem link) in it, usually `.xmp`, is kept as it is and appended to the new name or new
/// stem: `IMG_0001.CR3.xmp` goes with `IMG_0001.CR3` to `IMG_0001_1.CR3.xmp`, and `IMG_0001.xmp`
/// goes with it to `IMG_0001_1.xmp`. The old name is matched ignoring ASCII case, as the matcher
/// does; when it is not a prefix of the sidecar's name (a different Unicode normalisation, say) the
/// sidecar's last extension is used.
///
/// The name can be longer than the owner's by the length of that tail; see the module docs for
/// keeping it within a cap.
pub fn sidecar_destination(
    naming: SidecarNaming,
    old_owner_name: &str,
    new_owner_name: &str,
    sidecar_name: &str,
) -> String {
    let (old_prefix, new_prefix) = match naming {
        SidecarNaming::Exact => (old_owner_name, new_owner_name),
        SidecarNaming::Stem => (split_name(old_owner_name).0, split_name(new_owner_name).0),
    };
    let tail = tail_after(sidecar_name, old_prefix).unwrap_or_else(|| extension_tail(sidecar_name));
    format!("{new_prefix}{tail}")
}

/// What follows `prefix` in `name` (ignoring ASCII case), when it starts with a dot.
fn tail_after<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    let head = name.get(..prefix.len())?;
    let tail = name.get(prefix.len()..)?;
    (head.eq_ignore_ascii_case(prefix) && tail.starts_with('.')).then_some(tail)
}

/// The last extension of `name` with its dot, or an empty string.
fn extension_tail(name: &str) -> &str {
    match split_name(name) {
        (stem, Some(_)) => name.get(stem.len()..).unwrap_or_default(),
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn info(size: u64, hash: Option<u8>) -> ContentInfo {
        ContentInfo {
            size,
            hash: hash.map(|byte| [byte; 20]),
        }
    }

    #[test]
    fn numbers_go_before_the_last_extension() {
        for (name, n, expected) in [
            ("IMG_0001.jpg", 1, "IMG_0001_1.jpg"),
            ("IMG_0001.jpg", 12, "IMG_0001_12.jpg"),
            ("IMG_0001.jpg", 10_000, "IMG_0001_10000.jpg"),
            ("IMG_0001.JPG", 2, "IMG_0001_2.JPG"),
            ("IMG_0001.CR3.xmp", 2, "IMG_0001.CR3_2.xmp"),
            ("archive.tar.gz", 1, "archive.tar_1.gz"),
            ("my.holiday.photo.jpg", 3, "my.holiday.photo_3.jpg"),
            ("README", 1, "README_1"),
            (".bashrc", 1, ".bashrc_1"),
            (".hidden.jpg", 1, ".hidden_1.jpg"),
            ("photo.", 1, "photo._1"),
            ("..jpg", 1, "._1.jpg"),
            ("été.JPG", 3, "été_3.JPG"),
            ("e\u{301}te\u{301}.jpg", 1, "e\u{301}te\u{301}_1.jpg"),
            ("日本語.png", 1, "日本語_1.png"),
            ("IMG_0001_1.jpg", 1, "IMG_0001_1_1.jpg"),
            ("a b (1).jpg", 1, "a b (1)_1.jpg"),
            ("", 1, "_1"),
            ("IMG.jpg", 0, "IMG.jpg"),
        ] {
            assert_eq!(numbered(name, n), expected, "{name:?} + {n}");
        }
    }

    #[test]
    fn long_names_are_shortened_on_a_character_boundary_keeping_the_extension() {
        let long = format!("{}.jpg", "a".repeat(300));
        let got = numbered(&long, 1);
        assert_eq!(got.len(), 255);
        assert_eq!(got, format!("{}_1.jpg", "a".repeat(249)));

        let accents = format!("{}.jpg", "é".repeat(200));
        assert_eq!(numbered(&accents, 1), format!("{}_1.jpg", "é".repeat(124)));

        assert_eq!(
            numbered_within(&long, 7, 143),
            format!("{}_7.jpg", "a".repeat(137))
        );
        assert_eq!(numbered_within("IMG_0001.jpg", 1, 143), "IMG_0001_1.jpg");

        let tiny = format!("{}.jpg", "a".repeat(40));
        assert_eq!(
            numbered_within(&tiny, 1, 0),
            format!("{}_1.jpg", "a".repeat(10))
        );
    }

    #[test]
    fn an_extension_too_long_to_fit_is_cut_but_the_number_survives() {
        let name = format!("a.{}", "x".repeat(300));
        let got = numbered(&name, 5);
        assert_eq!(got.len(), 255);
        assert!(got.starts_with("_5.xxx"), "{got}");
    }

    #[test]
    fn a_free_name_is_returned_as_it_is() {
        assert_eq!(
            unique("IMG_0001.jpg", |_| false).as_deref(),
            Ok("IMG_0001.jpg")
        );
        let long = "x".repeat(400);
        assert_eq!(unique(&long, |_| false), Ok(long));
    }

    #[test]
    fn the_first_free_number_is_used() {
        let taken: HashSet<&str> = [
            "IMG_0001.jpg",
            "IMG_0001_1.jpg",
            "IMG_0001_2.jpg",
            "IMG_0001_4.jpg",
        ]
        .into();
        assert_eq!(
            unique("IMG_0001.jpg", |name| taken.contains(name)).as_deref(),
            Ok("IMG_0001_3.jpg")
        );
        assert_eq!(
            unique("IMG_0002.jpg", |name| taken.contains(name)).as_deref(),
            Ok("IMG_0002.jpg")
        );
    }

    #[test]
    fn the_search_is_bounded() {
        let mut calls = 0u32;
        let result = unique("IMG_0001.jpg", |_| {
            calls += 1;
            true
        });
        assert_eq!(
            result,
            Err(ConflictError::NoFreeName {
                name: "IMG_0001.jpg".to_owned(),
                attempts: MAX_ATTEMPTS,
            })
        );
        assert_eq!(calls, MAX_ATTEMPTS + 1);
        assert_eq!(
            result.map_err(|e| e.to_string()),
            Err(r#"no free name for "IMG_0001.jpg" after 10000 attempts"#.to_owned())
        );
    }

    #[test]
    fn the_last_attempt_still_succeeds() {
        let last = numbered("a.jpg", MAX_ATTEMPTS);
        let got = unique("a.jpg", |name| name != last);
        assert_eq!(got, Ok(last));
    }

    #[test]
    fn numbered_candidates_respect_the_given_cap() {
        let long = format!("{}.jpg", "a".repeat(200));
        let seen = std::cell::RefCell::new(Vec::new());
        let got = unique_within(&long, 143, |name| {
            seen.borrow_mut().push(name.len());
            name == long
        });
        assert_eq!(got.map(|name| name.len()), Ok(143));
        assert_eq!(seen.into_inner(), vec![204, 143]);
    }

    #[test]
    fn case_insensitive_search_works_on_normalised_keys() {
        let taken: HashSet<String> = ["img_0001.jpg", "img_0001_1.jpg"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut keys = Vec::new();
        let got = unique_case_insensitive("IMG_0001.JPG", |key| {
            keys.push(key.to_owned());
            taken.contains(key)
        });
        assert_eq!(got.as_deref(), Ok("IMG_0001_2.JPG"));
        assert_eq!(keys, ["img_0001.jpg", "img_0001_1.jpg", "img_0001_2.jpg"]);

        assert_eq!(
            unique_case_insensitive("IMG_0009.JPG", |key| taken.contains(key)).as_deref(),
            Ok("IMG_0009.JPG")
        );
    }

    #[test]
    fn case_insensitive_search_ignores_unicode_normalisation_too() {
        let nfd_taken: HashSet<String> = [normalize_name("e\u{301}te\u{301}.jpg")].into();
        let got = unique_case_insensitive("\u{e9}t\u{e9}.JPG", |key| nfd_taken.contains(key));
        assert_eq!(got.as_deref(), Ok("\u{e9}t\u{e9}_1.JPG"));
    }

    #[test]
    fn case_insensitive_search_is_bounded_too() {
        assert_eq!(
            unique_case_insensitive("a.jpg", |_| true),
            Err(ConflictError::NoFreeName {
                name: "a.jpg".to_owned(),
                attempts: MAX_ATTEMPTS,
            })
        );
        let long = format!("{}.JPG", "A".repeat(200));
        let got = unique_case_insensitive_within(&long, 143, |key| key == normalize_name(&long));
        assert_eq!(got.map(|name| name.len()), Ok(143));
    }

    #[test]
    fn first_free_number_starts_at_one_and_is_bounded() {
        assert_eq!(first_free_number(|_| true), Some(1));
        assert_eq!(first_free_number(|n| n >= 7), Some(7));
        assert_eq!(first_free_number(|n| n == MAX_ATTEMPTS), Some(MAX_ATTEMPTS));
        assert_eq!(first_free_number(|_| false), None);
    }

    #[test]
    fn owners_sharing_a_stem_sidecar_can_be_numbered_in_lockstep() {
        let taken: HashSet<String> = ["IMG_0001_1.JPG", "IMG_0001_2.xmp"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let n = first_free_number(|n| {
            [
                numbered("IMG_0001.CR3", n),
                numbered("IMG_0001.JPG", n),
                sidecar_destination(
                    SidecarNaming::Stem,
                    "IMG_0001.CR3",
                    &numbered("IMG_0001.CR3", n),
                    "IMG_0001.xmp",
                ),
            ]
            .iter()
            .all(|name| !taken.contains(name))
        });
        assert_eq!(n, Some(3));
    }

    #[test]
    fn an_owner_is_numbered_together_with_its_sidecars() {
        let taken: HashSet<&str> = ["IMG_0001.CR3", "IMG_0001_1.CR3.xmp", "IMG_0001_2.CR3"].into();
        let got = unique("IMG_0001.CR3", |candidate| {
            let sidecar = sidecar_destination(
                SidecarNaming::Exact,
                "IMG_0001.CR3",
                candidate,
                "IMG_0001.CR3.xmp",
            );
            taken.contains(candidate) || taken.contains(sidecar.as_str())
        });
        assert_eq!(got.as_deref(), Ok("IMG_0001_3.CR3"));
    }

    #[test]
    fn nothing_at_the_destination_always_proceeds() {
        for policy in [
            ConflictPolicy::Smart,
            ConflictPolicy::Skip,
            ConflictPolicy::Rename,
            ConflictPolicy::Replace,
        ] {
            for incoming in [info(10, Some(1)), info(0, None)] {
                assert_eq!(
                    decide(policy, None, incoming),
                    ConflictDecision::Proceed,
                    "{policy:?}"
                );
            }
        }
    }

    #[test]
    fn smart_skips_only_provably_identical_content() {
        use ConflictDecision::{Rename, Skip};
        use SkipReason::Identical;
        for (existing, incoming, expected) in [
            (info(10, Some(1)), info(10, Some(1)), Skip(Identical)),
            (info(0, Some(9)), info(0, Some(9)), Skip(Identical)),
            (info(10, Some(1)), info(10, Some(2)), Rename),
            (info(10, Some(1)), info(11, Some(1)), Rename),
            (info(10, Some(1)), info(11, Some(2)), Rename),
            (info(10, None), info(10, Some(1)), Rename),
            (info(10, Some(1)), info(10, None), Rename),
            (info(10, None), info(10, None), Rename),
            (info(10, None), info(11, None), Rename),
        ] {
            assert_eq!(
                decide(ConflictPolicy::Smart, Some(existing), incoming),
                expected,
                "{existing:?} vs {incoming:?}"
            );
        }
    }

    #[test]
    fn explicit_policies_ignore_the_content() {
        for (existing, incoming) in [
            (info(10, Some(1)), info(10, Some(1))),
            (info(10, Some(1)), info(10, Some(2))),
            (info(10, None), info(20, None)),
        ] {
            assert_eq!(
                decide(ConflictPolicy::Skip, Some(existing), incoming),
                ConflictDecision::Skip(SkipReason::Policy)
            );
            assert_eq!(
                decide(ConflictPolicy::Rename, Some(existing), incoming),
                ConflictDecision::Rename
            );
            assert_eq!(
                decide(ConflictPolicy::Replace, Some(existing), incoming),
                ConflictDecision::ReplaceExisting
            );
        }
    }

    #[test]
    fn the_default_policy_is_smart() {
        let same = info(5, Some(3));
        assert_eq!(
            decide(ConflictPolicy::default(), Some(same), same),
            ConflictDecision::Skip(SkipReason::Identical)
        );
    }

    #[test]
    fn hashes_are_only_needed_for_a_same_size_smart_collision() {
        let incoming = info(10, Some(1));
        for (policy, existing, incoming, expected) in [
            (ConflictPolicy::Smart, Some(info(10, None)), incoming, true),
            (
                ConflictPolicy::Smart,
                Some(info(10, Some(1))),
                info(10, None),
                true,
            ),
            (
                ConflictPolicy::Smart,
                Some(info(10, None)),
                info(10, None),
                true,
            ),
            (
                ConflictPolicy::Smart,
                Some(info(10, Some(2))),
                incoming,
                false,
            ),
            (ConflictPolicy::Smart, Some(info(11, None)), incoming, false),
            (ConflictPolicy::Smart, None, info(10, None), false),
            (ConflictPolicy::Skip, Some(info(10, None)), incoming, false),
            (
                ConflictPolicy::Rename,
                Some(info(10, None)),
                incoming,
                false,
            ),
            (
                ConflictPolicy::Replace,
                Some(info(10, None)),
                incoming,
                false,
            ),
        ] {
            assert_eq!(
                needs_hashes(policy, existing, incoming),
                expected,
                "{policy:?} {existing:?} {incoming:?}"
            );
        }
    }

    #[test]
    fn sidecars_follow_the_new_name_of_an_exact_owner() {
        use SidecarNaming::Exact;
        for (old, new, sidecar, expected) in [
            (
                "IMG_0001.CR3",
                "IMG_0002.CR3",
                "IMG_0001.CR3.xmp",
                "IMG_0002.CR3.xmp",
            ),
            (
                "IMG_0001.CR3",
                "IMG_0001_1.CR3",
                "IMG_0001.CR3.xmp",
                "IMG_0001_1.CR3.xmp",
            ),
            (
                "photo.jpg",
                "2026-10-photo.jpg",
                "photo.jpg.json",
                "2026-10-photo.jpg.json",
            ),
            (
                "IMG_0001.CR3",
                "IMG_0002.CR3",
                "img_0001.cr3.XMP",
                "IMG_0002.CR3.XMP",
            ),
            (
                "IMG_0001.CR3",
                "IMG_0001.dng",
                "IMG_0001.CR3.pp3",
                "IMG_0001.dng.pp3",
            ),
            ("README", "README_1", "README.xmp", "README_1.xmp"),
            (".hidden", ".hidden_1", ".hidden.xmp", ".hidden_1.xmp"),
            (
                "archive.tar.gz",
                "archive.tar_1.gz",
                "archive.tar.gz.json",
                "archive.tar_1.gz.json",
            ),
            (
                "IMG_0001.CR3",
                "IMG_0002.CR3",
                "IMG_0001.CR3.supplemental-metadata.json",
                "IMG_0002.CR3.supplemental-metadata.json",
            ),
        ] {
            assert_eq!(
                sidecar_destination(Exact, old, new, sidecar),
                expected,
                "{sidecar:?} of {old:?} -> {new:?}"
            );
        }
    }

    #[test]
    fn sidecars_follow_the_new_stem_of_a_stem_owner() {
        use SidecarNaming::Stem;
        for (old, new, sidecar, expected) in [
            (
                "IMG_0001.CR3",
                "IMG_0001_1.CR3",
                "IMG_0001.xmp",
                "IMG_0001_1.xmp",
            ),
            (
                "IMG_0001.JPG",
                "IMG_0001_1.JPG",
                "IMG_0001.xmp",
                "IMG_0001_1.xmp",
            ),
            (
                "IMG_0001.CR3",
                "2026-IMG_0001.CR3",
                "img_0001.XMP",
                "2026-IMG_0001.XMP",
            ),
            (
                "IMG_0001.CR3",
                "IMG_0001.dng",
                "IMG_0001.xmp",
                "IMG_0001.xmp",
            ),
            (
                "archive.tar.gz",
                "archive.tar_1.gz",
                "archive.tar.xmp",
                "archive.tar_1.xmp",
            ),
            ("IMG", "IMG_1", "IMG.xmp", "IMG_1.xmp"),
            (".bashrc", ".bashrc_1", ".bashrc.xmp", ".bashrc_1.xmp"),
        ] {
            assert_eq!(
                sidecar_destination(Stem, old, new, sidecar),
                expected,
                "{sidecar:?} of {old:?} -> {new:?}"
            );
        }
    }

    #[test]
    fn sidecars_whose_prefix_differs_fall_back_to_their_last_extension() {
        let nfc = "\u{e9}t\u{e9}.jpg";
        let nfd_sidecar = "e\u{301}te\u{301}.jpg.xmp";
        assert_eq!(
            sidecar_destination(SidecarNaming::Exact, nfc, "ete_1.jpg", nfd_sidecar),
            "ete_1.jpg.xmp"
        );
        assert_eq!(
            sidecar_destination(
                SidecarNaming::Stem,
                nfc,
                "ete_1.jpg",
                "e\u{301}te\u{301}.xmp"
            ),
            "ete_1.xmp"
        );
        assert_eq!(
            sidecar_destination(
                SidecarNaming::Stem,
                "IMG_0001.CR3",
                "IMG_0002.CR3",
                "other.xmp"
            ),
            "IMG_0002.xmp"
        );
    }

    proptest! {
        #[test]
        fn numbered_names_fit_the_cap_and_carry_their_number(
            name in "\\PC{0,300}",
            n in 1u32..=MAX_ATTEMPTS,
            cap in 0usize..300,
        ) {
            let got = numbered_within(&name, n, cap);
            let suffix = format!("_{n}");
            prop_assert!(got.len() <= cap.max(MIN_NAME_BYTES));
            prop_assert!(got.contains(&suffix));
        }

        #[test]
        fn short_names_keep_their_stem_and_extension(
            stem in "[A-Za-z0-9 _-]{1,40}",
            ext in "[A-Za-z0-9]{1,6}",
            n in 1u32..=MAX_ATTEMPTS,
        ) {
            let got = numbered(&format!("{stem}.{ext}"), n);
            prop_assert_eq!(got, format!("{stem}_{n}.{ext}"));
        }

        #[test]
        fn unique_returns_the_first_name_that_is_not_taken(
            name in "\\PC{1,40}",
            taken_count in 0u32..40,
        ) {
            let mut taken: HashSet<String> =
                (1..=taken_count).map(|n| numbered(&name, n)).collect();
            taken.insert(name.clone());
            let got = unique(&name, |candidate| taken.contains(candidate))?;
            prop_assert!(!taken.contains(&got));
            prop_assert_eq!(got, numbered(&name, taken_count + 1));
        }

        #[test]
        fn case_insensitive_unique_never_collides_by_key(
            name in "\\PC{1,40}",
            taken_count in 0u32..40,
        ) {
            let mut keys: HashSet<String> = (1..=taken_count)
                .map(|n| normalize_name(&numbered(&name, n)))
                .collect();
            keys.insert(normalize_name(&name));
            let got = unique_case_insensitive(&name, |key| keys.contains(key))?;
            prop_assert!(!keys.contains(&normalize_name(&got)));
        }

        #[test]
        fn unique_terminates_when_everything_is_taken(name in "\\PC{0,20}") {
            let result = unique(&name, |_| true);
            let is_exhausted = matches!(result, Err(ConflictError::NoFreeName { .. }));
            prop_assert!(is_exhausted);
        }

        #[test]
        fn sidecar_destinations_never_panic(
            owner in "\\PC{0,20}",
            new_owner in "\\PC{0,20}",
            sidecar in "\\PC{0,30}",
            exact in any::<bool>(),
        ) {
            let naming = if exact { SidecarNaming::Exact } else { SidecarNaming::Stem };
            let _ = sidecar_destination(naming, &owner, &new_owner, &sidecar);
        }

        #[test]
        fn a_linked_sidecar_keeps_its_relationship_after_the_rename(
            stem in "[A-Za-z0-9_]{1,20}",
            ext in "[A-Za-z0-9]{1,5}",
            sidecar_ext in "[a-z]{2,4}",
            n in 1u32..=MAX_ATTEMPTS,
        ) {
            let owner = format!("{stem}.{ext}");
            let renamed = numbered(&owner, n);
            let exact = sidecar_destination(
                SidecarNaming::Exact,
                &owner,
                &renamed,
                &format!("{owner}.{sidecar_ext}"),
            );
            prop_assert_eq!(exact, format!("{renamed}.{sidecar_ext}"));
            let shared = sidecar_destination(
                SidecarNaming::Stem,
                &owner,
                &renamed,
                &format!("{stem}.{sidecar_ext}"),
            );
            prop_assert_eq!(shared, format!("{stem}_{n}.{sidecar_ext}"));
        }
    }
}
