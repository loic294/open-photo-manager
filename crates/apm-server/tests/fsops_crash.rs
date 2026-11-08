//! Crash recovery of moves, copies and replacements.
//!
//! Each scenario kills one operation at each of the steps it has (a failpoint aborts it and
//! cleans nothing up, which is what a killed process leaves behind), restarts (a fresh `FsOps`
//! reconciles the journal) and checks what recovery made of it. `recover` makes the checks that
//! hold for every crash: no journal row stays pending, no part file is left, no content is lost
//! or invented, a second recovery finds nothing to do, and the index equals the disk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;

use Step::{
    AfterCopy, AfterFinalRename, AfterFsync, AfterJournal, AfterPartCreate, AfterVerify,
    BeforeIndexTx, BeforeUnlink,
};
use apm_core::dto::common::ConflictPolicy;
use apm_core::hash::sha1_bytes;
use apm_server::fsops::{FsOpError, OpContext, ReconcileReport, Step};
use common::fsops::Fate::{Done, Partial, Undone, Unreached};
use common::fsops::{Case, Census, Fate, Fx, Scenario, crash_at_every_step, loc, request};

const PAYLOAD: &[u8] = b"the bytes of one photograph";
const OLD: &[u8] = b"the file that gets replaced";

fn cx() -> OpContext {
    OpContext::detached()
}

fn digest(content: &[u8]) -> Option<Vec<u8>> {
    Some(sha1_bytes(content).to_vec())
}

/// `before` with one more copy of `content`.
fn one_more(before: &Census, content: &[u8]) -> Census {
    let mut census = before.clone();
    *census.entry(content.to_vec()).or_insert(0) += 1;
    census
}

// --- a move that is a rename ---------------------------------------------------------------------

struct RenameMove;

impl Scenario for RenameMove {
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
        let request = request(
            loc(fx.a, "x/IMG_1.jpg"),
            loc(fx.b, "y/z/IMG_1.jpg"),
            ConflictPolicy::Smart,
        );
        fx.ops.move_file(request, &cx()).await.map(drop)
    }

    async fn verify(fx: &Fx, id: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            assert_eq!(fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, &label).await, *id);
            fx.gone(fx.b, "y/z/IMG_1.jpg", &label).await;
            fx.assert_journal(&[("move", "abandoned")], &label).await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
        } else {
            fx.gone(fx.a, "x/IMG_1.jpg", &label).await;
            let moved = fx.present(fx.b, "y/z/IMG_1.jpg", PAYLOAD, &label).await;
            assert_eq!(moved, *id, "{label}: the row follows the file");
            fx.assert_journal(&[("move", "done")], &label).await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn a_rename_move_survives_a_crash_at_every_step() {
    crash_at_every_step::<RenameMove>().await;
}

// --- a move that copies, verifies and deletes ----------------------------------------------------

struct CrossMove;

impl Scenario for CrossMove {
    type State = i64;
    const SPLIT: bool = true;
    const MATRIX: &'static [(Step, usize, Fate)] = &[
        (AfterJournal, 1, Undone),
        (AfterPartCreate, 1, Undone),
        (AfterCopy, 1, Undone),
        (AfterFsync, 1, Undone),
        (AfterVerify, 1, Undone),
        (AfterFinalRename, 1, Done),
        (BeforeUnlink, 1, Done),
        (BeforeIndexTx, 1, Done),
    ];

    async fn arrange(fx: &Fx) -> i64 {
        RenameMove::arrange(fx).await
    }

    async fn perform(fx: &Fx, state: &i64) -> Result<(), FsOpError> {
        RenameMove::perform(fx, state).await
    }

    async fn verify(fx: &Fx, id: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            assert_eq!(fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, &label).await, *id);
            fx.gone(fx.b, "y/z/IMG_1.jpg", &label).await;
            fx.assert_journal(&[("move", "abandoned")], &label).await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
            let part_made = usize::from(case.step != AfterJournal);
            assert_eq!(report.parts_removed, part_made, "{label}");
        } else {
            fx.gone(fx.a, "x/IMG_1.jpg", &label).await;
            let moved = fx.present(fx.b, "y/z/IMG_1.jpg", PAYLOAD, &label).await;
            assert_eq!(moved, *id, "{label}: the row follows the file");
            assert_eq!(
                fx.hash_of(moved).await,
                digest(PAYLOAD),
                "{label}: the hash"
            );
            fx.assert_journal(&[("move", "done")], &label).await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
            assert_eq!(report.parts_removed, 0, "{label}");
        }
    }
}

#[tokio::test]
async fn a_cross_device_move_survives_a_crash_at_every_step() {
    crash_at_every_step::<CrossMove>().await;
}

/// The same move with the setting that keeps the source in the trash.
struct CrossMoveKeeping;

impl Scenario for CrossMoveKeeping {
    type State = i64;
    const SPLIT: bool = true;
    const MATRIX: &'static [(Step, usize, Fate)] = CrossMove::MATRIX;

    async fn arrange(fx: &Fx) -> i64 {
        fx.keep_cross_volume_sources_in_trash().await;
        RenameMove::arrange(fx).await
    }

    async fn perform(fx: &Fx, state: &i64) -> Result<(), FsOpError> {
        RenameMove::perform(fx, state).await
    }

    fn census(before: &Census, fate: Fate) -> Census {
        if fate == Done || fate == Unreached {
            one_more(before, PAYLOAD)
        } else {
            before.clone()
        }
    }

    async fn verify(fx: &Fx, id: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        if case.fate == Undone {
            assert_eq!(fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, &label).await, *id);
            fx.gone(fx.b, "y/z/IMG_1.jpg", &label).await;
            assert_eq!(fx.trash_items().await, [], "{label}");
            fx.assert_journal(&[("move", "abandoned")], &label).await;
            return;
        }
        fx.gone(fx.a, "x/IMG_1.jpg", &label).await;
        assert_eq!(
            fx.present(fx.b, "y/z/IMG_1.jpg", PAYLOAD, &label).await,
            *id,
            "{label}: the row follows the file"
        );
        fx.assert_journal(&[("move", "done")], &label).await;
        assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");

        let journal_id = fx.scalar("SELECT id FROM fs_journal").await;
        let items = fx.trash_items().await;
        assert_eq!(items.len(), 1, "{label}: {items:?}");
        let item = &items[0];
        assert_eq!(item.volume_id, fx.a, "{label}");
        assert_eq!(item.orig_path, "x/IMG_1.jpg", "{label}");
        assert_eq!(item.reason.as_deref(), Some("moved"), "{label}");
        assert!(!item.is_dir && item.has_meta, "{label}: {item:?}");
        assert!(
            item.batch.ends_with(&format!("-x{journal_id}")),
            "{label}: the batch {} is derived from the journal row",
            item.batch
        );
        assert_eq!(
            fs::read(fx.ta.path(&item.trash_path)).unwrap(),
            PAYLOAD,
            "{label}: the kept source"
        );
    }
}

#[tokio::test]
async fn a_cross_device_move_that_keeps_its_source_survives_a_crash_at_every_step() {
    crash_at_every_step::<CrossMoveKeeping>().await;
}

// --- a copy ---------------------------------------------------------------------------------------

struct CrossCopy;

impl Scenario for CrossCopy {
    type State = i64;
    const SPLIT: bool = true;
    const MATRIX: &'static [(Step, usize, Fate)] = CrossMove::MATRIX;

    async fn arrange(fx: &Fx) -> i64 {
        RenameMove::arrange(fx).await
    }

    async fn perform(fx: &Fx, _: &i64) -> Result<(), FsOpError> {
        let request = request(
            loc(fx.a, "x/IMG_1.jpg"),
            loc(fx.b, "y/z/IMG_1.jpg"),
            ConflictPolicy::Smart,
        );
        fx.ops.copy_file(request, &cx()).await.map(drop)
    }

    fn census(before: &Census, fate: Fate) -> Census {
        if fate == Done || fate == Unreached {
            one_more(before, PAYLOAD)
        } else {
            before.clone()
        }
    }

    async fn verify(fx: &Fx, id: &i64, case: &Case, report: &ReconcileReport) {
        let label = case.label();
        assert_eq!(
            fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, &label).await,
            *id,
            "{label}: the source is never touched"
        );
        if case.fate == Undone {
            fx.gone(fx.b, "y/z/IMG_1.jpg", &label).await;
            fx.assert_journal(&[("copy", "abandoned")], &label).await;
            assert_eq!((report.completed, report.abandoned), (0, 1), "{label}");
            let part_made = usize::from(case.step != AfterJournal);
            assert_eq!(report.parts_removed, part_made, "{label}");
        } else {
            let copy = fx.present(fx.b, "y/z/IMG_1.jpg", PAYLOAD, &label).await;
            assert_ne!(copy, *id, "{label}: the copy is a new row");
            assert_eq!(fx.hash_of(copy).await, digest(PAYLOAD), "{label}: the hash");
            fx.assert_journal(&[("copy", "done")], &label).await;
            assert_eq!((report.completed, report.abandoned), (1, 0), "{label}");
        }
    }
}

#[tokio::test]
async fn a_copy_survives_a_crash_at_every_step() {
    crash_at_every_step::<CrossCopy>().await;
}

// --- replacing a file ---------------------------------------------------------------------------

/// The file being replaced is trashed FIRST (the first journal row), then the move starts (the
/// second): the crash can leave the old file in the trash and the new one where it was.
struct ReplaceRename;

impl ReplaceRename {
    async fn arrange_files(fx: &Fx) -> (i64, i64) {
        fx.ta.write("s/IMG_1.jpg", PAYLOAD);
        fx.ta.write("d/IMG_1.jpg", OLD);
        fx.scan().await;
        (
            fx.row_id(fx.a, "s/IMG_1.jpg").await,
            fx.row_id(fx.a, "d/IMG_1.jpg").await,
        )
    }
}

impl Scenario for ReplaceRename {
    /// The row ids of the new file and of the one it replaces.
    type State = (i64, i64);
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

    async fn arrange(fx: &Fx) -> (i64, i64) {
        Self::arrange_files(fx).await
    }

    async fn perform(fx: &Fx, _: &(i64, i64)) -> Result<(), FsOpError> {
        let request = request(
            loc(fx.a, "s/IMG_1.jpg"),
            loc(fx.a, "d/IMG_1.jpg"),
            ConflictPolicy::Replace,
        );
        fx.ops.move_file(request, &cx()).await.map(drop)
    }

    async fn verify(fx: &Fx, ids: &(i64, i64), case: &Case, report: &ReconcileReport) {
        verify_replacement(fx, ids, case, report, true).await;
    }
}

/// The checks both replacements share. `rename` is whether the move is a rename (the journal
/// then holds the trash row before the move row).
async fn verify_replacement(
    fx: &Fx,
    (new, old): &(i64, i64),
    case: &Case,
    report: &ReconcileReport,
    rename: bool,
) {
    let label = case.label();
    let (src_vol, src, dst_vol, dst) = if rename {
        (fx.a, "s/IMG_1.jpg", fx.a, "d/IMG_1.jpg")
    } else {
        (fx.a, "IMG_1.jpg", fx.b, "IMG_1.jpg")
    };
    // How recovery closed the rows: the journal, then rows completed, rows abandoned and parts
    // removed by recovery (a row that finished before the crash is not recovery's).
    let (journal, completed, abandoned, parts): (Vec<(&str, &str)>, usize, usize, usize) =
        match (rename, case.fate, case.step, case.nth) {
            (true, Undone, ..) => (vec![("trash", "abandoned")], 0, 1, 0),
            (true, Partial, AfterJournal, _) => {
                (vec![("trash", "done"), ("move", "abandoned")], 0, 1, 0)
            }
            (true, Partial, ..) => (vec![("trash", "done")], 1, 0, 0),
            (true, Done, ..) => (vec![("trash", "done"), ("move", "done")], 1, 0, 0),
            (false, Undone, AfterJournal, 1) => (vec![("move", "abandoned")], 0, 1, 0),
            (false, Undone, AfterJournal, _) => {
                (vec![("move", "abandoned"), ("trash", "abandoned")], 0, 2, 1)
            }
            (false, Undone, ..) => (vec![("move", "abandoned")], 0, 1, 1),
            (false, Partial, ..) => (vec![("move", "abandoned"), ("trash", "done")], 1, 1, 1),
            (false, Done, ..) => (vec![("move", "done"), ("trash", "done")], 1, 0, 0),
            (_, Unreached, ..) => unreachable!(),
        };
    fx.assert_journal(&journal, &label).await;
    assert_eq!(
        (report.completed, report.abandoned, report.parts_removed),
        (completed, abandoned, parts),
        "{label}: {report:?}"
    );

    match case.fate {
        Undone => {
            assert_eq!(fx.present(src_vol, src, PAYLOAD, &label).await, *new);
            assert_eq!(fx.present(dst_vol, dst, OLD, &label).await, *old);
            assert_eq!(fx.trash_items().await, [], "{label}");
        }
        Partial | Done => {
            let items = fx.trash_items().await;
            assert_eq!(items.len(), 1, "{label}: {items:?}");
            let item = &items[0];
            assert_eq!(item.volume_id, dst_vol, "{label}");
            assert_eq!(item.orig_path, dst, "{label}");
            assert_eq!(
                fs::read(fx.tree(dst_vol).path(&item.trash_path)).unwrap(),
                OLD
            );
            // The reason is not journaled: only a trash that finished before the crash has one.
            let finished_live = matches!(
                (case.step, case.nth),
                (AfterJournal, 2) | (AfterFinalRename, 2) | (BeforeIndexTx, 2) | (BeforeUnlink, _)
            );
            let reason = finished_live.then_some("replaced");
            assert_eq!(item.reason.as_deref(), reason, "{label}");
            if case.fate == Partial {
                assert_eq!(fx.present(src_vol, src, PAYLOAD, &label).await, *new);
                fx.gone(dst_vol, dst, &label).await;
            } else {
                fx.gone(src_vol, src, &label).await;
                let placed = fx.present(dst_vol, dst, PAYLOAD, &label).await;
                assert_eq!(placed, *new, "{label}: the new file keeps its row");
            }
        }
        Unreached => unreachable!(),
    }
}

#[tokio::test]
async fn replacing_a_file_by_rename_survives_a_crash_at_every_step() {
    crash_at_every_step::<ReplaceRename>().await;
}

/// Across devices the old file is trashed only after the copy is verified (the second row), so
/// the crashes before that leave the old file in place.
struct ReplaceCross;

impl Scenario for ReplaceCross {
    type State = (i64, i64);
    const SPLIT: bool = true;
    const MATRIX: &'static [(Step, usize, Fate)] = &[
        (AfterJournal, 1, Undone),
        (AfterJournal, 2, Undone),
        (AfterPartCreate, 1, Undone),
        (AfterCopy, 1, Undone),
        (AfterFsync, 1, Undone),
        (AfterVerify, 1, Undone),
        (AfterFinalRename, 1, Partial),
        (AfterFinalRename, 2, Done),
        (BeforeUnlink, 1, Done),
        (BeforeIndexTx, 1, Partial),
        (BeforeIndexTx, 2, Done),
    ];

    async fn arrange(fx: &Fx) -> (i64, i64) {
        fx.ta.write("IMG_1.jpg", PAYLOAD);
        fx.tb.write("IMG_1.jpg", OLD);
        fx.scan().await;
        (
            fx.row_id(fx.a, "IMG_1.jpg").await,
            fx.row_id(fx.b, "IMG_1.jpg").await,
        )
    }

    async fn perform(fx: &Fx, _: &(i64, i64)) -> Result<(), FsOpError> {
        let request = request(
            loc(fx.a, "IMG_1.jpg"),
            loc(fx.b, "IMG_1.jpg"),
            ConflictPolicy::Replace,
        );
        fx.ops.move_file(request, &cx()).await.map(drop)
    }

    async fn verify(fx: &Fx, ids: &(i64, i64), case: &Case, report: &ReconcileReport) {
        verify_replacement(fx, ids, case, report, false).await;
    }
}

#[tokio::test]
async fn replacing_a_file_across_devices_survives_a_crash_at_every_step() {
    crash_at_every_step::<ReplaceCross>().await;
}
