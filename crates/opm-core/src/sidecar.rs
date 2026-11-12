//! Sidecar rules: which small companion files travel with a media file (plan D3, assumptions A17
//! and A18). Everything here is a pure function over file NAMES: the scanner feeds it a directory
//! listing, move and delete feed it the stored links.
//!
//! # What is a sidecar
//! A file is a sidecar when its extension (the part after the last dot) is in the configured list
//! and is not a media extension. Media wins: a list that contains `jpg` does not turn photos into
//! sidecars, such entries are accepted but inert. The defaults are
//! [`DEFAULT_SIDECAR_EXTENSIONS`] (`xmp json aae dop pp3 xml`).
//! [`SidecarMatcher::file_kind`] is the classification the scanner stores in `files.kind`.
//! Settings are validated with [`normalise_extensions`] (lowercase, no dot, `[a-z0-9]{1,10}`, at
//! most 50) before they are stored; [`SidecarMatcher::new`] never fails and ignores whatever it
//! cannot use.
//!
//! # Links
//! [`SidecarMatcher::link`] links the sidecars of ONE directory to their owners. Names are compared
//! case-insensitively and Unicode-normalisation-insensitively
//! ([`names::normalize_name`](crate::names::normalize_name)). Let `key` be the sidecar name without
//! its last extension:
//! - [`LinkKind::Exact`]: `key` is the owner's full name. `IMG_0001.CR3.xmp` belongs to
//!   `IMG_0001.CR3`, Google Takeout's `photo.jpg.json` to `photo.jpg`.
//! - [`LinkKind::Stem`]: `key` is the owner's stem. `IMG_0001.xmp` belongs to `IMG_0001.CR3` AND
//!   `IMG_0001.JPG`: one sidecar can have several owners, and one owner several sidecars.
//!
//! Rules:
//! - A name that merely STARTS with a stem never links: `IMG_0004.xmp` is not a sidecar of
//!   `IMG_00041.jpg`.
//! - A pair gets at most one link and Exact wins (the two only coincide for an owner without an
//!   extension). A sidecar holding an Exact link to one owner and a Stem link to another keeps
//!   both.
//! - Any file that is not itself a sidecar can be an owner, media or not (a RAW format this app
//!   does not know still keeps its `.xmp`: leaving a sidecar behind is worse than being careful
//!   with it). A sidecar never owns another sidecar.
//! - A sidecar without an owner is an *orphan*: for moving and deleting it is an ordinary file.
//! - Callers leave junk and partial files ([`junk`](crate::junk)) out of the listing, they could
//!   otherwise be linked like any other name.
//! - Linking costs O(n) hash lookups for n names, plus the links it produces.
//!
//! ```
//! use opm_core::sidecar::{Link, LinkKind, SidecarMatcher};
//!
//! let names = ["IMG_0001.CR3", "IMG_0001.JPG", "IMG_0001.xmp", "IMG_0001.CR3.xmp"];
//! let links = SidecarMatcher::default().link(&names);
//! assert_eq!(
//!     links,
//!     [
//!         Link { sidecar: 2, owner: 0, kind: LinkKind::Stem },
//!         Link { sidecar: 2, owner: 1, kind: LinkKind::Stem },
//!         Link { sidecar: 3, owner: 0, kind: LinkKind::Exact },
//!     ]
//! );
//! ```
//!
//! # Moving and deleting (A18)
//! [`split_follow`] decides, for the owners an operation touches, which sidecars go with them:
//! those whose owners are ALL part of the operation follow, a sidecar that is also linked to an
//! owner staying behind is *shared*. On a move the shared sidecar is COPIED next to the moved
//! owners and the original stays; on a delete it is not trashed.
//! [`SidecarMatcher::sidecar_names_for`] lists the names an owner's sidecars would have, for
//! lookups that cannot afford a directory listing.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::str::FromStr;

use crate::consts::DEFAULT_SIDECAR_EXTENSIONS;
use crate::dto::FileKind;
use crate::kind::{classify, classify_name};
use crate::names::{normalize_name, split_name};

/// Most extensions a settings list may hold.
pub const MAX_EXTENSIONS: usize = 50;

/// Longest sidecar extension, in characters.
pub const MAX_EXTENSION_LEN: usize = 10;

/// Longest rejected entry echoed back in an error message.
const ECHO_CHARS: usize = 32;

/// Why a sidecar extension list is rejected by [`normalise_extensions`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SidecarError {
    #[error("at most {max} sidecar extensions are allowed")]
    TooMany { max: usize },
    #[error("sidecar extension {entry:?} is not valid: use 1 to {max} letters or digits")]
    Invalid { entry: String, max: usize },
}

enum Cleaned {
    Blank,
    Valid(String),
    Invalid,
}

/// Trims one entry, strips ONE leading dot and lowercases it.
fn clean(raw: &str) -> Cleaned {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Cleaned::Blank;
    }
    let ext = trimmed.strip_prefix('.').unwrap_or(trimmed);
    let ext = ext.to_ascii_lowercase();
    let well_formed = (1..=MAX_EXTENSION_LEN).contains(&ext.len())
        && ext
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if well_formed {
        Cleaned::Valid(ext)
    } else {
        Cleaned::Invalid
    }
}

/// Validates and normalises the `sidecar_extensions` setting: entries are trimmed, a leading dot is
/// dropped, case is folded to lowercase, blank entries and duplicates are removed and the user's
/// order is kept. An empty list is valid (nothing is a sidecar then). Media extensions (`jpg`) are
/// accepted but have no effect, see the module docs.
///
/// Rejects entries that are not 1 to [`MAX_EXTENSION_LEN`] ASCII letters or digits (so no inner dot
/// and no `/`) and lists of more than [`MAX_EXTENSIONS`] distinct extensions.
pub fn normalise_extensions<S: AsRef<str>>(extensions: &[S]) -> Result<Vec<String>, SidecarError> {
    let mut kept: Vec<String> = Vec::new();
    for raw in extensions {
        match clean(raw.as_ref()) {
            Cleaned::Blank => {}
            Cleaned::Invalid => {
                let entry = raw.as_ref().trim().chars().take(ECHO_CHARS).collect();
                return Err(SidecarError::Invalid {
                    entry,
                    max: MAX_EXTENSION_LEN,
                });
            }
            Cleaned::Valid(ext) => {
                if kept.contains(&ext) {
                    continue;
                }
                if kept.len() >= MAX_EXTENSIONS {
                    return Err(SidecarError::TooMany {
                        max: MAX_EXTENSIONS,
                    });
                }
                kept.push(ext);
            }
        }
    }
    Ok(kept)
}

/// How a sidecar is tied to its owner. The strings are the `file_sidecars.kind` column values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LinkKind {
    /// The sidecar is the owner's full name plus the sidecar extension (`IMG_0001.CR3.xmp`).
    Exact,
    /// The sidecar is the owner's stem plus the sidecar extension (`IMG_0001.xmp`).
    Stem,
}

impl LinkKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Stem => "stem",
        }
    }
}

impl fmt::Display for LinkKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string is not a `file_sidecars.kind` value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown sidecar link kind `{0}`")]
pub struct ParseLinkKindError(pub String);

impl FromStr for LinkKind {
    type Err = ParseLinkKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "exact" => Ok(Self::Exact),
            "stem" => Ok(Self::Stem),
            other => Err(ParseLinkKindError(other.to_owned())),
        }
    }
}

/// One sidecar-to-owner link. `sidecar` and `owner` are indexes into the name list given to
/// [`SidecarMatcher::link`]. The derived order is the order `link` returns: by sidecar, then owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Link {
    pub sidecar: usize,
    pub owner: usize,
    pub kind: LinkKind,
}

/// The result of [`SidecarMatcher::linking`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Linking {
    /// Every link, sorted.
    pub links: Vec<Link>,
    /// Indexes of the sidecars that have no owner, in ascending order.
    pub orphans: Vec<usize>,
}

/// A name an owner's sidecar would have, see [`SidecarMatcher::sidecar_names_for`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Candidate {
    pub name: String,
    pub kind: LinkKind,
}

/// The configured sidecar extensions and the rules built on them. Cheap to build and clone; build
/// one from the settings per task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarMatcher {
    extensions: Vec<String>,
}

impl Default for SidecarMatcher {
    /// The built-in extension list, [`DEFAULT_SIDECAR_EXTENSIONS`].
    fn default() -> Self {
        Self {
            extensions: DEFAULT_SIDECAR_EXTENSIONS
                .iter()
                .map(|ext| (*ext).to_owned())
                .collect(),
        }
    }
}

impl SidecarMatcher {
    /// Builds a matcher from a settings list. Entries are normalised like
    /// [`normalise_extensions`] does, but entries that are not valid are ignored instead of
    /// rejected, and so is any length limit: a damaged setting must not stop a scan. An empty
    /// list means that nothing is a sidecar.
    pub fn new<S: AsRef<str>>(extensions: &[S]) -> Self {
        let mut seen = HashSet::new();
        let extensions = extensions
            .iter()
            .filter_map(|raw| match clean(raw.as_ref()) {
                Cleaned::Valid(ext) => Some(ext),
                Cleaned::Blank | Cleaned::Invalid => None,
            })
            .filter(|ext| seen.insert(ext.clone()))
            .collect();
        Self { extensions }
    }

    /// The configured extensions: lowercase, without dot, in the user's order. Includes the inert
    /// media entries.
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// The configured extensions that really make a file a sidecar (media ones left out).
    fn effective(&self) -> impl Iterator<Item = &str> {
        self.extensions
            .iter()
            .map(String::as_str)
            .filter(|ext| self.is_sidecar_ext(ext))
    }

    /// Whether `ext` (without the dot, any ASCII case) makes a file a sidecar.
    pub fn is_sidecar_ext(&self, ext: &str) -> bool {
        classify(ext, &self.extensions) == FileKind::Sidecar
    }

    /// Whether the file `name` is a sidecar: its extension is configured and is not a media
    /// extension. `.xmp` alone (a dotfile) and `xmp` have no extension.
    pub fn is_sidecar_name(&self, name: &str) -> bool {
        self.file_kind(name) == FileKind::Sidecar
    }

    /// The `files.kind` of a file name under this configuration (see [`crate::kind`]).
    pub fn file_kind(&self, name: &str) -> FileKind {
        classify_name(name, &self.extensions)
    }

    /// Links the sidecars among `names` (the file names of ONE directory) to their owners, see
    /// the module docs. Indexes refer to `names`; the result is sorted by sidecar, then owner.
    pub fn link<S: AsRef<str>>(&self, names: &[S]) -> Vec<Link> {
        self.linking(names).links
    }

    /// [`link`](Self::link) plus the orphan sidecars.
    pub fn linking<S: AsRef<str>>(&self, names: &[S]) -> Linking {
        let mut sidecars: Vec<(usize, String)> = Vec::new();
        let mut owners: Vec<(usize, &str)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let name = name.as_ref();
            if self.is_sidecar_name(name) {
                sidecars.push((index, normalize_name(split_name(name).0)));
            } else {
                owners.push((index, name));
            }
        }
        if sidecars.is_empty() {
            return Linking::default();
        }

        // Owner indexes are pushed in ascending order, so every list is sorted.
        let mut by_full: HashMap<String, Vec<usize>> = HashMap::with_capacity(owners.len());
        let mut by_stem: HashMap<String, Vec<usize>> = HashMap::with_capacity(owners.len());
        for &(index, name) in &owners {
            by_full.entry(normalize_name(name)).or_default().push(index);
            let stem = normalize_name(split_name(name).0);
            by_stem.entry(stem).or_default().push(index);
        }

        let mut links = Vec::new();
        let mut orphans = Vec::new();
        for (sidecar, key) in sidecars {
            let exact = by_full.get(&key).map(Vec::as_slice).unwrap_or_default();
            let stem = by_stem.get(&key).map(Vec::as_slice).unwrap_or_default();
            let before = links.len();
            links.extend(exact.iter().map(|&owner| Link {
                sidecar,
                owner,
                kind: LinkKind::Exact,
            }));
            links.extend(
                stem.iter()
                    .filter(|owner| exact.binary_search(owner).is_err())
                    .map(|&owner| Link {
                        sidecar,
                        owner,
                        kind: LinkKind::Stem,
                    }),
            );
            if links.len() == before {
                orphans.push(sidecar);
            }
        }
        links.sort_unstable();
        Linking { links, orphans }
    }

    /// The names the sidecars of `owner_name` (a file name, not a path) would have: first the
    /// [`Exact`](LinkKind::Exact) ones (`IMG_0001.CR3.xmp`), then the [`Stem`](LinkKind::Stem) ones
    /// (`IMG_0001.xmp`), each group in the order of the configured extensions. The spelling
    /// follows the owner and the extension is lowercase; on a case-sensitive disk a real sidecar
    /// may be spelled differently, so link a directory listing when the answer must be exact.
    ///
    /// Empty for a sidecar (it has no sidecars). An owner without an extension has no Stem
    /// candidates, they would repeat the Exact ones.
    pub fn sidecar_names_for(&self, owner_name: &str) -> Vec<Candidate> {
        if owner_name.is_empty() || self.is_sidecar_name(owner_name) {
            return Vec::new();
        }
        let (stem, ext) = split_name(owner_name);
        let mut candidates: Vec<Candidate> = self
            .effective()
            .map(|ext| Candidate {
                name: format!("{owner_name}.{ext}"),
                kind: LinkKind::Exact,
            })
            .collect();
        if ext.is_some() {
            candidates.extend(self.effective().map(|ext| Candidate {
                name: format!("{stem}.{ext}"),
                kind: LinkKind::Stem,
            }));
        }
        candidates
    }
}

/// The sidecars of an operation, sorted by what happens to them. See [`split_follow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowSplit<S> {
    /// Every owner of these sidecars is part of the operation: they are moved or trashed along.
    pub follow: Vec<S>,
    /// These are also linked to an owner that stays behind: a move COPIES them, a delete leaves
    /// them alone.
    pub shared: Vec<S>,
}

/// Decides which sidecars go along with the owners an operation touches (A18). `links` holds ALL
/// the `(sidecar, owner)` pairs of the sidecars that touch the selection (a sidecar's owners that
/// are not selected must be in it too, that is how a shared sidecar is recognised) and
/// `is_selected` says whether an owner is part of the operation.
///
/// - `follow`: the sidecar has at least one selected owner and no other owner.
/// - `shared`: the sidecar has a selected owner and an owner that is not selected.
/// - A sidecar without a selected owner is in neither list.
///
/// The link kind does not matter, only who the owners are. Both lists keep the order in which the
/// sidecars first appear in `links` and hold each sidecar once. A sidecar that is itself part of
/// the selection must be handled as the plain file it is: leave it out of `links` or filter it
/// afterwards.
pub fn split_follow<S, O>(
    links: impl IntoIterator<Item = (S, O)>,
    is_selected: impl Fn(&O) -> bool,
) -> FollowSplit<S>
where
    S: Eq + Hash + Clone,
{
    #[derive(Default)]
    struct Owners {
        selected: bool,
        staying: bool,
    }

    let mut order: Vec<S> = Vec::new();
    let mut owners: HashMap<S, Owners> = HashMap::new();
    for (sidecar, owner) in links {
        let selected = is_selected(&owner);
        let seen = owners.entry(sidecar.clone()).or_insert_with(|| {
            order.push(sidecar);
            Owners::default()
        });
        if selected {
            seen.selected = true;
        } else {
            seen.staying = true;
        }
    }

    let mut split = FollowSplit {
        follow: Vec::new(),
        shared: Vec::new(),
    };
    for sidecar in order {
        match owners.get(&sidecar) {
            Some(Owners {
                selected: true,
                staying: false,
            }) => split.follow.push(sidecar),
            Some(Owners {
                selected: true,
                staying: true,
            }) => split.shared.push(sidecar),
            _ => {}
        }
    }
    split
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};
    use std::time::{Duration, Instant};

    use proptest::prelude::*;

    use super::*;

    const NFC_E: &str = "\u{e9}";
    const NFD_E: &str = "e\u{301}";

    fn link(sidecar: usize, owner: usize, kind: LinkKind) -> Link {
        Link {
            sidecar,
            owner,
            kind,
        }
    }

    fn defaults(names: &[&str]) -> Vec<Link> {
        SidecarMatcher::default().link(names)
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    // ---- extension lists ----

    #[test]
    fn extensions_are_trimmed_lowercased_deduplicated_and_keep_their_order() {
        let list = [" XMP ", ".json", "xmp", "Aae", "", "  ", "JSON"];
        assert_eq!(
            normalise_extensions(&list).unwrap(),
            strings(&["xmp", "json", "aae"])
        );
    }

    #[test]
    fn only_one_leading_dot_is_dropped() {
        assert_eq!(normalise_extensions(&[".xmp"]).unwrap(), strings(&["xmp"]));
        for bad in ["..xmp", ".", "...", "tar.gz", ".tar.gz"] {
            assert!(
                matches!(
                    normalise_extensions(&[bad]),
                    Err(SidecarError::Invalid { .. })
                ),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn invalid_extensions_are_rejected_with_the_offending_entry() {
        for bad in ["x y", "a/b", "\u{e9}t\u{e9}", "xm-p", "x\0", "caf\u{e9}"] {
            let error = normalise_extensions(&["xmp", bad]).unwrap_err();
            assert_eq!(
                error,
                SidecarError::Invalid {
                    entry: bad.to_owned(),
                    max: MAX_EXTENSION_LEN
                }
            );
        }
        let longest = "a".repeat(MAX_EXTENSION_LEN);
        assert_eq!(
            normalise_extensions(&[longest.as_str()]).unwrap(),
            vec![longest.clone()]
        );
        let too_long = "a".repeat(MAX_EXTENSION_LEN + 1);
        assert!(normalise_extensions(&[too_long.as_str()]).is_err());
    }

    #[test]
    fn an_enormous_bad_entry_is_not_echoed_in_full() {
        let huge = "x ".repeat(10_000);
        let Err(SidecarError::Invalid { entry, .. }) = normalise_extensions(&[huge.as_str()])
        else {
            panic!("must be rejected");
        };
        assert_eq!(entry.chars().count(), ECHO_CHARS);
    }

    #[test]
    fn an_empty_list_is_valid() {
        let none: [&str; 0] = [];
        assert_eq!(normalise_extensions(&none).unwrap(), Vec::<String>::new());
        assert_eq!(
            normalise_extensions(&["", " "]).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_list_holds_at_most_fifty_distinct_extensions() {
        let fifty: Vec<String> = (0..MAX_EXTENSIONS).map(|i| format!("e{i}")).collect();
        assert_eq!(normalise_extensions(&fifty).unwrap(), fifty);

        let mut fifty_one = fifty.clone();
        fifty_one.push("extra".to_owned());
        assert_eq!(
            normalise_extensions(&fifty_one),
            Err(SidecarError::TooMany {
                max: MAX_EXTENSIONS
            })
        );

        // Duplicates and blanks do not count.
        let mut padded = fifty.clone();
        padded.extend(fifty.iter().map(|ext| ext.to_uppercase()));
        padded.push(String::new());
        assert_eq!(normalise_extensions(&padded).unwrap(), fifty);
    }

    #[test]
    fn media_extensions_are_accepted_in_the_list() {
        assert_eq!(
            normalise_extensions(&["jpg", "xmp"]).unwrap(),
            strings(&["jpg", "xmp"])
        );
    }

    #[test]
    fn the_defaults_are_already_normal() {
        assert_eq!(
            normalise_extensions(DEFAULT_SIDECAR_EXTENSIONS).unwrap(),
            strings(DEFAULT_SIDECAR_EXTENSIONS)
        );
    }

    #[test]
    fn error_messages_are_readable() {
        assert_eq!(
            SidecarError::TooMany { max: 50 }.to_string(),
            "at most 50 sidecar extensions are allowed"
        );
        assert_eq!(
            SidecarError::Invalid {
                entry: "x y".to_owned(),
                max: 10
            }
            .to_string(),
            "sidecar extension \"x y\" is not valid: use 1 to 10 letters or digits"
        );
    }

    // ---- the matcher ----

    #[test]
    fn the_default_matcher_uses_the_default_extensions() {
        let matcher = SidecarMatcher::default();
        assert_eq!(matcher.extensions(), strings(DEFAULT_SIDECAR_EXTENSIONS));
    }

    #[test]
    fn construction_ignores_what_it_cannot_use() {
        let matcher =
            SidecarMatcher::new(&[" .XMP", "bad ext", "", "xmp", "x/y", "json", "caf\u{e9}"]);
        assert_eq!(matcher.extensions(), strings(&["xmp", "json"]));
        let many: Vec<String> = (0..80).map(|i| format!("e{i}")).collect();
        assert_eq!(SidecarMatcher::new(&many).extensions().len(), 80);
    }

    #[test]
    fn sidecars_are_recognised_by_their_last_extension() {
        let matcher = SidecarMatcher::default();
        for name in [
            "IMG_0001.xmp",
            "IMG_0001.XMP",
            "IMG_0001.CR3.xmp",
            "photo.jpg.json",
            "IMG_E0001.AAE",
            "a.b.c.pp3",
            "x.dop",
            "C0001M01.XML",
            "._IMG_0001.xmp",
        ] {
            assert!(matcher.is_sidecar_name(name), "{name} is a sidecar");
            assert_eq!(matcher.file_kind(name), FileKind::Sidecar);
        }
        for name in [
            "IMG_0001.jpg",
            "IMG_0001.CR3",
            "movie.mp4",
            "xmp",
            ".xmp",
            "notes.txt",
            "IMG_0001.xmp.bak",
            "IMG_0001.xmp.",
            "",
        ] {
            assert!(!matcher.is_sidecar_name(name), "{name} is not a sidecar");
        }
    }

    #[test]
    fn media_always_wins_over_the_extension_list() {
        let matcher = SidecarMatcher::new(&["xmp", "jpg", "mp4"]);
        assert_eq!(matcher.extensions(), strings(&["xmp", "jpg", "mp4"]));
        assert!(matcher.is_sidecar_name("a.xmp"));
        assert!(!matcher.is_sidecar_name("a.jpg"));
        assert!(!matcher.is_sidecar_name("a.MP4"));
        assert!(matcher.is_sidecar_ext("xmp"));
        assert!(!matcher.is_sidecar_ext("jpg"));
        assert_eq!(matcher.file_kind("a.jpg"), FileKind::Image);
        assert_eq!(matcher.file_kind("a.mp4"), FileKind::Video);
        // The inert entries do not produce candidates or links either.
        assert_eq!(matcher.link(&["a.jpg", "a.jpg.jpg", "a.mp4"]), vec![]);
        let candidates = matcher.sidecar_names_for("a.dng");
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn the_list_decides_what_else_is_a_sidecar() {
        let matcher = SidecarMatcher::new(&["thm", "TXT"]);
        assert!(matcher.is_sidecar_name("clip.THM"));
        assert!(matcher.is_sidecar_name("notes.txt"));
        assert!(!matcher.is_sidecar_name("a.xmp"));
        assert_eq!(matcher.file_kind("a.xmp"), FileKind::Other);
    }

    #[test]
    fn an_empty_list_has_no_sidecars() {
        let matcher = SidecarMatcher::new::<&str>(&[]);
        assert!(!matcher.is_sidecar_name("a.xmp"));
        assert_eq!(matcher.link(&["a.jpg", "a.xmp"]), vec![]);
        assert_eq!(matcher.linking(&["a.jpg", "a.xmp"]), Linking::default());
        assert_eq!(matcher.sidecar_names_for("a.jpg"), vec![]);
    }

    // ---- links ----

    #[test]
    fn exact_links_use_the_full_owner_name() {
        assert_eq!(
            defaults(&["IMG_0001.CR3", "IMG_0001.CR3.xmp"]),
            vec![link(1, 0, LinkKind::Exact)]
        );
        // Google Takeout.
        assert_eq!(
            defaults(&["photo.jpg", "photo.jpg.json", "other.jpg"]),
            vec![link(1, 0, LinkKind::Exact)]
        );
    }

    #[test]
    fn stem_links_use_the_owner_stem() {
        assert_eq!(
            defaults(&["IMG_0001.CR3", "IMG_0001.xmp"]),
            vec![link(1, 0, LinkKind::Stem)]
        );
    }

    #[test]
    fn a_stem_sidecar_has_every_owner_that_shares_the_stem() {
        let names = [
            "IMG_0001.CR3",
            "IMG_0001.JPG",
            "IMG_0001.xmp",
            "IMG_0002.JPG",
        ];
        assert_eq!(
            defaults(&names),
            vec![link(2, 0, LinkKind::Stem), link(2, 1, LinkKind::Stem)]
        );
    }

    #[test]
    fn an_owner_can_have_several_sidecars() {
        let names = ["a.CR3", "a.xmp", "a.CR3.xmp", "a.dop", "a.CR3.pp3", "a.aae"];
        assert_eq!(
            defaults(&names),
            vec![
                link(1, 0, LinkKind::Stem),
                link(2, 0, LinkKind::Exact),
                link(3, 0, LinkKind::Stem),
                link(4, 0, LinkKind::Exact),
                link(5, 0, LinkKind::Stem),
            ]
        );
    }

    #[test]
    fn names_are_compared_ignoring_case() {
        let names = ["IMG_0001.CR3", "img_0001.cr3.XMP", "Img_0001.Xmp"];
        assert_eq!(
            defaults(&names),
            vec![link(1, 0, LinkKind::Exact), link(2, 0, LinkKind::Stem)]
        );
    }

    #[test]
    fn names_are_compared_ignoring_the_unicode_form() {
        let nfc_owner = format!("caf{NFC_E}.jpg");
        let nfd_sidecar = format!("caf{NFD_E}.XMP");
        let nfd_exact = format!("CAF{NFD_E}.JPG.json");
        let names = [nfc_owner.as_str(), nfd_sidecar.as_str(), nfd_exact.as_str()];
        assert_eq!(
            defaults(&names),
            vec![link(1, 0, LinkKind::Stem), link(2, 0, LinkKind::Exact)]
        );
    }

    #[test]
    fn multi_dot_names_split_at_the_last_dot_only() {
        let names = [
            "trip.2023.07.jpg",
            "trip.2023.07.xmp",
            "trip.2023.07.jpg.xmp",
            "trip.2023.xmp",
            "trip.xmp",
            "trip.2023.07.json",
        ];
        assert_eq!(
            defaults(&names),
            vec![
                link(1, 0, LinkKind::Stem),
                link(2, 0, LinkKind::Exact),
                link(5, 0, LinkKind::Stem),
            ]
        );
    }

    #[test]
    fn a_name_that_only_starts_with_a_stem_never_links() {
        let names = [
            "IMG_00041.jpg",
            "IMG_0004.xmp",
            "IMG_0004.jpg.xmp",
            "IMG_00041.jpg.x",
        ];
        assert_eq!(defaults(&names), vec![]);
        let names = [
            "IMG_0004.jpg",
            "IMG_0004_edit.xmp",
            "IMG_0004 (1).xmp",
            "IMG_0004-1.json",
        ];
        assert_eq!(defaults(&names), vec![]);
    }

    #[test]
    fn orphans_are_reported_in_order() {
        let names = ["b.xmp", "a.jpg", "a.xmp", "c.jpg.json", "d.json", "e.txt"];
        let linking = SidecarMatcher::default().linking(&names);
        assert_eq!(linking.links, vec![link(2, 1, LinkKind::Stem)]);
        assert_eq!(linking.orphans, vec![0, 3, 4]);
    }

    #[test]
    fn a_sidecar_never_owns_a_sidecar() {
        let names = ["a.xmp", "a.xmp.json", "a.json", "a.json.xmp"];
        let linking = SidecarMatcher::default().linking(&names);
        assert_eq!(linking.links, vec![]);
        assert_eq!(linking.orphans, vec![0, 1, 2, 3]);
        // The same names linked to a real owner.
        let names = ["a.xmp", "a.xmp.json", "a.xmp.jpg"];
        assert_eq!(defaults(&names), vec![link(1, 2, LinkKind::Stem)]);
    }

    #[test]
    fn exact_wins_for_the_same_pair() {
        // For an owner without an extension the full name is also the stem.
        assert_eq!(
            defaults(&["README", "README.xmp"]),
            vec![link(1, 0, LinkKind::Exact)]
        );
    }

    #[test]
    fn both_kinds_are_kept_when_the_owners_differ() {
        let names = ["a.CR3", "a.CR3.jpg", "a.CR3.xmp"];
        assert_eq!(
            defaults(&names),
            vec![link(2, 0, LinkKind::Exact), link(2, 1, LinkKind::Stem)]
        );
    }

    #[test]
    fn any_file_that_is_not_a_sidecar_can_be_an_owner() {
        let names = [
            "IMG.XYZ",
            "IMG.xmp",
            "notes.txt",
            "notes.json",
            "README",
            "README.xmp",
        ];
        assert_eq!(
            defaults(&names),
            vec![
                link(1, 0, LinkKind::Stem),
                link(3, 2, LinkKind::Stem),
                link(5, 4, LinkKind::Exact),
            ]
        );
    }

    #[test]
    fn changing_the_extension_list_changes_the_links() {
        let names = ["a.jpg", "a.xmp", "a.thm", "a.jpg.thm"];
        // `.thm` is not a sidecar by default: `a.thm` is an ordinary file that owns `a.xmp` too.
        assert_eq!(
            defaults(&names),
            vec![link(1, 0, LinkKind::Stem), link(1, 2, LinkKind::Stem)]
        );
        let with_thm = SidecarMatcher::new(&["xmp", "thm"]);
        assert_eq!(
            with_thm.link(&names),
            vec![
                link(1, 0, LinkKind::Stem),
                link(2, 0, LinkKind::Stem),
                link(3, 0, LinkKind::Exact),
            ]
        );
        // `a.xmp` is an ordinary file once `xmp` leaves the list and owns the `.thm` named after it.
        let only_thm = SidecarMatcher::new(&["thm"]);
        assert_eq!(
            only_thm.link(&names),
            vec![
                link(2, 0, LinkKind::Stem),
                link(2, 1, LinkKind::Stem),
                link(3, 0, LinkKind::Exact),
            ]
        );
    }

    #[test]
    fn nothing_links_without_sidecars_or_owners() {
        let none: [&str; 0] = [];
        assert_eq!(defaults(&none), vec![]);
        assert_eq!(defaults(&["a.jpg", "b.jpg", "c.mp4"]), vec![]);
        let only_sidecars = SidecarMatcher::default().linking(&["a.xmp", "b.json"]);
        assert_eq!(only_sidecars.links, vec![]);
        assert_eq!(only_sidecars.orphans, vec![0, 1]);
    }

    #[test]
    fn indexes_refer_to_the_input_and_links_are_sorted() {
        let names = ["z.xmp", "z.jpg", "a.jpg", "a.xmp", "a.jpg.json", "z.cr3"];
        let links = defaults(&names);
        assert_eq!(
            links,
            vec![
                link(0, 1, LinkKind::Stem),
                link(0, 5, LinkKind::Stem),
                link(3, 2, LinkKind::Stem),
                link(4, 2, LinkKind::Exact),
            ]
        );
        assert!(links.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn duplicate_names_are_separate_entries() {
        assert_eq!(
            defaults(&["a.jpg", "a.jpg", "a.xmp"]),
            vec![link(2, 0, LinkKind::Stem), link(2, 1, LinkKind::Stem)]
        );
    }

    #[test]
    fn owned_strings_work_as_well_as_literals() {
        let names = vec!["a.jpg".to_owned(), "a.xmp".to_owned()];
        assert_eq!(
            SidecarMatcher::default().link(&names),
            vec![link(1, 0, LinkKind::Stem)]
        );
    }

    #[test]
    fn link_kinds_round_trip_through_their_column_values() {
        for kind in [LinkKind::Exact, LinkKind::Stem] {
            assert_eq!(kind.as_str().parse::<LinkKind>(), Ok(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(LinkKind::Exact.as_str(), "exact");
        assert_eq!(LinkKind::Stem.as_str(), "stem");
        assert_eq!(
            "Exact".parse::<LinkKind>(),
            Err(ParseLinkKindError("Exact".to_owned()))
        );
        assert_eq!(
            ParseLinkKindError("x".to_owned()).to_string(),
            "unknown sidecar link kind `x`"
        );
    }

    // ---- candidate names ----

    fn candidate(name: &str, kind: LinkKind) -> Candidate {
        Candidate {
            name: name.to_owned(),
            kind,
        }
    }

    #[test]
    fn candidates_list_the_exact_names_before_the_stem_names() {
        let matcher = SidecarMatcher::new(&["xmp", "json"]);
        assert_eq!(
            matcher.sidecar_names_for("IMG_0001.CR3"),
            vec![
                candidate("IMG_0001.CR3.xmp", LinkKind::Exact),
                candidate("IMG_0001.CR3.json", LinkKind::Exact),
                candidate("IMG_0001.xmp", LinkKind::Stem),
                candidate("IMG_0001.json", LinkKind::Stem),
            ]
        );
        assert_eq!(
            SidecarMatcher::default().sidecar_names_for("a.jpg").len(),
            12
        );
    }

    #[test]
    fn an_owner_without_an_extension_has_no_stem_candidates() {
        let matcher = SidecarMatcher::new(&["xmp"]);
        assert_eq!(
            matcher.sidecar_names_for("README"),
            vec![candidate("README.xmp", LinkKind::Exact)]
        );
        assert_eq!(
            matcher.sidecar_names_for(".hidden"),
            vec![candidate(".hidden.xmp", LinkKind::Exact)]
        );
    }

    #[test]
    fn a_sidecar_or_an_empty_name_has_no_candidates() {
        let matcher = SidecarMatcher::default();
        assert_eq!(matcher.sidecar_names_for("IMG_0001.xmp"), vec![]);
        assert_eq!(matcher.sidecar_names_for("IMG_0001.CR3.json"), vec![]);
        assert_eq!(matcher.sidecar_names_for(""), vec![]);
    }

    #[test]
    fn candidates_keep_the_spelling_of_the_owner() {
        let matcher = SidecarMatcher::new(&["XMP"]);
        assert_eq!(
            matcher.sidecar_names_for("Caf\u{e9} 1.JPG"),
            vec![
                candidate("Caf\u{e9} 1.JPG.xmp", LinkKind::Exact),
                candidate("Caf\u{e9} 1.xmp", LinkKind::Stem),
            ]
        );
    }

    #[test]
    fn candidates_link_back_to_their_owner() {
        let matcher = SidecarMatcher::default();
        for owner in [
            "IMG_0001.CR3",
            "a.b.c.jpg",
            "README",
            "caf\u{e9}.mov",
            ".hidden",
        ] {
            for found in matcher.sidecar_names_for(owner) {
                let names = [owner, found.name.as_str()];
                assert_eq!(
                    matcher.link(&names),
                    vec![link(1, 0, found.kind)],
                    "{owner} and {}",
                    found.name
                );
            }
        }
    }

    // ---- follow or share (A18) ----

    #[test]
    fn a_sidecar_follows_when_all_its_owners_are_selected() {
        let links = [("a.xmp", 1), ("a.xmp", 2), ("b.xmp", 1), ("c.xmp", 3)];
        let split = split_follow(links, |owner| *owner <= 2);
        assert_eq!(split.follow, vec!["a.xmp", "b.xmp"]);
        assert_eq!(split.shared, Vec::<&str>::new());
    }

    #[test]
    fn a_sidecar_with_an_owner_left_behind_is_shared() {
        let links = [(10, 1), (10, 2), (11, 1), (12, 3), (12, 4)];
        let split = split_follow(links, |owner| *owner == 1);
        assert_eq!(split.follow, vec![11]);
        assert_eq!(split.shared, vec![10]);
        // Nothing is said about 12: none of its owners is selected.
    }

    #[test]
    fn a_sidecar_without_a_selected_owner_is_left_out() {
        let split = split_follow([(1, 1), (2, 2)], |_: &i32| false);
        assert_eq!(
            split,
            FollowSplit {
                follow: vec![],
                shared: vec![]
            }
        );
        let none: [(i32, i32); 0] = [];
        let split = split_follow(none, |_| true);
        assert_eq!(
            split,
            FollowSplit {
                follow: vec![],
                shared: vec![]
            }
        );
    }

    #[test]
    fn the_lists_keep_first_seen_order_without_repeats() {
        let links = [(5, 1), (3, 1), (5, 1), (9, 1), (3, 2), (9, 1), (4, 1)];
        let split = split_follow(links, |owner| *owner == 1);
        assert_eq!(split.follow, vec![5, 9, 4]);
        assert_eq!(split.shared, vec![3]);
    }

    #[test]
    fn split_follow_works_with_the_links_of_a_listing() {
        let names = ["a.CR3", "a.JPG", "a.xmp", "a.CR3.xmp", "b.JPG", "b.json"];
        let links = defaults(&names);
        let pairs = links.iter().map(|link| (link.sidecar, link.owner));
        // Moving only the RAW: its own xmp follows, the one shared with the JPEG is copied.
        let split = split_follow(pairs, |owner| *owner == 0);
        assert_eq!(split.follow, vec![3]);
        assert_eq!(split.shared, vec![2]);
        // Moving both pictures of the pair takes both sidecars.
        let pairs = links.iter().map(|link| (link.sidecar, link.owner));
        let split = split_follow(pairs, |owner| *owner <= 1);
        assert_eq!(split.follow, vec![2, 3]);
        assert_eq!(split.shared, Vec::<usize>::new());
    }

    // ---- scale ----

    #[test]
    fn fifty_thousand_names_link_in_linear_time() {
        let mut names: Vec<String> = Vec::with_capacity(50_000);
        for i in 0..20_000 {
            names.push(format!("IMG_{i:05}.CR3"));
            names.push(format!("IMG_{i:05}.JPG"));
        }
        for i in 0..5_000 {
            names.push(format!("IMG_{i:05}.CR3.xmp"));
        }
        for i in 5_000..10_000 {
            names.push(format!("IMG_{i:05}.xmp"));
        }
        assert_eq!(names.len(), 50_000);

        let started = Instant::now();
        let linking = SidecarMatcher::default().linking(&names);
        let elapsed = started.elapsed();

        assert_eq!(linking.links.len(), 5_000 + 5_000 * 2);
        assert!(linking.orphans.is_empty());
        // A quadratic implementation needs minutes for this input.
        assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    }

    // ---- properties ----

    fn name() -> impl Strategy<Value = String> {
        let stem = prop_oneof![
            Just("a"),
            Just("A"),
            Just("b"),
            Just("ab"),
            Just("a.b"),
            Just("\u{e9}"),
            Just("e\u{301}"),
            Just("E\u{301}"),
        ];
        let ext = prop_oneof![
            Just("jpg"),
            Just("JPG"),
            Just("cr3"),
            Just("xmp"),
            Just("XMP"),
            Just("json"),
            Just("txt"),
        ];
        (stem, prop::collection::vec(ext, 0..3)).prop_map(|(stem, exts)| {
            exts.iter()
                .fold(stem.to_owned(), |name, ext| format!("{name}.{ext}"))
        })
    }

    fn matcher() -> impl Strategy<Value = SidecarMatcher> {
        prop_oneof![
            Just(SidecarMatcher::default()),
            Just(SidecarMatcher::new(&["xmp", "txt", "jpg"])),
            Just(SidecarMatcher::new(&["json"])),
            Just(SidecarMatcher::new::<&str>(&[])),
        ]
    }

    /// The quadratic definition of the rules, straight from the module docs.
    fn oracle(matcher: &SidecarMatcher, names: &[String]) -> Linking {
        let mut links = Vec::new();
        let mut orphans = Vec::new();
        for (s, sidecar) in names.iter().enumerate() {
            if !matcher.is_sidecar_name(sidecar) {
                continue;
            }
            let key = normalize_name(split_name(sidecar).0);
            let before = links.len();
            for (o, owner) in names.iter().enumerate() {
                if matcher.is_sidecar_name(owner) {
                    continue;
                }
                let kind = if normalize_name(owner) == key {
                    Some(LinkKind::Exact)
                } else if normalize_name(split_name(owner).0) == key {
                    Some(LinkKind::Stem)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    links.push(link(s, o, kind));
                }
            }
            if links.len() == before {
                orphans.push(s);
            }
        }
        links.sort();
        Linking { links, orphans }
    }

    proptest! {
        #[test]
        fn linking_matches_the_quadratic_definition(
            matcher in matcher(),
            names in prop::collection::vec(name(), 0..12),
        ) {
            prop_assert_eq!(matcher.linking(&names), oracle(&matcher, &names));
        }

        #[test]
        fn the_order_of_the_listing_does_not_change_what_is_linked(
            matcher in matcher(),
            names in prop::collection::vec(name(), 0..12),
        ) {
            let triples = |names: &[String]| -> BTreeSet<(String, String, LinkKind)> {
                matcher
                    .link(names)
                    .iter()
                    .map(|l| (names[l.sidecar].clone(), names[l.owner].clone(), l.kind))
                    .collect()
            };
            let mut reversed = names.clone();
            reversed.reverse();
            prop_assert_eq!(triples(&names), triples(&reversed));
        }

        #[test]
        fn every_candidate_links_back_to_its_owner(matcher in matcher(), owner in name()) {
            for found in matcher.sidecar_names_for(&owner) {
                let names = [owner.as_str(), found.name.as_str()];
                let links = matcher.link(&names);
                prop_assert_eq!(links, vec![link(1, 0, found.kind)]);
            }
        }

        #[test]
        fn normalised_extension_lists_are_valid_and_stable(
            raw in prop::collection::vec("[ .a-zA-Z0-9/\u{e9}]{0,14}", 0..12),
        ) {
            if let Ok(list) = normalise_extensions(&raw) {
                prop_assert!(list.len() <= MAX_EXTENSIONS);
                let distinct: HashSet<&String> = list.iter().collect();
                prop_assert_eq!(distinct.len(), list.len());
                for ext in &list {
                    prop_assert!((1..=MAX_EXTENSION_LEN).contains(&ext.len()));
                    prop_assert!(ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
                }
                prop_assert_eq!(normalise_extensions(&list), Ok(list.clone()));
                // The lenient constructor agrees with the strict normaliser on valid input.
                let lenient = SidecarMatcher::new(&raw);
                prop_assert_eq!(lenient.extensions(), list.as_slice());
            }
        }

        #[test]
        fn split_follow_agrees_with_its_definition(
            links in prop::collection::vec((0u8..6, 0u8..6), 0..24),
            selected in prop::collection::hash_set(0u8..6, 0..6),
        ) {
            let split = split_follow(links.iter().copied(), |owner| selected.contains(owner));

            let mut first_seen: Vec<u8> = Vec::new();
            for (sidecar, _) in &links {
                if !first_seen.contains(sidecar) {
                    first_seen.push(*sidecar);
                }
            }
            let owners_of = |sidecar: u8| -> Vec<u8> {
                links.iter().filter(|(s, _)| *s == sidecar).map(|(_, o)| *o).collect()
            };
            let expected_follow: Vec<u8> = first_seen
                .iter()
                .copied()
                .filter(|&s| {
                    let owners = owners_of(s);
                    owners.iter().any(|o| selected.contains(o))
                        && owners.iter().all(|o| selected.contains(o))
                })
                .collect();
            let expected_shared: Vec<u8> = first_seen
                .iter()
                .copied()
                .filter(|&s| {
                    let owners = owners_of(s);
                    owners.iter().any(|o| selected.contains(o))
                        && owners.iter().any(|o| !selected.contains(o))
                })
                .collect();
            prop_assert_eq!(split.follow, expected_follow);
            prop_assert_eq!(split.shared, expected_shared);
        }

        #[test]
        fn selecting_more_owners_never_makes_a_sidecar_stay_behind(
            links in prop::collection::vec((0u8..6, 0u8..6), 0..24),
            selected in prop::collection::hash_set(0u8..6, 0..6),
            extra in prop::collection::hash_set(0u8..6, 0..6),
        ) {
            let wider: HashSet<u8> = selected.union(&extra).copied().collect();
            let before = split_follow(links.iter().copied(), |owner| selected.contains(owner));
            let after = split_follow(links.iter().copied(), |owner| wider.contains(owner));
            for sidecar in &before.follow {
                prop_assert!(after.follow.contains(sidecar));
            }
            for sidecar in &before.shared {
                prop_assert!(after.shared.contains(sidecar) || after.follow.contains(sidecar));
            }
        }
    }
}
