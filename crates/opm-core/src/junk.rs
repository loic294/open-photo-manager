//! Junk rules: names and directories that are never user content (`@eaDir`, `#recycle`,
//! `.DS_Store`, `Thumbs.db`, `._*`, `.opm-trash`, `*.opm-part`, ...).
//!
//! # Contract
//! - A *junk* entry is never indexed by the scanner, ignored by the watcher, and does not count as
//!   content when deciding whether a folder is empty: a folder that holds only junk is empty, its
//!   junk is trashed first and the folder removed afterwards (plan A16, see
//!   [`JunkRules::is_empty_after_junk`]).
//! - Two rules are built in and cannot be switched off by the settings, even when both lists are
//!   empty: the per-volume trash folder ([`TRASH_DIR`]) and in-flight copies (names ending in
//!   [`PART_SUFFIX`], files only).
//! - The configurable rules are the `junk_names` list (the WHOLE name must equal an entry) and the
//!   `junk_prefixes` list (the name must START with an entry, e.g. `._` for AppleDouble files). They
//!   apply to files and directories alike: `@eaDir` and `Thumbs.db` are listed once and work
//!   for both.
//! - Comparison is case-insensitive and Unicode-normalisation-insensitive: entries and names both go
//!   through [`names::normalize_name`](crate::names::normalize_name). ASCII names take an
//!   allocation-free fast path, so calling these predicates for every directory entry is cheap.
//! - Entries are single name components: no `/`, no NUL, and a name entry is not `.` or `..`.
//!   [`JunkRules::new`] silently ignores entries that are blank or invalid (an empty prefix would
//!   otherwise make EVERY name junk); settings are validated with [`normalise_names`] and
//!   [`normalise_prefixes`] before they are stored.
//!
//! Defaults come from [`consts`](crate::consts) (`DEFAULT_JUNK_NAMES`, `DEFAULT_JUNK_PREFIXES`).

use std::collections::HashSet;

use crate::consts::{DEFAULT_JUNK_NAMES, DEFAULT_JUNK_PREFIXES, PART_SUFFIX, TRASH_DIR};
use crate::names::normalize_name;
use crate::paths::RelPath;

/// Most entries a settings list may hold.
pub const MAX_ENTRIES: usize = 200;

/// Longest settings entry, in characters.
pub const MAX_ENTRY_CHARS: usize = 255;

/// Why a junk settings list is rejected by [`normalise_names`] or [`normalise_prefixes`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JunkError {
    #[error("at most {max} junk {list} are allowed")]
    TooMany { list: &'static str, max: usize },
    #[error("a junk {list} entry is longer than {max} characters")]
    TooLong { list: &'static str, max: usize },
    #[error("junk {list} entry {entry:?} is not a valid name")]
    Invalid { list: &'static str, entry: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    Names,
    Prefixes,
}

impl List {
    const fn label(self) -> &'static str {
        match self {
            Self::Names => "names",
            Self::Prefixes => "prefixes",
        }
    }
}

enum Problem {
    TooLong,
    Invalid,
}

/// Trims one settings entry; `Ok(None)` for a blank one.
fn clean(raw: &str, list: List) -> Result<Option<&str>, Problem> {
    let entry = raw.trim();
    if entry.is_empty() {
        return Ok(None);
    }
    if entry.chars().count() > MAX_ENTRY_CHARS {
        return Err(Problem::TooLong);
    }
    let dot_name = list == List::Names && (entry == "." || entry == "..");
    if dot_name || entry.contains(['/', '\0']) {
        return Err(Problem::Invalid);
    }
    Ok(Some(entry))
}

fn normalise_list<S: AsRef<str>>(entries: &[S], list: List) -> Result<Vec<String>, JunkError> {
    let mut seen = HashSet::new();
    let mut kept: Vec<String> = Vec::new();
    for raw in entries {
        let entry = match clean(raw.as_ref(), list) {
            Ok(Some(entry)) => entry,
            Ok(None) => continue,
            Err(Problem::TooLong) => {
                return Err(JunkError::TooLong {
                    list: list.label(),
                    max: MAX_ENTRY_CHARS,
                });
            }
            Err(Problem::Invalid) => {
                return Err(JunkError::Invalid {
                    list: list.label(),
                    entry: raw.as_ref().trim().to_owned(),
                });
            }
        };
        if !seen.insert(normalize_name(entry)) {
            continue;
        }
        if kept.len() >= MAX_ENTRIES {
            return Err(JunkError::TooMany {
                list: list.label(),
                max: MAX_ENTRIES,
            });
        }
        kept.push(entry.to_owned());
    }
    Ok(kept)
}

/// Validates and normalises the `junk_names` setting: entries are trimmed, blank ones dropped,
/// duplicates (case- and normalisation-insensitive) removed keeping the first spelling and the
/// user's order. Rejects entries containing `/` or NUL, `.` and `..`, entries longer than
/// [`MAX_ENTRY_CHARS`] and lists longer than [`MAX_ENTRIES`].
pub fn normalise_names<S: AsRef<str>>(names: &[S]) -> Result<Vec<String>, JunkError> {
    normalise_list(names, List::Names)
}

/// Validates and normalises the `junk_prefixes` setting with the rules of [`normalise_names`]
/// (a prefix may be `.`).
pub fn normalise_prefixes<S: AsRef<str>>(prefixes: &[S]) -> Result<Vec<String>, JunkError> {
    normalise_list(prefixes, List::Prefixes)
}

/// Comparison keys of the usable entries: normalised, sorted, without duplicates.
fn keys<S: AsRef<str>>(entries: &[S], list: List) -> Vec<String> {
    let mut keys: Vec<String> = entries
        .iter()
        .filter_map(|raw| match clean(raw.as_ref(), list) {
            Ok(Some(entry)) => Some(normalize_name(entry)),
            _ => None,
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

fn starts_with_ignore_ascii_case(name: &str, prefix: &str) -> bool {
    name.as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

fn ends_with_part_suffix(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes
        .len()
        .checked_sub(PART_SUFFIX.len())
        .and_then(|start| bytes.get(start..))
        .is_some_and(|tail| tail.eq_ignore_ascii_case(PART_SUFFIX.as_bytes()))
}

/// A directory entry as seen by [`JunkRules::is_empty_after_junk`]. Pass symlinks and other special
/// files with `is_dir: false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub is_dir: bool,
}

impl<'a> Entry<'a> {
    /// A non-directory entry.
    pub const fn file(name: &'a str) -> Self {
        Self {
            name,
            is_dir: false,
        }
    }

    /// A directory entry.
    pub const fn dir(name: &'a str) -> Self {
        Self { name, is_dir: true }
    }
}

/// The junk rules in force: the built-in ones plus the configured names and prefixes.
/// See the module docs for the exact semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JunkRules {
    names: Vec<String>,
    prefixes: Vec<String>,
}

impl Default for JunkRules {
    /// The defaults of the settings (`DEFAULT_JUNK_NAMES`, `DEFAULT_JUNK_PREFIXES`).
    fn default() -> Self {
        Self::new(DEFAULT_JUNK_NAMES, DEFAULT_JUNK_PREFIXES)
    }
}

impl JunkRules {
    /// Builds the rules from the `junk_names` and `junk_prefixes` settings. Blank and invalid
    /// entries are ignored and no limits are enforced (validate settings with
    /// [`normalise_names`] / [`normalise_prefixes`] when they are stored).
    pub fn new<N: AsRef<str>, P: AsRef<str>>(names: &[N], prefixes: &[P]) -> Self {
        Self {
            names: keys(names, List::Names),
            prefixes: keys(prefixes, List::Prefixes),
        }
    }

    /// Only the built-in rules (trash folder and `*.opm-part`): what is left with empty settings.
    pub fn reserved_only() -> Self {
        Self {
            names: Vec::new(),
            prefixes: Vec::new(),
        }
    }

    fn matches_configured(&self, name: &str) -> bool {
        if self.names.is_empty() && self.prefixes.is_empty() {
            return false;
        }
        if name.is_ascii() {
            self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
                || self
                    .prefixes
                    .iter()
                    .any(|p| starts_with_ignore_ascii_case(name, p))
        } else {
            let key = normalize_name(name);
            self.names.contains(&key) || self.prefixes.iter().any(|p| key.starts_with(p.as_str()))
        }
    }

    /// Whether a directory with this name is junk: the trash folder, a configured name or a
    /// configured prefix. The `*.opm-part` rule does not apply to directories.
    pub fn is_junk_dir(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case(TRASH_DIR) || self.matches_configured(name)
    }

    /// Whether a file with this name is junk: everything [`is_junk_dir`](Self::is_junk_dir)
    /// matches, plus names ending in `.opm-part`.
    pub fn is_junk_name(&self, name: &str) -> bool {
        self.is_junk_dir(name) || ends_with_part_suffix(name)
    }

    /// [`is_junk_dir`](Self::is_junk_dir) for a directory entry, [`is_junk_name`](Self::is_junk_name)
    /// for anything else.
    pub fn is_junk_entry(&self, entry: Entry<'_>) -> bool {
        if entry.is_dir {
            self.is_junk_dir(entry.name)
        } else {
            self.is_junk_name(entry.name)
        }
    }

    /// Whether any component of `path` is junk: ancestors are judged as directories, the last
    /// component as a file (so a leaf named `x.opm-part` is junk whatever it is). The root is not.
    pub fn is_junk_path(&self, path: &RelPath) -> bool {
        let mut components = path.components().peekable();
        while let Some(component) = components.next() {
            let junk = if components.peek().is_some() {
                self.is_junk_dir(component)
            } else {
                self.is_junk_name(component)
            };
            if junk {
                return true;
            }
        }
        false
    }

    /// Whether a folder with these children is empty once its junk is trashed (plan A16): every
    /// child is junk. A child directory that is not junk is content, so a caller that walks
    /// bottom-up must leave out the sub-folders it has already scheduled for removal. A folder
    /// without children is empty.
    pub fn is_empty_after_junk<'a>(&self, children: impl IntoIterator<Item = Entry<'a>>) -> bool {
        children.into_iter().all(|entry| self.is_junk_entry(entry))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const NO_STRINGS: &[String] = &[];

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn rules(names: &[&str], prefixes: &[&str]) -> JunkRules {
        JunkRules::new(&strings(names), &strings(prefixes))
    }

    #[test]
    fn default_names_and_prefixes_are_junk_for_files_and_dirs() {
        let rules = JunkRules::default();
        for name in DEFAULT_JUNK_NAMES {
            assert!(rules.is_junk_name(name), "{name}");
            assert!(rules.is_junk_dir(name), "{name}");
        }
        for prefix in DEFAULT_JUNK_PREFIXES {
            let name = format!("{prefix}IMG_0001.jpg");
            assert!(rules.is_junk_name(&name), "{name}");
            assert!(rules.is_junk_dir(&name), "{name}");
        }
        assert_eq!(
            rules,
            JunkRules::new(DEFAULT_JUNK_NAMES, DEFAULT_JUNK_PREFIXES)
        );
    }

    #[test]
    fn comparison_ignores_case() {
        let rules = JunkRules::default();
        for name in [
            "thumbs.db",
            "THUMBS.DB",
            "@EADIR",
            "#Recycle",
            ".ds_store",
            "._x",
            "._",
        ] {
            assert!(rules.is_junk_name(name), "{name}");
        }
    }

    #[test]
    fn user_content_is_not_junk() {
        let rules = JunkRules::default();
        for name in [
            "IMG_0001.jpg",
            ".hidden",
            "Thumbs.db.jpg",
            "Thumbs",
            "my@eaDir",
            "x._y",
            "_.x",
            ".opm-trash2",
            "opm-trash",
            ".opm-trashed",
            "part",
            "opm-part",
            "",
        ] {
            assert!(!rules.is_junk_name(name), "{name:?}");
            assert!(!rules.is_junk_dir(name), "{name:?}");
        }
    }

    #[test]
    fn trash_and_partial_copies_are_junk_even_with_empty_settings() {
        let rules = JunkRules::reserved_only();
        assert!(rules.is_junk_dir(TRASH_DIR));
        assert!(rules.is_junk_name(TRASH_DIR));
        assert!(rules.is_junk_dir(".OPM-TRASH"));
        assert!(rules.is_junk_name("IMG_0001.jpg.opm-part"));
        assert!(rules.is_junk_name("IMG_0001.JPG.OPM-PART"));
        assert!(rules.is_junk_name(".opm-part"));
        assert!(!rules.is_junk_name("Thumbs.db"));
        assert!(!rules.is_junk_name("._x"));
        assert!(!rules.is_junk_name("IMG_0001.jpg"));
        assert!(!rules.is_junk_name("opm-part.jpg"));
        assert_eq!(JunkRules::new(NO_STRINGS, NO_STRINGS), rules);
    }

    #[test]
    fn settings_cannot_switch_the_built_in_rules_off() {
        let rules = rules(&["Cache"], &["~$"]);
        assert!(rules.is_junk_dir(TRASH_DIR));
        assert!(rules.is_junk_name("x.opm-part"));
    }

    #[test]
    fn the_part_suffix_rule_is_for_files_only() {
        let rules = JunkRules::default();
        assert!(rules.is_junk_name("copy.jpg.opm-part"));
        assert!(!rules.is_junk_dir("copy.jpg.opm-part"));
    }

    #[test]
    fn configured_names_must_match_the_whole_name() {
        let rules = rules(&["Cache", " Spaced "], &[]);
        assert!(rules.is_junk_name("cache"));
        assert!(rules.is_junk_dir("CACHE"));
        assert!(rules.is_junk_name("spaced"));
        assert!(!rules.is_junk_name("cache2"));
        assert!(!rules.is_junk_name("mycache"));
        assert!(!rules.is_junk_name("cache.txt"));
    }

    #[test]
    fn configured_prefixes_match_the_start_of_the_name() {
        let rules = rules(&[], &["~$", "tmp_"]);
        assert!(rules.is_junk_name("~$Report.docx"));
        assert!(rules.is_junk_name("TMP_1"));
        assert!(rules.is_junk_dir("tmp_dir"));
        assert!(rules.is_junk_name("tmp_"));
        assert!(!rules.is_junk_name("x~$"));
        assert!(!rules.is_junk_name("tmp"));
        assert!(!rules.is_junk_name("my_tmp_"));
    }

    #[test]
    fn unicode_forms_and_case_are_ignored() {
        let nfd = "E\u{301}ditions";
        let nfc_lower = "\u{e9}ditions";
        let rules = rules(&[nfd], &["\u{c9}t\u{e9}"]);
        assert!(rules.is_junk_name(nfc_lower));
        assert!(rules.is_junk_name("\u{c9}DITIONS"));
        assert!(rules.is_junk_dir("e\u{301}ditions"));
        assert!(rules.is_junk_name("e\u{301}te\u{301}-2024"));
        assert!(rules.is_junk_name("\u{c9}T\u{c9}.txt"));
        assert!(!rules.is_junk_name("editions"));
        assert!(!rules.is_junk_name("\u{e9}ditions2"));
    }

    #[test]
    fn non_ascii_text_never_matches_ascii_entries_by_accident() {
        let rules = JunkRules::default();
        assert!(!rules.is_junk_name("Thumbs.d\u{e9}"));
        assert!(!rules.is_junk_name("\u{e9}Thumbs.db"));
        assert!(rules.is_junk_name("._\u{e9}t\u{e9}.jpg"));
    }

    #[test]
    fn blank_and_invalid_entries_are_ignored_by_new() {
        let rules = rules(
            &["", "   ", "a/b", "nul\0", ".", "..", "Real"],
            &["", " \t", "x/y", "nul\0"],
        );
        assert!(rules.is_junk_name("real"));
        assert!(!rules.is_junk_name("IMG_0001.jpg"));
        assert!(!rules.is_junk_name("a"));
        assert!(!rules.is_junk_name("."));
        assert!(!rules.is_junk_name(".."));
        assert!(!rules.is_junk_name("x"));
        assert_eq!(rules, JunkRules::new(&["Real"], NO_STRINGS));
    }

    #[test]
    fn an_empty_prefix_does_not_make_everything_junk() {
        let rules = rules(&[""], &["", "  "]);
        assert!(!rules.is_junk_name("IMG_0001.jpg"));
        assert!(!rules.is_junk_dir("2024"));
        assert!(!rules.is_junk_name("\u{e9}t\u{e9}"));
    }

    #[test]
    fn duplicate_entries_collapse() {
        let one = rules(&["Cache"], &["._"]);
        let many = rules(&["cache", "CACHE", "Cache "], &["._", "._"]);
        assert_eq!(one, many);
    }

    #[test]
    fn normalise_names_trims_dedupes_and_keeps_the_first_spelling_and_order() {
        let input = strings(&[" @eaDir ", "", "Thumbs.db", "@EADIR", "  ", "Cache"]);
        assert_eq!(
            normalise_names(&input).unwrap(),
            strings(&["@eaDir", "Thumbs.db", "Cache"])
        );
        assert_eq!(normalise_names(NO_STRINGS).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn normalise_dedupes_across_unicode_forms() {
        let input = strings(&["\u{c9}t\u{e9}", "e\u{301}te\u{301}"]);
        assert_eq!(
            normalise_names(&input).unwrap(),
            strings(&["\u{c9}t\u{e9}"])
        );
    }

    #[test]
    fn normalise_keeps_the_defaults_as_they_are() {
        let names = normalise_names(DEFAULT_JUNK_NAMES).unwrap();
        assert_eq!(names, strings(DEFAULT_JUNK_NAMES));
        let prefixes = normalise_prefixes(DEFAULT_JUNK_PREFIXES).unwrap();
        assert_eq!(prefixes, strings(DEFAULT_JUNK_PREFIXES));
    }

    #[test]
    fn normalise_rejects_path_separators_nul_and_dot_names() {
        for bad in ["a/b", "/", "x\0", ".", ".."] {
            assert_eq!(
                normalise_names(&[bad]),
                Err(JunkError::Invalid {
                    list: "names",
                    entry: bad.to_owned()
                }),
                "{bad:?}"
            );
        }
        assert_eq!(
            normalise_prefixes(&["../"]),
            Err(JunkError::Invalid {
                list: "prefixes",
                entry: "../".to_owned()
            })
        );
        assert_eq!(normalise_prefixes(&["."]).unwrap(), strings(&["."]));
    }

    #[test]
    fn normalise_enforces_the_size_limits() {
        let long = "x".repeat(MAX_ENTRY_CHARS + 1);
        assert_eq!(
            normalise_names(&[long]),
            Err(JunkError::TooLong {
                list: "names",
                max: MAX_ENTRY_CHARS
            })
        );
        let longest = "\u{e9}".repeat(MAX_ENTRY_CHARS);
        assert_eq!(normalise_prefixes(&[&longest]).unwrap().len(), 1);

        let many: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("n{i}")).collect();
        assert_eq!(
            normalise_names(&many),
            Err(JunkError::TooMany {
                list: "names",
                max: MAX_ENTRIES
            })
        );
        assert_eq!(
            normalise_names(&many[..MAX_ENTRIES]).unwrap().len(),
            MAX_ENTRIES
        );
        // Blanks and duplicates do not count towards the limit.
        let padded: Vec<String> = many[..MAX_ENTRIES]
            .iter()
            .flat_map(|n| [n.clone(), n.to_uppercase(), String::new()])
            .collect();
        assert_eq!(normalise_names(&padded).unwrap().len(), MAX_ENTRIES);
    }

    #[test]
    fn error_messages_name_the_list() {
        let err = normalise_names(&["a/b"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "junk names entry \"a/b\" is not a valid name"
        );
        let err = normalise_prefixes(&["x".repeat(300)]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "a junk prefixes entry is longer than 255 characters"
        );
    }

    #[test]
    fn paths_are_junk_when_any_component_is() {
        let rules = JunkRules::default();
        let path = |p: &str| RelPath::new(p).unwrap();
        assert!(rules.is_junk_path(&path("Photos/@eaDir/IMG.jpg/SYNOPHOTO_THUMB_M.jpg")));
        assert!(rules.is_junk_path(&path("Photos/2024/.DS_Store")));
        assert!(rules.is_junk_path(&path(".opm-trash/20240101/Photos/IMG.jpg")));
        assert!(rules.is_junk_path(&path("Photos/._IMG.jpg")));
        assert!(rules.is_junk_path(&path("Photos/IMG.jpg.opm-part")));
        assert!(!rules.is_junk_path(&path("Photos/2024/IMG.jpg")));
        assert!(!rules.is_junk_path(&path("Photos/IMG.opm-part/IMG.jpg")));
        assert!(!rules.is_junk_path(&RelPath::root()));
    }

    #[test]
    fn a_folder_with_only_junk_is_empty() {
        let rules = JunkRules::default();
        assert!(rules.is_empty_after_junk([]));
        assert!(rules.is_empty_after_junk([Entry::file(".DS_Store")]));
        assert!(rules.is_empty_after_junk([
            Entry::file("Thumbs.db"),
            Entry::dir("@eaDir"),
            Entry::file("._IMG.jpg"),
            Entry::file("copy.jpg.opm-part"),
        ]));
    }

    #[test]
    fn any_content_keeps_a_folder_non_empty() {
        let rules = JunkRules::default();
        assert!(!rules.is_empty_after_junk([Entry::file("IMG_0001.jpg")]));
        assert!(!rules.is_empty_after_junk([Entry::file(".DS_Store"), Entry::file("notes.txt")]));
        assert!(!rules.is_empty_after_junk([Entry::dir("2024"), Entry::file("Thumbs.db")]));
        // A directory is judged by the directory rules, so `x.opm-part/` is content.
        assert!(!rules.is_empty_after_junk([Entry::dir("x.opm-part")]));
        assert!(rules.is_empty_after_junk([Entry::file("x.opm-part")]));
    }

    #[test]
    fn junk_entries_follow_the_entry_kind() {
        let rules = JunkRules::default();
        assert!(rules.is_junk_entry(Entry::dir("@eaDir")));
        assert!(rules.is_junk_entry(Entry::file("a.opm-part")));
        assert!(!rules.is_junk_entry(Entry::dir("a.opm-part")));
        assert!(!rules.is_junk_entry(Entry::file("a.jpg")));
    }

    proptest! {
        #[test]
        fn predicates_never_panic_and_the_built_in_rules_always_apply(
            names in proptest::collection::vec("\\PC{0,12}", 0..5),
            prefixes in proptest::collection::vec("\\PC{0,4}", 0..5),
            name in "\\PC{0,20}",
        ) {
            let rules = JunkRules::new(&names, &prefixes);
            let _ = rules.is_junk_name(&name);
            let _ = rules.is_junk_dir(&name);
            let partial = format!("{name}{PART_SUFFIX}");
            prop_assert!(rules.is_junk_dir(TRASH_DIR));
            prop_assert!(rules.is_junk_name(&partial));
        }

        #[test]
        fn blank_settings_entries_never_match_ordinary_names(
            blanks in proptest::collection::vec("[ \\t]{0,3}", 0..4),
            name in "[A-Z0-9_]{1,12}",
        ) {
            let rules = JunkRules::new(&blanks, &blanks);
            prop_assert!(!rules.is_junk_name(&name));
            prop_assert!(!rules.is_junk_dir(&name));
        }

        #[test]
        fn ascii_matching_ignores_case(name in "[A-Za-z0-9@#._ -]{0,14}") {
            let rules = JunkRules::default();
            prop_assert_eq!(
                rules.is_junk_name(&name.to_ascii_lowercase()),
                rules.is_junk_name(&name.to_ascii_uppercase())
            );
        }

        #[test]
        fn a_configured_name_matches_in_any_ascii_case(entry in "[A-Za-z0-9@#_-]{1,10}") {
            let rules = JunkRules::new(std::slice::from_ref(&entry), NO_STRINGS);
            prop_assert!(rules.is_junk_name(&entry.to_ascii_uppercase()));
            prop_assert!(rules.is_junk_dir(&entry.to_ascii_lowercase()));
        }

        #[test]
        fn a_configured_prefix_matches_any_continuation(
            prefix in "[A-Za-z0-9@#_~$-]{1,6}",
            rest in "\\PC{0,10}",
        ) {
            let rules = JunkRules::new(NO_STRINGS, std::slice::from_ref(&prefix));
            let name = format!("{}{rest}", prefix.to_ascii_uppercase());
            prop_assert!(rules.is_junk_name(&name));
        }

        #[test]
        fn emptiness_is_the_conjunction_of_the_entries(
            entries in proptest::collection::vec(("[A-Za-z._]{0,8}", any::<bool>()), 0..6),
        ) {
            let rules = JunkRules::default();
            let entries: Vec<Entry<'_>> = entries
                .iter()
                .map(|(name, is_dir)| Entry { name, is_dir: *is_dir })
                .collect();
            let expected = entries.iter().all(|e| rules.is_junk_entry(*e));
            prop_assert_eq!(rules.is_empty_after_junk(entries), expected);
        }
    }
}
