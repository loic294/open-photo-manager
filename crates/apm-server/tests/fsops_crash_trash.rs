//! Crash recovery of trashing, restoring, purging and removing folders.
//!
//! See `fsops_crash.rs`: each operation is killed at each of its steps, the server restarts and
//! recovers, and the disk, the index and the journal must agree with no content lost or invented.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;

use Step::{
    AfterCopy, AfterFinalRename, AfterFsync, AfterJournal, AfterPartCreate, AfterVerify,
    BeforeIndexTx, BeforeUnlink,
};
use apm_core::dto::common::ConflictPolicy;
use apm_server::fsops::{
    FsOpError, Loc, OpContext, ReconcileReport, RmdirOutcome, Step, TrashBatch, TrashRequest,
};
use common::fsops::Fate::{Done, Partial, Undone, Unreached};
use common::fsops::{Case, Census, Fate, Fx, Scenario, crash_at_every_step, loc};

const PAYLOAD: &[u8] = b"the bytes of one photograph";
const OTHER: &[u8] = b"the bytes of another one";
const JUNK: &[u8] = b"finder litter";

fn cx() -> OpContext {
    OpContext::detached()
}

fn trash_request(loc: Loc) -> TrashRequest {
    TrashRequest {
        file_id: None,
        loc,
        expect: None,
        batch: TrashBatch::adhoc(),
        reason: "user".to_owned(),
    }
}

/// `before` without the contents that were purged.
fn without(before: &Census, gone: &[&[u8]]) -> Census {
    let mut census = before.clone();
    for content in gone {
        census.remove(*content);
    }
    census
}

/// The trashed copy of `orig` on volume `a`, as the one `trash_items` row says.
async fn trashed(fx: &Fx, label: &str, orig: &str, is_dir: bool) -> common::fsops::TrashRow {
    let items = fx.trash_items().await;
    assert_eq!(items.len(), 1, "{label}: {items:?}");
    let item = items.into_iter().next().unwrap();
    assert_eq!(
        (item.volume_id, item.orig_path.as_str()),
        (fx.a, orig),
        "{label}"
    );
    assert_eq!(item.is_dir, is_dir, "{label}");
    assert!(
        item.trash_path.starts_with(".apm-trash/") && item.trash_path.ends_with(orig),
        "{label}: {}",
        item.trash_path
    );
    assert!(item.trash_path.contains(&item.batch), "{label}: {item:?}");
    item
}

// --- trashing ------------------------------------------------------------------------------------

struct TrashFile;

impl Scenario for TrashFile {
    type State = i64;
    const MATRIX: &'static [(Step, usize, Fate)] = &[
        (AfterJournal, 1, Undone),
        (AfterPartCreate, 1, Unreached),
        (AfterCopy, 1, Unreached),
        (AfterFsync, 1, Unreached),
        (AfterVerify, 1, Unreached),
        (AfterFinalRename, 1, Done),
        (BeforeUnlink, 1, Unreached),
        (BeforeIndexTx, 1, Done),
    ];

    async fn arrange(fx: &Fx) -> i64 {
        fx.ta.write("x/IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        fx.row_id(fx.a, "x/IMG_1.jpg").await
    }

    async fn perform(fx: &Fx, _: &i64) -> Result<(), FsOpError> {
        let request = trash_request(loc(fx.a, "x/IMG_1.jpg"));
        fx.ops.trash_file(request, &cx()).await.map(drop)
    }

    async fn verify(fx: &Fx, id: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            assert_eq!(fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, &label).await, *id);
            assert_eq!(fx.trash_items().await, [], "{label}");
            fx.assert_journal(&[("trash", "abandoned")], &label).await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
        } else {
            fx.gone(fx.a, "x/IMG_1.jpg", &label).await;
            let item = trashed(fx, &label, "x/IMG_1.jpg", false).await;
            // The reason is not journaled, so a trash recovery finishes has none.
            assert_eq!(item.reason, None, "{label}");
            assert!(
                item.has_meta,
                "{label}: the row's metadata is kept for a restore"
            );
            assert_eq!(fs::read(fx.ta.path(&item.trash_path)).unwrap(), PAYLOAD);
            fx.assert_journal(&[("trash", "done")], &label).await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn trashing_a_file_survives_a_crash_at_every_step() {
    crash_at_every_step::<TrashFile>().await;
}

struct TrashDir;

impl Scenario for TrashDir {
    type State = ();
    const MATRIX: &'static [(Step, usize, Fate)] = TrashFile::MATRIX;

    async fn arrange(fx: &Fx) {
        fx.ta.write("x/a.jpg", PAYLOAD);
        fx.ta.write("x/sub/b.jpg", OTHER);
        fx.scan().await;
        assert!(fx.dir_row(fx.a, "x/sub").await);
    }

    async fn perform(fx: &Fx, (): &()) -> Result<(), FsOpError> {
        let request = trash_request(loc(fx.a, "x"));
        fx.ops.trash_file(request, &cx()).await.map(drop)
    }

    async fn verify(fx: &Fx, (): &(), case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            fx.present(fx.a, "x/a.jpg", PAYLOAD, &label).await;
            fx.present(fx.a, "x/sub/b.jpg", OTHER, &label).await;
            assert!(fx.dir_row(fx.a, "x").await && fx.dir_row(fx.a, "x/sub").await);
            assert_eq!(fx.trash_items().await, [], "{label}");
            fx.assert_journal(&[("trash", "abandoned")], &label).await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
        } else {
            fx.gone(fx.a, "x/a.jpg", &label).await;
            fx.gone(fx.a, "x/sub/b.jpg", &label).await;
            assert!(!fx.dir_row(fx.a, "x").await && !fx.dir_row(fx.a, "x/sub").await);
            assert!(!fx.ta.path("x").exists(), "{label}");
            let item = trashed(fx, &label, "x", true).await;
            assert_eq!(item.reason, None, "{label}");
            let inside = fx.ta.path(&item.trash_path);
            assert_eq!(fs::read(inside.join("a.jpg")).unwrap(), PAYLOAD, "{label}");
            assert_eq!(
                fs::read(inside.join("sub/b.jpg")).unwrap(),
                OTHER,
                "{label}"
            );
            fx.assert_journal(&[("trash", "done")], &label).await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn trashing_a_folder_survives_a_crash_at_every_step() {
    crash_at_every_step::<TrashDir>().await;
}

// --- restoring -----------------------------------------------------------------------------------

struct RestoreFile;

impl Scenario for RestoreFile {
    /// The trash item.
    type State = i64;
    const MATRIX: &'static [(Step, usize, Fate)] = TrashFile::MATRIX;

    async fn arrange(fx: &Fx) -> i64 {
        fx.ta.write("x/IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        let request = trash_request(loc(fx.a, "x/IMG_1.jpg"));
        fx.ops
            .trash_file(request, &cx())
            .await
            .unwrap()
            .trash_item_id
    }

    async fn perform(fx: &Fx, item: &i64) -> Result<(), FsOpError> {
        fx.ops
            .restore(*item, ConflictPolicy::Smart, &cx())
            .await
            .map(drop)
    }

    async fn verify(fx: &Fx, item: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            fx.gone(fx.a, "x/IMG_1.jpg", &label).await;
            let kept = trashed(fx, &label, "x/IMG_1.jpg", false).await;
            assert_eq!(kept.id, *item, "{label}");
            assert_eq!(fs::read(fx.ta.path(&kept.trash_path)).unwrap(), PAYLOAD);
            fx.assert_journal(&[("trash", "done"), ("restore", "abandoned")], &label)
                .await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
        } else {
            fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, &label).await;
            assert_eq!(fx.trash_items().await, [], "{label}: the item is gone");
            fx.assert_journal(&[("trash", "done"), ("restore", "done")], &label)
                .await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn restoring_a_file_survives_a_crash_at_every_step() {
    crash_at_every_step::<RestoreFile>().await;
}

struct RestoreDir;

impl Scenario for RestoreDir {
    type State = i64;
    const MATRIX: &'static [(Step, usize, Fate)] = TrashFile::MATRIX;

    async fn arrange(fx: &Fx) -> i64 {
        fx.ta.write("x/a.jpg", PAYLOAD);
        fx.ta.write("x/sub/b.jpg", OTHER);
        fx.scan().await;
        let request = trash_request(loc(fx.a, "x"));
        fx.ops
            .trash_file(request, &cx())
            .await
            .unwrap()
            .trash_item_id
    }

    async fn perform(fx: &Fx, item: &i64) -> Result<(), FsOpError> {
        fx.ops
            .restore(*item, ConflictPolicy::Smart, &cx())
            .await
            .map(drop)
    }

    async fn verify(fx: &Fx, item: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            assert!(!fx.ta.path("x").exists(), "{label}");
            let kept = trashed(fx, &label, "x", true).await;
            assert_eq!(kept.id, *item, "{label}");
            fx.assert_journal(&[("trash", "done"), ("restore", "abandoned")], &label)
                .await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
        } else {
            // The folder's content is indexed by the rescan the report asks for.
            assert!(!report.rescan.is_empty(), "{label}: {report:?}");
            fx.present(fx.a, "x/a.jpg", PAYLOAD, &label).await;
            fx.present(fx.a, "x/sub/b.jpg", OTHER, &label).await;
            assert!(fx.dir_row(fx.a, "x/sub").await, "{label}");
            assert_eq!(fx.trash_items().await, [], "{label}: the item is gone");
            fx.assert_journal(&[("trash", "done"), ("restore", "done")], &label)
                .await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn restoring_a_folder_survives_a_crash_at_every_step() {
    crash_at_every_step::<RestoreDir>().await;
}

// --- purging -------------------------------------------------------------------------------------

/// Purge has two steps only: after the journal row and before the index change.
const PURGE_MATRIX: &[(Step, usize, Fate)] = &[
    (AfterJournal, 1, Undone),
    (AfterPartCreate, 1, Unreached),
    (AfterCopy, 1, Unreached),
    (AfterFsync, 1, Unreached),
    (AfterVerify, 1, Unreached),
    (AfterFinalRename, 1, Unreached),
    (BeforeUnlink, 1, Unreached),
    (BeforeIndexTx, 1, Done),
];

struct PurgeFile;

impl Scenario for PurgeFile {
    type State = i64;
    const MATRIX: &'static [(Step, usize, Fate)] = PURGE_MATRIX;

    async fn arrange(fx: &Fx) -> i64 {
        RestoreFile::arrange(fx).await
    }

    async fn perform(fx: &Fx, item: &i64) -> Result<(), FsOpError> {
        fx.ops.purge(*item, &cx()).await.map(drop)
    }

    fn census(before: &Census, fate: Fate) -> Census {
        if fate == Done || fate == Unreached {
            without(before, &[PAYLOAD])
        } else {
            before.clone()
        }
    }

    async fn verify(fx: &Fx, item: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            let kept = trashed(fx, &label, "x/IMG_1.jpg", false).await;
            assert_eq!(kept.id, *item, "{label}");
            assert_eq!(fs::read(fx.ta.path(&kept.trash_path)).unwrap(), PAYLOAD);
            fx.assert_journal(&[("trash", "done"), ("delete", "abandoned")], &label)
                .await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
        } else {
            assert_eq!(fx.trash_items().await, [], "{label}");
            fx.gone(fx.a, "x/IMG_1.jpg", &label).await;
            fx.assert_journal(&[("trash", "done"), ("delete", "done")], &label)
                .await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn purging_a_file_survives_a_crash_at_every_step() {
    crash_at_every_step::<PurgeFile>().await;
}

struct PurgeDir;

impl Scenario for PurgeDir {
    type State = i64;
    const MATRIX: &'static [(Step, usize, Fate)] = PURGE_MATRIX;

    async fn arrange(fx: &Fx) -> i64 {
        RestoreDir::arrange(fx).await
    }

    async fn perform(fx: &Fx, item: &i64) -> Result<(), FsOpError> {
        fx.ops.purge(*item, &cx()).await.map(drop)
    }

    fn census(before: &Census, fate: Fate) -> Census {
        if fate == Done || fate == Unreached {
            without(before, &[PAYLOAD, OTHER])
        } else {
            before.clone()
        }
    }

    async fn verify(fx: &Fx, item: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            let kept = trashed(fx, &label, "x", true).await;
            assert_eq!(kept.id, *item, "{label}");
            let inside = fx.ta.path(&kept.trash_path);
            assert_eq!(fs::read(inside.join("a.jpg")).unwrap(), PAYLOAD, "{label}");
            assert_eq!(
                fs::read(inside.join("sub/b.jpg")).unwrap(),
                OTHER,
                "{label}"
            );
            fx.assert_journal(&[("trash", "done"), ("delete", "abandoned")], &label)
                .await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
        } else {
            assert_eq!(fx.trash_items().await, [], "{label}");
            fx.assert_journal(&[("trash", "done"), ("delete", "done")], &label)
                .await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn purging_a_folder_survives_a_crash_at_every_step() {
    crash_at_every_step::<PurgeDir>().await;
}

// --- removing a folder ---------------------------------------------------------------------------

struct RmdirEmpty;

impl Scenario for RmdirEmpty {
    type State = ();
    const MATRIX: &'static [(Step, usize, Fate)] = TrashFile::MATRIX;

    async fn arrange(fx: &Fx) {
        fx.ta.mkdir("empty");
        fx.scan().await;
        assert!(fx.dir_row(fx.a, "empty").await);
    }

    async fn perform(fx: &Fx, (): &()) -> Result<(), FsOpError> {
        let outcome = fx
            .ops
            .remove_dir_if_removable(loc(fx.a, "empty"), &TrashBatch::adhoc(), &cx())
            .await?;
        assert_eq!(outcome, RmdirOutcome::Removed { junk_trashed: 0 });
        Ok(())
    }

    async fn verify(fx: &Fx, (): &(), case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            assert!(fx.ta.path("empty").is_dir(), "{label}");
            assert!(fx.dir_row(fx.a, "empty").await, "{label}");
            fx.assert_journal(&[("rmdir", "abandoned")], &label).await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
        } else {
            assert!(!fx.ta.path("empty").exists(), "{label}");
            assert!(!fx.dir_row(fx.a, "empty").await, "{label}");
            fx.assert_journal(&[("rmdir", "done")], &label).await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn removing_an_empty_folder_survives_a_crash_at_every_step() {
    crash_at_every_step::<RmdirEmpty>().await;
}

/// A folder with junk in it: the junk is trashed first (its own three steps), then the folder is
/// removed (three more).
struct RmdirJunk;

impl Scenario for RmdirJunk {
    type State = ();
    const MATRIX: &'static [(Step, usize, Fate)] = &[
        (AfterJournal, 1, Undone),
        (AfterJournal, 2, Partial),
        (AfterPartCreate, 1, Unreached),
        (AfterCopy, 1, Unreached),
        (AfterFsync, 1, Unreached),
        (AfterVerify, 1, Unreached),
        (AfterFinalRename, 1, Partial),
        (AfterFinalRename, 2, Done),
        (BeforeUnlink, 1, Unreached),
        (BeforeIndexTx, 1, Partial),
        (BeforeIndexTx, 2, Done),
    ];

    async fn arrange(fx: &Fx) {
        fx.ta.write("junk/.DS_Store", JUNK);
        fx.scan().await;
        assert!(fx.dir_row(fx.a, "junk").await);
    }

    async fn perform(fx: &Fx, (): &()) -> Result<(), FsOpError> {
        let outcome = fx
            .ops
            .remove_dir_if_removable(loc(fx.a, "junk"), &TrashBatch::adhoc(), &cx())
            .await?;
        assert_eq!(outcome, RmdirOutcome::Removed { junk_trashed: 1 });
        Ok(())
    }

    async fn verify(fx: &Fx, (): &(), case: &Case, report: &ReconcileReport) {
        let label = case.label();
        let first_trash = case.nth == 1;
        let crashed_before_the_disk_changed = case.step == AfterJournal;
        // What the junk's trash item says it was trashed for: only the run that was not killed
        // knows ("junk"); a recovery cannot tell.
        let (journal, junk, folder, settled): (&[(&str, &str)], _, _, _) =
            match (crashed_before_the_disk_changed, first_trash) {
                (true, true) => (&[("trash", "abandoned")], None, true, (0, 1)),
                (true, false) => (
                    &[("trash", "done"), ("rmdir", "abandoned")],
                    Some("junk"),
                    true,
                    (0, 1),
                ),
                (false, true) => (&[("trash", "done")], Some(""), true, (1, 0)),
                (false, false) => (
                    &[("trash", "done"), ("rmdir", "done")],
                    Some("junk"),
                    false,
                    (1, 0),
                ),
            };

        assert_eq!(fx.ta.path("junk").is_dir(), folder, "{label}");
        assert_eq!(fx.dir_row(fx.a, "junk").await, folder, "{label}");
        match junk {
            None => {
                assert_eq!(fs::read(fx.ta.path("junk/.DS_Store")).unwrap(), JUNK);
                assert_eq!(fx.trash_items().await, [], "{label}");
            }
            Some(reason) => {
                assert!(!fx.ta.path("junk/.DS_Store").exists(), "{label}");
                let item = trashed(fx, &label, "junk/.DS_Store", false).await;
                assert_eq!(item.reason.as_deref().unwrap_or(""), reason, "{label}");
                assert_eq!(fs::read(fx.ta.path(&item.trash_path)).unwrap(), JUNK);
            }
        }
        fx.assert_journal(journal, &label).await;
        assert_eq!((report.completed, report.abandoned), settled, "{label}");
    }
}

#[tokio::test]
async fn removing_a_folder_with_junk_survives_a_crash_at_every_step() {
    crash_at_every_step::<RmdirJunk>().await;
}
