//! Architecture guardrails for the file-operations module.
//!
//! `fsops` is the only code that may change the files of a library volume: it journals every
//! operation, recovers it after a crash and keeps the index in step. These tests read the Rust
//! sources as text and fail the build when other code starts deleting, renaming or copying files
//! on its own, and when `fsops` itself slips into a habit the module rules out (panics, unsafe
//! code, spawned processes, paths in logs above debug level, its own hashing).
//!
//! They are a net for accidents, not a defence against someone who wants to evade them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use apm_core::dto::common::ConflictPolicy;
use apm_server::fsops::{OpContext, Step, TrashBatch, TrashRequest};
use common::fsops::{Fx, Trap, crash_fx, loc, request};

const FSOPS_DIR: &str = "apm-server/src/fsops/";

/// Where files may be deleted, renamed or copied outside `fsops/`: `(file under crates/, number of
/// call sites, why)`. Every entry works on the app's own data (a cache, a key file) or on a file
/// the caller has just created itself; none touches the files of a library. A new call site, in
/// these files or elsewhere, fails the build until it is either routed through `fsops` or listed
/// here with a justification. Keep the list sorted by path.
const ALLOWED_OUTSIDE_FSOPS: &[(&str, usize, &str)] = &[
    (
        "apm-server/src/db/volumes.rs",
        1,
        "the writability probe removes the uniquely named file it has just created in the volume root",
    ),
    (
        "apm-server/src/immich/crypto.rs",
        1,
        "replaces an unusable Immich key file in the app's own data directory",
    ),
    (
        "apm-server/src/media/cache.rs",
        3,
        "the media cache in the data directory: empties its tmp folder, publishes an entry, drops a failed part",
    ),
    (
        "apm-server/src/media/external.rs",
        1,
        "removes the temporary output of an external converter from the cache's tmp folder",
    ),
    (
        "apm-server/src/media/thumbs.rs",
        1,
        "the sweep that evicts stale thumbnails from the media cache",
    ),
];

// --- reading the sources -----------------------------------------------------------------------

struct Token {
    text: String,
    line: usize,
}

struct Source {
    /// Relative to `crates/`, with `/` separators.
    path: String,
    /// Braces open and close equally often once comments and literals are blanked out: the lexer
    /// kept track of what is code.
    balanced: bool,
    /// Only compiled for tests: declared by its parent as `#[cfg(test)] mod name;`, or inside such
    /// a module.
    test_only: bool,
    /// The production code: no comments, no literal contents, no `#[cfg(test)]` module. Empty for
    /// a test-only file.
    tokens: Vec<Token>,
}

fn sources() -> &'static [Source] {
    static SOURCES: OnceLock<Vec<Source>> = OnceLock::new();
    SOURCES.get_or_init(load_sources)
}

fn production() -> impl Iterator<Item = &'static Source> {
    sources().iter().filter(|source| !source.test_only)
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    entries
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for path in sorted_entries(dir) {
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn load_sources() -> Vec<Source> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let mut files = Vec::new();
    for krate in sorted_entries(&crates) {
        let src = krate.join("src");
        if src.is_dir() {
            collect_rust_files(&src, &mut files);
        }
    }
    let texts: Vec<(String, String)> = files
        .iter()
        .map(|file| {
            let rel = file
                .strip_prefix(&crates)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let blank = blank_non_code(&fs::read_to_string(file).unwrap()).unwrap_or_else(|| {
                panic!("{rel}: a literal or a comment is not closed (or the guardrail lexer lost track)")
            });
            (rel, blank)
        })
        .collect();
    let names: BTreeSet<&str> = texts.iter().map(|(rel, _)| rel.as_str()).collect();
    let mut test_files = BTreeSet::new();
    let mut test_dirs = Vec::new();
    let mut parsed = Vec::new();
    for (rel, blank) in &texts {
        let (tokens, test_mods) = production_tokens(blank);
        for name in test_mods {
            let dir = module_dir(rel);
            let file = [format!("{dir}/{name}.rs"), format!("{dir}/{name}/mod.rs")]
                .into_iter()
                .find(|candidate| names.contains(candidate.as_str()))
                .unwrap_or_else(|| panic!("{rel}: no file for `#[cfg(test)] mod {name};`"));
            test_files.insert(file);
            test_dirs.push(format!("{dir}/{name}/"));
        }
        parsed.push(tokens);
    }
    texts
        .iter()
        .zip(parsed)
        .map(|((rel, blank), tokens)| {
            let test_only = test_files.contains(rel)
                || test_dirs.iter().any(|dir| rel.starts_with(dir.as_str()));
            Source {
                path: rel.clone(),
                balanced: blank.matches('{').count() == blank.matches('}').count(),
                test_only,
                tokens: if test_only { Vec::new() } else { tokens },
            }
        })
        .collect()
}

/// The folder where the submodules of the module in `rel` live.
fn module_dir(rel: &str) -> String {
    let (dir, file) = rel.rsplit_once('/').unwrap_or(("", rel));
    match file {
        "mod.rs" | "lib.rs" | "main.rs" => dir.to_owned(),
        _ => format!("{dir}/{}", file.trim_end_matches(".rs")),
    }
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The text with comments and the insides of string and character literals replaced by spaces
/// (new lines are kept, so line numbers still match). `None` when a literal or a comment is not
/// closed.
fn blank_non_code(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0_usize;
            loop {
                match (chars.get(i), chars.get(i + 1)) {
                    (Some('/'), Some('*')) => {
                        depth += 1;
                        out.push_str("  ");
                        i += 2;
                    }
                    (Some('*'), Some('/')) => {
                        depth -= 1;
                        out.push_str("  ");
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    }
                    (Some(&other), _) => {
                        out.push(blank(other));
                        i += 1;
                    }
                    (None, _) => return None,
                }
            }
        } else if c == '"' {
            out.push('"');
            i += 1;
            loop {
                match chars.get(i) {
                    None => return None,
                    Some('"') => {
                        out.push('"');
                        i += 1;
                        break;
                    }
                    Some('\\') => {
                        out.push(' ');
                        i += 1;
                        if let Some(&escaped) = chars.get(i) {
                            out.push(blank(escaped));
                            i += 1;
                        }
                    }
                    Some(&other) => {
                        out.push(blank(other));
                        i += 1;
                    }
                }
            }
        } else if c == 'r' && starts_raw_string(&chars, i) {
            let hashes = chars[i + 1..].iter().take_while(|&&h| h == '#').count();
            out.push('r');
            out.extend(std::iter::repeat_n('#', hashes));
            out.push('"');
            i += hashes + 2;
            loop {
                match chars.get(i) {
                    None => return None,
                    Some('"')
                        if chars[i + 1..].iter().take_while(|&&h| h == '#').count() >= hashes =>
                    {
                        out.push('"');
                        out.extend(std::iter::repeat_n('#', hashes));
                        i += hashes + 1;
                        break;
                    }
                    Some(&other) => {
                        out.push(blank(other));
                        i += 1;
                    }
                }
            }
        } else if c == '\'' {
            if next == Some('\\') {
                // An escaped character: skip the escaped one, then find the closing quote.
                let close = (i + 3..chars.len()).find(|&j| chars[j] == '\'')?;
                out.push('\'');
                out.extend(chars[i + 1..close].iter().map(|&inner| blank(inner)));
                out.push('\'');
                i = close + 1;
            } else if chars.get(i + 2) == Some(&'\'') {
                out.push_str("' '");
                i += 3;
            } else {
                // A lifetime or a label.
                out.push('\'');
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    Some(out)
}

/// Whether the `r` at `i` opens a raw string (`r"..."`, `r#"..."#`, also as `br`/`cr`).
fn starts_raw_string(chars: &[char], i: usize) -> bool {
    let standalone = match i.checked_sub(1).map(|p| chars[p]) {
        None => true,
        Some('b' | 'c') => i == 1 || !is_identifier_char(chars[i - 2]),
        Some(prev) => !is_identifier_char(prev),
    };
    let hashes = chars[i + 1..].iter().take_while(|&&c| c == '#').count();
    standalone && chars.get(i + 1 + hashes) == Some(&'"')
}

/// The tokens of the production code in `blank` (comments and literals already blanked out), and
/// the names of the `#[cfg(test)] mod name;` declarations. The code from an inline
/// `#[cfg(test)] mod name { ... }` to the end of the file is not production code: clippy
/// (`items_after_test_module`) keeps that module last in a file.
fn production_tokens(blank: &str) -> (Vec<Token>, Vec<String>) {
    let mut tokens = Vec::new();
    let mut test_mods = Vec::new();
    let mut pending = false;
    for (index, raw) in blank.lines().enumerate() {
        let line = raw.trim();
        let item = match line.strip_prefix("#[cfg(test)]") {
            Some(rest) => {
                pending = true;
                rest.trim()
            }
            None => line,
        };
        if pending && !item.is_empty() && !item.starts_with("#[") {
            pending = false;
            if let Some((name, inline)) = parse_mod(item) {
                if inline {
                    break;
                }
                test_mods.push(name);
                continue;
            }
        }
        if line != "#[cfg(test)]" {
            tokenize(raw, index + 1, &mut tokens);
        }
    }
    (tokens, test_mods)
}

/// `mod name;` (out of line) or `mod name {` (inline), with an optional visibility.
fn parse_mod(item: &str) -> Option<(String, bool)> {
    let mut rest = item;
    if let Some(after) = rest.strip_prefix("pub") {
        rest = after.trim_start();
        if rest.starts_with('(') {
            rest = rest[rest.find(')')? + 1..].trim_start();
        }
    }
    let rest = rest.strip_prefix("mod ")?.trim_start();
    let name: String = rest
        .chars()
        .take_while(|&c| is_identifier_char(c))
        .collect();
    match rest[name.len()..].trim_start().chars().next()? {
        '{' => Some((name, true)),
        ';' => Some((name, false)),
        _ => None,
    }
}

/// Identifiers, `::` and single punctuation characters.
fn tokenize(line: &str, number: usize, out: &mut Vec<Token>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_identifier_char(c) {
            let start = i;
            while i < chars.len() && is_identifier_char(chars[i]) {
                i += 1;
            }
            out.push(Token {
                text: chars[start..i].iter().collect(),
                line: number,
            });
        } else if c == ':' && chars.get(i + 1) == Some(&':') {
            out.push(Token {
                text: "::".to_owned(),
                line: number,
            });
            i += 2;
        } else {
            if !c.is_whitespace() {
                out.push(Token {
                    text: c.to_string(),
                    line: number,
                });
            }
            i += 1;
        }
    }
}

fn tokens_of(code: &str) -> (Vec<Token>, Vec<String>) {
    production_tokens(&blank_non_code(code).unwrap())
}

fn text(tokens: &[Token], i: usize) -> &str {
    tokens.get(i).map_or("", |token| token.text.as_str())
}

// --- what is looked for ------------------------------------------------------------------------

/// The lines of the calls that delete, rename or copy a file or a folder: `remove_file`,
/// `remove_dir`, `remove_dir_all`, `fs::rename`, `fs::copy` (also imported with `use`), the
/// system calls under other names, and `hard_link`.
fn destructive_calls(tokens: &[Token]) -> Vec<usize> {
    const DIRECT: [&str; 11] = [
        "remove_file",
        "remove_dir",
        "remove_dir_all",
        "unlink",
        "unlinkat",
        "rmdir",
        "renameat",
        "renameat2",
        "renameat_with",
        "renamex_np",
        "hard_link",
    ];
    let mut lines = Vec::new();
    let (mut in_use, mut use_has_fs) = (false, false);
    for (i, token) in tokens.iter().enumerate() {
        let name = token.text.as_str();
        match name {
            "use" => (in_use, use_has_fs) = (true, false),
            ";" => in_use = false,
            "fs" if in_use => use_has_fs = true,
            _ => {}
        }
        let qualified = i >= 2 && text(tokens, i - 1) == "::" && text(tokens, i - 2) == "fs";
        let fs_function =
            matches!(name, "rename" | "copy") && (qualified || (in_use && use_has_fs));
        if DIRECT.contains(&name) || fs_function {
            lines.push(token.line);
        }
    }
    lines
}

/// What `fsops` must never contain: `(line, what)`.
fn forbidden_in_fsops(tokens: &[Token]) -> Vec<(usize, String)> {
    const LINTS: [&str; 5] = [
        "unwrap_used",
        "expect_used",
        "unsafe_code",
        "panic",
        "indexing_slicing",
    ];
    const MACROS: [&str; 11] = [
        "panic",
        "todo",
        "unimplemented",
        "unreachable",
        "assert",
        "assert_eq",
        "assert_ne",
        "println",
        "eprintln",
        "print",
        "dbg",
    ];
    let mut found = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let (name, prev, next) = (
            token.text.as_str(),
            if i == 0 { "" } else { text(tokens, i - 1) },
            text(tokens, i + 1),
        );
        let what = match name {
            "unwrap" | "expect" if prev == "." && next == "(" => Some(format!(".{name}()")),
            "unsafe" => Some("unsafe".to_owned()),
            "Command" => Some("a spawned process".to_owned()),
            "process" if next == "::" => Some("std::process".to_owned()),
            "sha1" | "Sha1" => Some("hashing outside apm_core::hash".to_owned()),
            _ if next == "!" && MACROS.contains(&name) => Some(format!("{name}!")),
            "allow" if prev == "[" && next == "(" => {
                let args = tokens[i + 2..]
                    .iter()
                    .take_while(|inner| inner.text != ")")
                    .map(|inner| inner.text.as_str());
                let silenced: Vec<&str> = args.filter(|arg| LINTS.contains(arg)).collect();
                (!silenced.is_empty()).then(|| format!("#[allow({})]", silenced.join(", ")))
            }
            _ => None,
        };
        if let Some(what) = what {
            found.push((token.line, what));
        }
    }
    found
}

/// The lines of the `error!`, `warn!` and `info!` calls that name a path: paths belong at debug
/// level, where an operator asks for them.
fn paths_in_logs(tokens: &[Token]) -> Vec<usize> {
    const PATHY: [&str; 13] = [
        "path", "paths", "src", "dst", "rel", "loc", "dir", "part", "name", "from", "to", "folder",
        "file",
    ];
    let mut lines = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if !matches!(token.text.as_str(), "error" | "warn" | "info")
            || text(tokens, i + 1) != "!"
            || text(tokens, i + 2) != "("
        {
            continue;
        }
        let mut depth = 0_usize;
        for inner in &tokens[i + 2..] {
            match inner.text.as_str() {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                name if PATHY.contains(&name) => {
                    lines.push(token.line);
                    break;
                }
                _ => {}
            }
        }
    }
    lines
}

// --- the guardrails ----------------------------------------------------------------------------

#[test]
fn only_fsops_deletes_renames_or_copies_files() {
    let mut found: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for source in production().filter(|source| !source.path.starts_with(FSOPS_DIR)) {
        let lines = destructive_calls(&source.tokens);
        if !lines.is_empty() {
            found.insert(&source.path, lines);
        }
    }
    let mut problems = Vec::new();
    for (path, lines) in &found {
        match ALLOWED_OUTSIDE_FSOPS
            .iter()
            .find(|(allowed, ..)| allowed == path)
        {
            None => problems.push(format!(
                "{path}: lines {lines:?} delete, rename or copy files"
            )),
            Some((_, calls, _)) if *calls != lines.len() => problems.push(format!(
                "{path}: {} call sites (lines {lines:?}), the allow-list says {calls}",
                lines.len()
            )),
            Some(_) => {}
        }
    }
    for (path, calls, _) in ALLOWED_OUTSIDE_FSOPS {
        if !found.contains_key(path) {
            problems.push(format!(
                "{path}: allowed {calls} call sites but none is left: remove the entry"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "files are changed in one place only, crates/apm-server/src/fsops (it journals the \
         operation, recovers it after a crash and updates the index). Route the change through \
         `FsOps`, or, when the code only touches the app's own data, list the file in \
         ALLOWED_OUTSIDE_FSOPS (tests/fsops_guardrails.rs) with a one-line justification:\n{problems:#?}"
    );
}

#[test]
fn the_allow_list_is_sorted_and_every_entry_is_justified() {
    for pair in ALLOWED_OUTSIDE_FSOPS.windows(2) {
        assert!(
            pair[0].0 < pair[1].0,
            "{} and {} are not sorted",
            pair[0].0,
            pair[1].0
        );
    }
    for (path, calls, why) in ALLOWED_OUTSIDE_FSOPS {
        assert!(*calls > 0, "{path}: an entry without call sites");
        assert!(
            !why.trim().is_empty() && !why.contains('\n'),
            "{path}: one line of justification is needed"
        );
        assert!(
            !path.starts_with(FSOPS_DIR),
            "{path}: fsops needs no allowance"
        );
        assert!(
            sources().iter().any(|source| source.path == *path),
            "{path}: no such file"
        );
    }
}

#[test]
fn fsops_has_no_panics_unsafe_code_processes_printing_or_hashing_of_its_own() {
    let mut problems = Vec::new();
    for source in production().filter(|source| source.path.starts_with(FSOPS_DIR)) {
        for (line, what) in forbidden_in_fsops(&source.tokens) {
            problems.push(format!("{}:{line}: {what}", source.path));
        }
    }
    assert!(
        problems.is_empty(),
        "fsops returns errors instead of panicking, has no unsafe code, runs no shell or other \
         process, logs through `tracing` and hashes through apm_core::hash:\n{problems:#?}"
    );
}

#[test]
fn fsops_logs_no_path_above_debug_level() {
    let mut problems = Vec::new();
    for source in production().filter(|source| source.path.starts_with(FSOPS_DIR)) {
        for line in paths_in_logs(&source.tokens) {
            problems.push(format!("{}:{line}", source.path));
        }
    }
    assert!(
        problems.is_empty(),
        "a path (even a volume-relative one) is logged at debug level only; put it in a \
         separate `tracing::debug!`:\n{problems:#?}"
    );
}

// --- the scanner itself ------------------------------------------------------------------------

#[test]
fn the_scanner_reads_every_source_and_the_module_that_owns_the_calls() {
    let all = sources();
    assert!(all.len() > 40, "only {} sources were found", all.len());
    let lost: Vec<&str> = all
        .iter()
        .filter(|source| !source.balanced)
        .map(|source| source.path.as_str())
        .collect();
    assert!(
        lost.is_empty(),
        "the lexer lost track of the code in {lost:?}"
    );
    let owned: usize = production()
        .filter(|source| source.path.starts_with(FSOPS_DIR))
        .map(|source| destructive_calls(&source.tokens).len())
        .sum();
    assert!(
        owned >= 5,
        "fsops owns the destructive calls, the scanner saw {owned}"
    );
    for test_only in ["fsops/test_support.rs", "indexer/pending/tests.rs"] {
        let source = all
            .iter()
            .find(|source| source.path.ends_with(test_only))
            .unwrap_or_else(|| panic!("{test_only} is not among the sources"));
        assert!(source.test_only && source.tokens.is_empty(), "{test_only}");
    }
    assert!(production().any(|source| source.path == "apm-server/src/fsops/copy.rs"));
}

#[test]
fn the_scanner_sees_code_and_only_code() {
    let sample = r##"
use std::fs;
use std::io::copy;
// fs::remove_file(in_a_comment);
/// fs::rename(in_a_doc_comment);
/* fs::remove_dir(in_a /* nested */ block_comment); */
fn sample<'a>(path: &'a Path) -> &'a Path {
    let message = "fs::remove_file(in_a_string) \
                   fs::remove_file(still_in_the_string)";
    let raw = r#"fs::copy("in", "a raw string")"#;
    let quote = '"';
    let brace = '{';
    let escaped = '\'';
    fs::remove_file(path);
    std::fs::rename(path, other);
    tokio::fs::copy(path, other).await;
    fs::remove_dir_all(path);
    fs::remove_dir(path);
    remove_dir_if_removable(path);
    io::copy(&mut reader, &mut writer);
    path
}
#[cfg(test)]
mod tests {
    fn in_the_tests() {
        fs::remove_file(path);
    }
}
"##;
    let (tokens, test_mods) = tokens_of(sample);
    assert!(test_mods.is_empty());
    assert_eq!(destructive_calls(&tokens), [14, 15, 16, 17, 18]);

    let imports = "use std::fs::{self, copy};\nuse std::fs::rename;\npub use tokio::fs::remove_file;\n\
                   use std::{fs, io};\nuse std::io::copy as other;\n";
    assert_eq!(destructive_calls(&tokens_of(imports).0), [1, 2, 3]);
}

#[test]
fn the_scanner_tells_test_modules_from_production_code() {
    let sample = "\
#[cfg(test)]
mod tests;
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod helpers;
#[cfg(test)]
fn only_for_tests() { fs::remove_file(path); }
pub mod real;
#[cfg(test)] mod inline { fn t() {} }
fn never_scanned() { fs::remove_file(path); }
";
    let (tokens, test_mods) = tokens_of(sample);
    assert_eq!(test_mods, ["tests", "helpers"]);
    // A `#[cfg(test)]` item that is not a module counts as production code: the cautious way.
    assert_eq!(destructive_calls(&tokens), [7]);
    assert!(tokens.iter().any(|token| token.text == "real"));
    assert!(!tokens.iter().any(|token| token.text == "never_scanned"));
}

#[test]
fn the_scanner_finds_what_fsops_must_not_contain() {
    let sample = r##"
fn bad() {
    a.unwrap();
    b.expect("why");
    c.unwrap_or(1);
    d.unwrap_or_else(|| 2);
    debug_assert!(ok);
    unsafe { f() }
    panic!("no");
    std::process::exit(1);
    Command::new("sh");
    println!("hi");
    let hasher = Sha1::new();
}
#![allow(clippy::expect_used)]
#[allow(dead_code, clippy::unwrap_used)]
#[allow(clippy::too_many_arguments)]
"##;
    let found: Vec<String> = forbidden_in_fsops(&tokens_of(sample).0)
        .into_iter()
        .map(|(line, what)| format!("{line} {what}"))
        .collect();
    let expected = [
        "3 .unwrap()",
        "4 .expect()",
        "8 unsafe",
        "9 panic!",
        "10 std::process",
        "11 a spawned process",
        "12 println!",
        "13 hashing outside apm_core::hash",
        "15 #[allow(expect_used)]",
        "16 #[allow(unwrap_used)]",
    ];
    assert_eq!(found, expected);
}

#[test]
fn the_scanner_finds_paths_in_logs_above_debug_level() {
    let sample = r##"
fn logs() {
    tracing::warn!(error = %err, "could not do the thing");
    tracing::warn!(path = %p, "a path at warn level");
    tracing::debug!(path = %p, "a path at debug level");
    tracing::info!(
        journal_id = row.id, src = ?row.src,
        "continued \
         string"
    );
    tracing::error!(kind = ?err.kind(), "the word path in a message is fine");
    error!(dst = ?x, "without the prefix");
    tracing::info!(folders, "a count");
}
"##;
    assert_eq!(paths_in_logs(&tokens_of(sample).0), [4, 6, 12]);
}

// --- no absolute path leaves the module --------------------------------------------------------

/// Rows of text columns that contain `needle`, as `table.column`.
async fn columns_containing(fx: &Fx, needle: &str) -> Vec<String> {
    const COLUMNS: [(&str, &str); 10] = [
        ("fs_journal", "src_path"),
        ("fs_journal", "dst_path"),
        ("fs_journal", "part_path"),
        ("fs_journal", "error"),
        ("trash_items", "orig_path"),
        ("trash_items", "trash_path"),
        ("trash_items", "reason"),
        ("trash_items", "meta_json"),
        ("files", "dir_path"),
        ("files", "name"),
    ];
    let mut found = Vec::new();
    for (table, column) in COLUMNS {
        let sql =
            format!("SELECT COUNT(*) FROM {table} WHERE instr(COALESCE({column}, ''), ?) > 0");
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(needle)
            .fetch_one(fx.app.state.db.read())
            .await
            .unwrap();
        if count > 0 {
            found.push(format!("{table}.{column}"));
        }
    }
    found
}

#[tokio::test]
async fn no_absolute_path_reaches_the_database_or_an_error_message() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    for name in ["IMG_1", "IMG_2", "IMG_3", "IMG_4"] {
        fx.ta.write(&format!("x/{name}.jpg"), name.as_bytes());
    }
    let outside = common::scratch_dir();
    fs::write(outside.path().join("elsewhere.jpg"), b"not in the library").unwrap();
    symlink(
        outside.path().join("elsewhere.jpg"),
        fx.ta.path("x/link.jpg"),
    )
    .unwrap();
    fx.scan().await;
    let cx = OpContext::detached();
    let policy = ConflictPolicy::Smart;
    let trash = |path: &str| TrashRequest {
        file_id: None,
        loc: loc(fx.a, path),
        expect: None,
        batch: TrashBatch::adhoc(),
        reason: "a test".to_owned(),
    };

    // What succeeds: a move into a new folder, a trash, a restore, a second trash and a purge.
    fx.ops
        .move_file(
            request(loc(fx.a, "x/IMG_1.jpg"), loc(fx.b, "y/z/IMG_1.jpg"), policy),
            &cx,
        )
        .await
        .unwrap();
    let first = fx.ops.trash_file(trash("x/IMG_2.jpg"), &cx).await.unwrap();
    fx.ops
        .restore(first.trash_item_id, policy, &cx)
        .await
        .unwrap();
    let second = fx.ops.trash_file(trash("x/IMG_2.jpg"), &cx).await.unwrap();
    fx.ops.purge(second.trash_item_id, &cx).await.unwrap();
    fx.ops.mkdir_all(loc(fx.a, "new/folders")).await.unwrap();

    // What fails, and what is left behind by a crash.
    let too_long = format!("y/{}.jpg", "n".repeat(300));
    let failures = [
        request(
            loc(fx.a, "x/missing.jpg"),
            loc(fx.a, "y/missing.jpg"),
            policy,
        ),
        request(loc(fx.a, "x/link.jpg"), loc(fx.a, "y/link.jpg"), policy),
        request(
            loc(fx.a, "x/IMG_3.jpg"),
            loc(fx.a, ".apm-trash/b/IMG_3.jpg"),
            policy,
        ),
        request(loc(fx.a, "x/IMG_3.jpg"), loc(fx.a, &too_long), policy),
        request(loc(fx.a, "x/IMG_3.jpg"), loc(9999, "y/IMG_3.jpg"), policy),
    ];
    let mut messages = Vec::new();
    for failing in failures {
        let err = fx.ops.move_file(failing, &cx).await.unwrap_err();
        messages.push(format!("{err} | {err:?} | {}", err.code()));
    }
    trap.arm(Step::AfterJournal, 1);
    let crashed = fx
        .ops
        .move_file(
            request(loc(fx.a, "x/IMG_4.jpg"), loc(fx.a, "y/IMG_4.jpg"), policy),
            &cx,
        )
        .await;
    assert!(crashed.is_err() && !trap.armed());
    let report = fx.restarted().reconcile().await.unwrap();
    assert_eq!(report.abandoned, 1, "{report:?}");
    messages.push(format!("{report:?}"));
    let with_reason = fx
        .scalar("SELECT COUNT(*) FROM fs_journal WHERE error IS NOT NULL")
        .await;
    assert!(
        with_reason >= 1,
        "the probe has an `error` column to look at"
    );
    let known = columns_containing(&fx, "IMG_4").await;
    assert!(
        known.contains(&"fs_journal.src_path".to_owned()),
        "the probe is blind: it does not find a stored name ({known:?})"
    );

    let mut roots = Vec::new();
    for tree in [&fx.ta, &fx.tb] {
        roots.push(tree.root().to_string_lossy().into_owned());
        roots.push(
            fs::canonicalize(tree.root())
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
    }
    roots.push(outside.path().to_string_lossy().into_owned());
    for root in &roots {
        assert_eq!(
            columns_containing(&fx, root).await,
            Vec::<String>::new(),
            "{root}"
        );
        for message in &messages {
            assert!(!message.contains(root.as_str()), "{message} names {root}");
        }
    }
}
