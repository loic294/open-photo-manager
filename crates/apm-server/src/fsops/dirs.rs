//! Folders: `mkdir_all` and the junk-aware `remove_dir_if_removable`, plus the checks and the
//! untracked folder creation every other operation uses.
//!
//! A folder is only ever removed by [`FsOps::remove_dir_if_removable`], and only when everything
//! in it is junk. The junk is TRASHED first (never deleted), so even a wrong guess about what is
//! junk is recoverable.

use std::fs;
use std::io;

use apm_core::conflict::unique_within;
use apm_core::consts::TRASH_DIR;
use apm_core::junk::{Entry, JunkRules};
use apm_core::paths::RelPath;

use super::context::{OpContext, TrashBatch};
use super::error::FsOpError;
use super::failpoints::Step;
use super::journal::{NewEntry, Op};
use super::trash::TrashSpec;
use super::{FsOps, Loc, Rules, blocking};
use crate::db::volumes::PROBE_PREFIX;
use crate::security::{FsError, VolumeRoot};

/// What `remove_dir_if_removable` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RmdirOutcome {
    /// The folder is gone; `junk_trashed` junk entries were trashed on the way.
    Removed { junk_trashed: u64 },
    /// The folder did not exist any more.
    AlreadyGone,
}

/// Synology's own folders: the scanner skips them, but they are somebody's data, so a folder that
/// holds one is never removable.
fn is_protected_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("#recycle") || name.eq_ignore_ascii_case("#snapshot")
}

/// Whether `rel` is the trash folder or something inside it.
pub(crate) fn in_trash(rel: &RelPath) -> bool {
    rel.components()
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case(TRASH_DIR))
}

fn check_components(max_len: usize, rel: &RelPath) -> Result<(), FsOpError> {
    for name in rel.components() {
        if name.len() > max_len {
            return Err(FsOpError::NameTooLong);
        }
        if name.starts_with(PROBE_PREFIX) {
            return Err(FsOpError::DestinationInvalid("that name is reserved"));
        }
    }
    Ok(())
}

/// Checks a path a FILE is about to be created at: not the root, not in the trash, no component
/// longer than `name_max_len`, and no name the scanner would ignore (the file would vanish from
/// the index). `junk` is the rule set to judge names by: a restore passes
/// [`JunkRules::reserved_only`], because the file was there before.
pub(crate) fn validate_file_path(
    rules: &Rules,
    junk: &JunkRules,
    rel: &RelPath,
) -> Result<(), FsOpError> {
    if rel.is_root() {
        return Err(FsOpError::DestinationInvalid(
            "the volume root is not a file location",
        ));
    }
    check_components(rules.name_max_len(), rel)?;
    if in_trash(rel) {
        return Err(FsOpError::DestinationInvalid(
            "the trash is not a destination",
        ));
    }
    if junk.is_junk_path(rel) {
        return Err(FsOpError::DestinationInvalid(
            "the scanner ignores that name",
        ));
    }
    Ok(())
}

/// Like [`validate_file_path`] for a folder.
fn validate_dir_path(rules: &Rules, rel: &RelPath) -> Result<(), FsOpError> {
    check_components(rules.name_max_len(), rel)?;
    if in_trash(rel) {
        return Err(FsOpError::DestinationInvalid(
            "the trash is not a destination",
        ));
    }
    if rel.components().any(|name| rules.junk.is_junk_dir(name)) {
        return Err(FsOpError::DestinationInvalid(
            "the scanner ignores that folder name",
        ));
    }
    Ok(())
}

/// Creates `rel` and every missing folder above it, one component at a time, never following a
/// link. Returns the folders it created, shallowest first. Idempotent and not tracked in the
/// index: the caller decides whether the folders need `dirs` rows.
pub(crate) fn make_dirs(root: &VolumeRoot, rel: &RelPath) -> Result<Vec<RelPath>, FsOpError> {
    if root.resolve_dir(rel).is_ok() {
        return Ok(Vec::new());
    }
    let mut created = Vec::new();
    let mut current = RelPath::root();
    for name in rel.components() {
        current = current
            .join(name)
            .map_err(|_| FsOpError::DestinationInvalid("not a valid folder name"))?;
        match root.resolve_for_create(&current) {
            Ok(path) => match fs::create_dir(&path) {
                Ok(()) => created.push(current.clone()),
                // Somebody else created it first: it only has to be a folder.
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    verify_dir(root, &current)?;
                }
                Err(err) => return Err(FsOpError::from(err)),
            },
            Err(FsError::AlreadyExists(_)) => verify_dir(root, &current)?,
            Err(err) => return Err(FsOpError::from_destination(err)),
        }
    }
    Ok(created)
}

fn verify_dir(root: &VolumeRoot, rel: &RelPath) -> Result<(), FsOpError> {
    match root.resolve_dir(rel) {
        Ok(_) => Ok(()),
        Err(FsError::NotADirectory(_)) => Err(FsOpError::DestinationInvalid(
            "a file is in the way of a folder",
        )),
        Err(err) => Err(FsOpError::from_destination(err)),
    }
}

/// The first name in `dir` that nothing uses: `name` itself, or the first free numbered variant
/// that fits `max_bytes`. A link at a name takes it, as does anything else.
pub(crate) fn free_name(
    root: &VolumeRoot,
    dir: &RelPath,
    name: &str,
    max_bytes: usize,
) -> Result<String, FsOpError> {
    free_name_with(root, dir, name, max_bytes, &|_| Vec::new())
}

/// [`free_name`] for a file that brings companions along (its sidecars): a candidate is only
/// free when every name `companions` derives from it is free as well, so the whole group can
/// take the number together.
pub(crate) fn free_name_with(
    root: &VolumeRoot,
    dir: &RelPath,
    name: &str,
    max_bytes: usize,
    companions: &dyn Fn(&str) -> Vec<String>,
) -> Result<String, FsOpError> {
    let mut failure: Option<FsOpError> = None;
    let found = unique_within(name, max_bytes, |candidate| {
        if failure.is_some() {
            return true;
        }
        let mut names = vec![candidate.to_owned()];
        names.extend(companions(candidate));
        for other in &names {
            match is_taken(root, dir, other) {
                Ok(false) => {}
                Ok(true) => return true,
                Err(err) => {
                    failure = Some(err);
                    return true;
                }
            }
        }
        false
    });
    if let Some(err) = failure {
        return Err(err);
    }
    found.map_err(|_| FsOpError::DestinationInvalid("no free name was found"))
}

/// Whether `name` is used in `dir`: by anything at all, a link included.
fn is_taken(root: &VolumeRoot, dir: &RelPath, name: &str) -> Result<bool, FsOpError> {
    let rel = dir
        .join(name)
        .map_err(|_| FsOpError::DestinationInvalid("not a valid file name"))?;
    match root.resolve_for_create(&rel) {
        Ok(_) => Ok(false),
        Err(FsError::AlreadyExists(_)) => Ok(true),
        Err(FsError::SymlinkRejected(at)) if at == rel => Ok(true),
        Err(err) => Err(FsOpError::from_destination(err)),
    }
}

/// What a folder holds, as far as removing it is concerned.
#[derive(Debug, PartialEq, Eq)]
enum Contents {
    /// The folder does not exist.
    Gone,
    /// Something that is not junk is in it.
    Content,
    /// Only junk (possibly nothing): the names to trash.
    OnlyJunk(Vec<String>),
}

/// Lists a folder without following links. Symbolic links, special files, names that are not
/// UTF-8 and the Synology folders are content.
fn list_contents(
    root: &VolumeRoot,
    rel: &RelPath,
    junk: &JunkRules,
) -> Result<Contents, FsOpError> {
    let path = match root.resolve_dir(rel) {
        Ok(path) => path,
        Err(FsError::NotFound(_)) => return Ok(Contents::Gone),
        Err(FsError::NotADirectory(at)) if &at != rel => return Ok(Contents::Gone),
        Err(FsError::NotADirectory(_)) => {
            return Err(FsOpError::DestinationInvalid("that is not a folder"));
        }
        Err(err) => return Err(FsOpError::from_source(err)),
    };
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Contents::Gone),
        Err(err) => return Err(FsOpError::from(err)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Ok(Contents::Content);
        };
        let kind = entry.file_type()?;
        let plain = kind.is_dir() || kind.is_file();
        let is_junk = plain
            && !is_protected_name(&name)
            && junk.is_junk_entry(Entry {
                name: &name,
                is_dir: kind.is_dir(),
            });
        if !is_junk {
            return Ok(Contents::Content);
        }
        names.push(name);
    }
    names.sort();
    Ok(Contents::OnlyJunk(names))
}

impl FsOps {
    /// Creates `loc` and its missing parents and gives them their `dirs` rows. Returns the
    /// folders that did not exist, shallowest first. Nothing is journaled: a crash leaves at worst
    /// an empty folder the next scan picks up. Callers that show the folder publish their own
    /// `fs_change` (this takes no context).
    ///
    /// # Errors
    /// `destination_invalid` for the trash, a name the scanner ignores or a file in the way,
    /// `name_too_long`, `symlink_rejected`, `volume_read_only`, `volume_offline`.
    pub async fn mkdir_all(&self, loc: Loc) -> Result<Vec<RelPath>, FsOpError> {
        let rules = self.rules();
        let root = self.writable_volume(loc.volume_id)?;
        if loc.rel.is_root() {
            return Ok(Vec::new());
        }
        validate_dir_path(&rules, &loc.rel)?;
        let rel = loc.rel.clone();
        let created = blocking(move || make_dirs(&root, &rel)).await?;
        self.apply_mkdir(loc.volume_id, &loc.rel).await?;
        let mut touched = Vec::new();
        for dir in &created {
            touched.push(dir.clone());
            touched.push(dir.parent().unwrap_or_else(RelPath::root));
        }
        self.note_dirs(loc.volume_id, &touched);
        Ok(created)
    }

    /// Removes the folder when everything in it is junk (`.DS_Store`, `@eaDir`, `Thumbs.db`, ...
    /// by the settings): the junk entries are moved to the trash batch first, then the folder is
    /// removed. A folder that holds anything else is left exactly as it is (`not_empty`). Folders
    /// are never removed any other way.
    ///
    /// # Errors
    /// `not_empty` (nothing was touched, except junk trashed before a racing writer put
    /// content in), `destination_invalid` for the volume root, the trash or a file,
    /// `symlink_rejected`, `volume_read_only`, `volume_offline`, `cancelled`.
    pub async fn remove_dir_if_removable(
        &self,
        loc: Loc,
        batch: &TrashBatch,
        cx: &OpContext,
    ) -> Result<RmdirOutcome, FsOpError> {
        cx.ensure_active()?;
        let rules = self.rules();
        let root = self.writable_volume(loc.volume_id)?;
        if loc.rel.is_root() {
            return Err(FsOpError::DestinationInvalid(
                "the volume root cannot be removed",
            ));
        }
        if in_trash(&loc.rel) {
            return Err(FsOpError::DestinationInvalid("the trash cannot be removed"));
        }

        let contents = {
            let (root, rel, junk) = (root.clone(), loc.rel.clone(), rules.junk.clone());
            blocking(move || list_contents(&root, &rel, &junk)).await?
        };
        let names = match contents {
            Contents::Gone => return Ok(RmdirOutcome::AlreadyGone),
            Contents::Content => return Err(FsOpError::NotEmpty),
            Contents::OnlyJunk(names) => names,
        };

        let mut junk_trashed = 0;
        for name in &names {
            let child = loc.child(name)?;
            self.trash_core(
                TrashSpec {
                    loc: &child,
                    expect: None,
                    batch,
                    reason: "junk",
                },
                cx,
            )
            .await?;
            junk_trashed += 1;
        }

        cx.ensure_active()?;
        let guard = self
            .journal
            .begin(NewEntry {
                task_id: cx.task_id,
                op: Op::Rmdir,
                src: Some(&loc),
                dst: None,
                size: None,
            })
            .await?;
        let journal_id = guard.id();
        let removed = async {
            self.check(Step::AfterJournal)?;
            let (root, rel) = (root.clone(), loc.rel.clone());
            blocking(move || remove_empty_dir(&root, &rel)).await
        }
        .await;
        if let Err(err) = removed {
            if !err.is_abort() {
                self.journal.fail(journal_id, &err).await;
            }
            return Err(err);
        }
        self.check(Step::AfterFinalRename)?;
        self.apply_rmdir(journal_id, loc.volume_id, &loc.rel)
            .await?;
        self.announce(cx, &[(loc.volume_id, loc.dir())]);
        Ok(RmdirOutcome::Removed { junk_trashed })
    }
}

/// The one place a folder is removed. `remove_dir` only succeeds on an EMPTY folder, so a file
/// that arrived since the listing is never lost. A folder that is already gone counts as removed.
fn remove_empty_dir(root: &VolumeRoot, rel: &RelPath) -> Result<(), FsOpError> {
    let path = match root.resolve_dir(rel) {
        Ok(path) => path,
        Err(FsError::NotFound(_)) => return Ok(()),
        Err(FsError::NotADirectory(at)) if &at != rel => return Ok(()),
        Err(FsError::NotADirectory(_)) => {
            return Err(FsOpError::DestinationInvalid("that is not a folder"));
        }
        Err(err) => return Err(FsOpError::from_source(err)),
    };
    match fs::remove_dir(&path) {
        Ok(()) => Ok(()),
        Err(err) => match err.kind() {
            io::ErrorKind::NotFound => Ok(()),
            io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists => {
                Err(FsOpError::NotEmpty)
            }
            _ => Err(FsOpError::from(err)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::test_support::scratch;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    fn root_of(dir: &tempfile::TempDir) -> VolumeRoot {
        VolumeRoot::new(1, dir.path()).unwrap()
    }

    #[test]
    fn make_dirs_creates_the_missing_chain_once() {
        let dir = scratch("make-dirs");
        let root = root_of(&dir);
        let created = make_dirs(&root, &rel("a/b/c")).unwrap();
        assert_eq!(created, vec![rel("a"), rel("a/b"), rel("a/b/c")]);
        assert!(dir.path().join("a/b/c").is_dir());
        assert!(make_dirs(&root, &rel("a/b/c")).unwrap().is_empty());
        let more = make_dirs(&root, &rel("a/b/d/e")).unwrap();
        assert_eq!(more, vec![rel("a/b/d"), rel("a/b/d/e")]);
    }

    #[test]
    fn make_dirs_refuses_a_file_in_the_way() {
        let dir = scratch("make-dirs-file");
        let root = root_of(&dir);
        fs::write(dir.path().join("a"), b"x").unwrap();
        assert_eq!(
            make_dirs(&root, &rel("a/b")),
            Err(FsOpError::DestinationInvalid(
                "a file is in the way of a folder"
            ))
        );
    }

    #[test]
    fn make_dirs_never_follows_a_link() {
        let dir = scratch("make-dirs-link");
        let root = root_of(&dir);
        let outside = scratch("make-dirs-outside");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert_eq!(
            make_dirs(&root, &rel("link/inner")),
            Err(FsOpError::SymlinkRejected)
        );
        assert!(!outside.path().join("inner").exists());
    }

    #[test]
    fn free_names_are_numbered_and_links_count_as_taken() {
        let dir = scratch("free-name");
        let root = root_of(&dir);
        fs::write(dir.path().join("a.jpg"), b"1").unwrap();
        fs::write(dir.path().join("a_1.jpg"), b"2").unwrap();
        std::os::unix::fs::symlink("nowhere", dir.path().join("a_2.jpg")).unwrap();
        assert_eq!(
            free_name(&root, &RelPath::root(), "b.jpg", 255).unwrap(),
            "b.jpg"
        );
        assert_eq!(
            free_name(&root, &RelPath::root(), "a.jpg", 255).unwrap(),
            "a_3.jpg"
        );
    }

    #[test]
    fn a_link_in_the_parent_chain_is_an_error_not_a_free_name() {
        let dir = scratch("free-name-link");
        let root = root_of(&dir);
        let outside = scratch("free-name-outside");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert_eq!(
            free_name(&root, &rel("link"), "a.jpg", 255),
            Err(FsOpError::SymlinkRejected)
        );
    }

    fn rules() -> Rules {
        let settings = std::sync::Arc::new(crate::settings::Settings::defaults_for(1, 2));
        Rules {
            junk: JunkRules::new(&settings.junk_names, &settings.junk_prefixes),
            matcher: apm_core::sidecar::SidecarMatcher::new(&settings.sidecar_extensions),
            settings,
        }
    }

    #[test]
    fn new_file_paths_are_checked() {
        let rules = rules();
        let junk = rules.junk.clone();
        assert!(validate_file_path(&rules, &junk, &rel("a/b.jpg")).is_ok());
        for bad in [
            ".apm-trash/x.jpg",
            ".APM-TRASH/b/x.jpg",
            "a/@eaDir/x.jpg",
            "a/.DS_Store",
            "a/._x.jpg",
            "a/x.jpg.apm-part",
            "a/.apm-probe-1-2",
        ] {
            assert!(
                matches!(
                    validate_file_path(&rules, &junk, &rel(bad)),
                    Err(FsOpError::DestinationInvalid(_))
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            validate_file_path(&rules, &junk, &RelPath::root()),
            Err(FsOpError::DestinationInvalid(_))
        ));
        let long = format!("a/{}.jpg", "x".repeat(300));
        assert_eq!(
            validate_file_path(&rules, &junk, &rel(&long)),
            Err(FsOpError::NameTooLong)
        );
    }

    #[test]
    fn a_restore_may_use_a_configured_junk_name_but_never_the_reserved_ones() {
        let rules = rules();
        let reserved = JunkRules::reserved_only();
        assert!(validate_file_path(&rules, &reserved, &rel("a/.DS_Store")).is_ok());
        assert!(validate_file_path(&rules, &reserved, &rel("a/x.apm-part")).is_err());
        assert!(validate_file_path(&rules, &reserved, &rel(".apm-trash/x")).is_err());
    }

    #[test]
    fn new_folder_paths_are_checked() {
        let rules = rules();
        assert!(validate_dir_path(&rules, &rel("a/b")).is_ok());
        for bad in [".apm-trash/b", "a/@eaDir", "a/.DS_Store/b", "a/.apm-probe-x"] {
            assert!(validate_dir_path(&rules, &rel(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn listing_tells_junk_from_content() {
        let dir = scratch("list-contents");
        let root = root_of(&dir);
        let junk = JunkRules::default();

        assert_eq!(
            list_contents(&root, &rel("missing"), &junk).unwrap(),
            Contents::Gone
        );

        fs::create_dir(dir.path().join("empty")).unwrap();
        assert_eq!(
            list_contents(&root, &rel("empty"), &junk).unwrap(),
            Contents::OnlyJunk(Vec::new())
        );

        fs::create_dir_all(dir.path().join("junky/@eaDir")).unwrap();
        fs::write(dir.path().join("junky/.DS_Store"), b"x").unwrap();
        fs::write(dir.path().join("junky/._x.jpg"), b"x").unwrap();
        assert_eq!(
            list_contents(&root, &rel("junky"), &junk).unwrap(),
            Contents::OnlyJunk(vec![
                "._x.jpg".to_owned(),
                ".DS_Store".to_owned(),
                "@eaDir".to_owned()
            ])
        );

        fs::write(dir.path().join("junky/photo.jpg"), b"x").unwrap();
        assert_eq!(
            list_contents(&root, &rel("junky"), &junk).unwrap(),
            Contents::Content
        );
    }

    #[test]
    fn links_synology_folders_and_sub_folders_are_content() {
        let dir = scratch("list-content-kinds");
        let root = root_of(&dir);
        let junk = JunkRules::default();

        fs::create_dir_all(dir.path().join("a/#recycle")).unwrap();
        assert_eq!(
            list_contents(&root, &rel("a"), &junk).unwrap(),
            Contents::Content
        );

        fs::create_dir_all(dir.path().join("b/#snapshot")).unwrap();
        assert_eq!(
            list_contents(&root, &rel("b"), &junk).unwrap(),
            Contents::Content
        );

        fs::create_dir_all(dir.path().join("c/sub")).unwrap();
        assert_eq!(
            list_contents(&root, &rel("c"), &junk).unwrap(),
            Contents::Content
        );

        fs::create_dir(dir.path().join("d")).unwrap();
        std::os::unix::fs::symlink("nowhere", dir.path().join("d/.DS_Store")).unwrap();
        assert_eq!(
            list_contents(&root, &rel("d"), &junk).unwrap(),
            Contents::Content,
            "a link named like junk is still a link"
        );
    }

    #[test]
    fn a_file_where_a_folder_was_expected_is_an_error() {
        let dir = scratch("list-file");
        let root = root_of(&dir);
        fs::write(dir.path().join("f"), b"x").unwrap();
        let junk = JunkRules::default();
        assert!(matches!(
            list_contents(&root, &rel("f"), &junk),
            Err(FsOpError::DestinationInvalid(_))
        ));
        assert_eq!(
            list_contents(&root, &rel("f/inside"), &junk).unwrap(),
            Contents::Gone
        );
    }

    #[test]
    fn only_an_empty_folder_is_removed() {
        let dir = scratch("remove-empty");
        let root = root_of(&dir);
        fs::create_dir(dir.path().join("a")).unwrap();
        fs::write(dir.path().join("a/x"), b"x").unwrap();
        assert_eq!(remove_empty_dir(&root, &rel("a")), Err(FsOpError::NotEmpty));
        assert!(dir.path().join("a/x").exists());
        fs::remove_file(dir.path().join("a/x")).unwrap();
        remove_empty_dir(&root, &rel("a")).unwrap();
        assert!(!dir.path().join("a").exists());
        remove_empty_dir(&root, &rel("a")).unwrap();
    }
}
