//! Recovery in the situations a crash and a restart can bring that the crash matrix cannot: a file
//! system that left both names of a rename, somebody else's files in the way, damage to the
//! copy, old part files, a volume that is away, a hook that fails, an operation that is still
//! running while recovery looks at the journal.
//!
//! The rule under test: recovery finishes what the disk proves finished and abandons what the
//! disk proves never started; whatever it cannot tell, it leaves exactly as it found it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::BASE_MTIME;
use common::fsops::{
    Fx, SplitMover, Trap, case_insensitive, crash_fx, loc, permissions_are_enforced, request,
};
use futures_util::future::BoxFuture;
use opm_core::dto::common::ConflictPolicy;
use opm_server::fsops::{
    AppliedOp, FsOpError, Loc, OpContext, OpHook, ReconcileReport, Step, TrashBatch, TrashRequest,
};
use opm_server::indexer::{RunContext, ScanRequest};
use sqlx::SqliteConnection;
use tokio_util::sync::CancellationToken;

const PAYLOAD: &[u8] = b"the bytes of one photograph";
const OTHER: &[u8] = b"the bytes of another one";

fn cx() -> OpContext {
    OpContext::detached()
}

fn now() -> i64 {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since_epoch.as_secs()).unwrap()
}

/// Moves `src` to `dst` on a fixture that crashes the first time `step` is reached.
async fn crash_move(fx: &Fx, trap: &Trap, step: Step, src: Loc, dst: Loc) {
    trap.arm(step, 1);
    let moved = fx
        .ops
        .move_file(request(src, dst, ConflictPolicy::Smart), &cx())
        .await;
    assert_eq!(moved, Err(FsOpError::Aborted));
    assert!(!trap.armed(), "the move never reached {step:?}");
}

/// The folders a report asks to have rescanned, as `(volume, folder, recursive)`.
fn rescan_of(report: &ReconcileReport) -> BTreeSet<(i64, String, bool)> {
    report
        .rescan
        .iter()
        .map(|target| {
            (
                target.volume_id,
                target.dir.as_str().to_owned(),
                target.recursive,
            )
        })
        .collect()
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{what}: it did not happen within ten seconds");
}

// --- two names for one file -----------------------------------------------------------------

/// A rename done as "link, then unlink" (the fallback where the file system has no atomic
/// no-replace rename) that was killed between its two steps leaves two hard links to one file.
/// The destination is the file that was moved; the source name is the redundant one.
#[tokio::test]
async fn a_rename_that_crashed_between_link_and_unlink_is_finished() {
    // (what differs, destination on volume b, destination path)
    let cases = [
        ("another folder", false, "d/IMG_1.jpg"),
        ("another volume", true, "IMG_1.jpg"),
        ("the same folder", false, "s/IMG_2.jpg"),
    ];
    for (case, on_b, dst) in cases {
        let trap = Trap::default();
        let fx = crash_fx(&trap, false).await;
        fx.ta.write("s/IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        let id = fx.row_id(fx.a, "s/IMG_1.jpg").await;
        let volume = if on_b { fx.b } else { fx.a };
        crash_move(
            &fx,
            &trap,
            Step::AfterJournal,
            loc(fx.a, "s/IMG_1.jpg"),
            loc(volume, dst),
        )
        .await;
        fs::hard_link(fx.ta.path("s/IMG_1.jpg"), fx.tree(volume).path(dst)).unwrap();

        let report = fx.restarted().reconcile().await.unwrap();

        assert_eq!(
            (report.completed, report.abandoned),
            (1, 0),
            "{case}: {report:?}"
        );
        fx.gone(fx.a, "s/IMG_1.jpg", case).await;
        assert_eq!(fx.present(volume, dst, PAYLOAD, case).await, id, "{case}");
        assert_eq!(fs::metadata(fx.tree(volume).path(dst)).unwrap().nlink(), 1);
        fx.assert_journal(&[("move", "done")], case).await;
        fx.assert_in_sync_for(case).await;
    }
}

/// On a case-insensitive file system the names `IMG.jpg` and `img.jpg` are one file: after a
/// crash the disk says only which spelling the folder holds.
#[tokio::test]
async fn a_case_only_rename_that_crashed_is_recovered_whichever_spelling_is_left() {
    let probe = common::scratch_dir();
    if !case_insensitive(probe.path()) {
        return;
    }
    // (the step crashed at, the spelling left on the disk)
    for (step, left, completed) in [
        (Step::AfterJournal, "IMG_1.JPG", 0),
        (Step::AfterFinalRename, "img_1.jpg", 1),
    ] {
        let trap = Trap::default();
        let fx = crash_fx(&trap, false).await;
        fx.ta.write("IMG_1.JPG", PAYLOAD);
        fx.scan().await;
        let id = fx.row_id(fx.a, "IMG_1.JPG").await;
        crash_move(
            &fx,
            &trap,
            step,
            loc(fx.a, "IMG_1.JPG"),
            loc(fx.a, "img_1.jpg"),
        )
        .await;

        let report = fx.restarted().reconcile().await.unwrap();

        let label = format!("{step:?}");
        assert_eq!(report.completed, completed, "{label}: {report:?}");
        assert_eq!(report.abandoned, 1 - completed, "{label}: {report:?}");
        assert_eq!(names_in(fx.ta.root()), [left], "{label}");
        assert_eq!(
            fx.paths(fx.a).await,
            BTreeSet::from([left.to_owned()]),
            "{label}"
        );
        assert_eq!(
            fx.scalar("SELECT id FROM files").await,
            id,
            "{label}: the row is kept"
        );
        fx.assert_in_sync_for(&label).await;
    }
}

// --- somebody else's files -------------------------------------------------------------------

/// What a stranger can have put at the destination while the server was down.
#[tokio::test]
async fn whatever_stands_at_the_destination_is_never_replaced_by_recovery() {
    for what in ["a different file", "a folder", "a link"] {
        let trap = Trap::default();
        let fx = crash_fx(&trap, false).await;
        fx.ta.write("s/IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        let id = fx.row_id(fx.a, "s/IMG_1.jpg").await;
        crash_move(
            &fx,
            &trap,
            Step::AfterJournal,
            loc(fx.a, "s/IMG_1.jpg"),
            loc(fx.a, "d/IMG_1.jpg"),
        )
        .await;
        let there = fx.ta.path("d/IMG_1.jpg");
        match what {
            "a different file" => fx.ta.write("d/IMG_1.jpg", OTHER),
            "a folder" => fs::create_dir(&there).unwrap(),
            _ => symlink(fx.ta.path("s/IMG_1.jpg"), &there).unwrap(),
        }

        let report = fx.restarted().reconcile().await.unwrap();

        assert_eq!(
            (report.completed, report.abandoned),
            (0, 1),
            "{what}: {report:?}"
        );
        assert_eq!(
            fs::read(fx.ta.path("s/IMG_1.jpg")).unwrap(),
            PAYLOAD,
            "{what}"
        );
        assert_eq!(fx.row_id(fx.a, "s/IMG_1.jpg").await, id, "{what}");
        match what {
            "a different file" => assert_eq!(fs::read(&there).unwrap(), OTHER),
            "a folder" => assert!(there.is_dir()),
            _ => assert!(fs::symlink_metadata(&there).unwrap().is_symlink()),
        }
        fx.assert_journal(&[("move", "abandoned")], what).await;
        // The index has not learnt what is there: the report hands both folders to the indexer.
        let expected =
            BTreeSet::from([(fx.a, "d".to_owned(), false), (fx.a, "s".to_owned(), false)]);
        assert_eq!(rescan_of(&report), expected, "{what}");
    }
}

#[tokio::test]
async fn a_different_file_left_at_the_destination_is_indexed_by_the_rescan() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    fx.ta.write("s/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    crash_move(
        &fx,
        &trap,
        Step::AfterJournal,
        loc(fx.a, "s/IMG_1.jpg"),
        loc(fx.a, "d/IMG_1.jpg"),
    )
    .await;
    fx.ta.write("d/IMG_1.jpg", OTHER);

    let report = fx.restarted().reconcile().await.unwrap();
    let rescan = ScanRequest {
        targets: report.rescan.clone(),
        ..ScanRequest::default()
    };
    fx.app
        .state
        .indexer
        .scan(&rescan, &RunContext::noop())
        .await
        .unwrap();

    fx.present(fx.a, "s/IMG_1.jpg", PAYLOAD, "the source").await;
    fx.present(fx.a, "d/IMG_1.jpg", OTHER, "the stranger").await;
    fx.assert_in_sync().await;
}

// --- damage ----------------------------------------------------------------------------------

/// The copy was verified and put in place, then the source was about to be deleted when the
/// process died. If the copy is damaged afterwards, the source is the only good one.
#[tokio::test]
async fn a_damaged_copy_is_not_trusted_and_the_source_is_kept() {
    let damages: [(&str, &[u8]); 2] = [
        ("same size", b"the bytes of one photograpH"),
        ("shorter", b"the bytes"),
    ];
    for (what, damaged) in damages {
        assert_ne!(damaged, PAYLOAD);
        let trap = Trap::default();
        let fx = crash_fx(&trap, true).await;
        fx.ta.write("IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        let id = fx.row_id(fx.a, "IMG_1.jpg").await;
        crash_move(
            &fx,
            &trap,
            Step::BeforeUnlink,
            loc(fx.a, "IMG_1.jpg"),
            loc(fx.b, "IMG_1.jpg"),
        )
        .await;
        fs::write(fx.tb.path("IMG_1.jpg"), damaged).unwrap();

        let report = fx.restarted().reconcile().await.unwrap();

        assert_eq!(
            (report.completed, report.abandoned),
            (0, 1),
            "{what}: {report:?}"
        );
        assert_eq!(report.parts_removed, 0, "{what}");
        assert_eq!(
            fs::read(fx.ta.path("IMG_1.jpg")).unwrap(),
            PAYLOAD,
            "{what}"
        );
        assert_eq!(
            fs::read(fx.tb.path("IMG_1.jpg")).unwrap(),
            damaged,
            "{what}"
        );
        assert_eq!(fx.row_id(fx.a, "IMG_1.jpg").await, id, "{what}");
        assert!(fx.row(fx.b, "IMG_1.jpg").await.is_none(), "{what}");
        fx.assert_journal(&[("move", "abandoned")], what).await;
        assert_eq!(
            rescan_of(&report),
            BTreeSet::from([(fx.b, String::new(), false)]),
            "{what}"
        );
    }
}

/// The copy is fine but the source was edited since: deleting it would lose the edit.
#[tokio::test]
async fn a_source_that_changed_since_the_copy_is_kept_with_it() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, true).await;
    fx.ta.write("IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    crash_move(
        &fx,
        &trap,
        Step::BeforeUnlink,
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "IMG_1.jpg"),
    )
    .await;
    fx.ta
        .write("IMG_1.jpg", b"an edited photograph, longer than before");

    let report = fx.restarted().reconcile().await.unwrap();

    assert_eq!((report.completed, report.abandoned), (0, 1), "{report:?}");
    assert_eq!(
        fs::read(fx.ta.path("IMG_1.jpg")).unwrap(),
        b"an edited photograph, longer than before"
    );
    assert_eq!(fs::read(fx.tb.path("IMG_1.jpg")).unwrap(), PAYLOAD);
    let expected = BTreeSet::from([(fx.a, String::new(), false), (fx.b, String::new(), false)]);
    assert_eq!(rescan_of(&report), expected);
}

// --- journal rows recovery cannot make sense of -----------------------------------------------

#[tokio::test]
async fn rows_with_nothing_to_go_on_are_abandoned() {
    let fx = Fx::new().await;
    for op in ["move", "copy", "restore", "trash", "rmdir", "delete"] {
        sqlx::query(
            "INSERT INTO fs_journal (op, state, created_at) VALUES (?, 'pending', 1700000000)",
        )
        .bind(op)
        .execute(fx.app.state.db.write())
        .await
        .unwrap();
    }

    let report = fx.restarted().reconcile().await.unwrap();

    assert_eq!(
        (report.examined, report.completed, report.abandoned),
        (6, 0, 6),
        "{report:?}"
    );
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(fx.pending_rows().await, 0);
}

// --- stray part files -------------------------------------------------------------------------

#[tokio::test]
async fn old_part_files_no_row_owns_are_swept_and_everything_else_stays() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, true).await;
    fx.ta.write("IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    crash_move(
        &fx,
        &trap,
        Step::AfterCopy,
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "d/IMG_1.jpg"),
    )
    .await;
    let first = fx.restarted().reconcile().await.unwrap();
    assert_eq!(
        (first.parts_removed, first.stray_parts_swept),
        (1, 0),
        "{first:?}"
    );

    // Parts a crash lost track of (the journal row never learnt their names), and look-alikes.
    let old = "d/.opm-0123456789abcdef.opm-part";
    let young = "d/.opm-fedcba9876543210.opm-part";
    let not_hex = "d/.opm-0123456789abcdeg.opm-part";
    let too_short = "d/.opm-0123456789abcde.opm-part";
    let theirs = "d/.opm-0123456789abcdef.part";
    fx.tb.write_at(old, b"debris", BASE_MTIME);
    fx.tb
        .write_at(young, b"a copy that is just starting", now());
    for lookalike in [not_hex, too_short, theirs] {
        fx.tb.write_at(lookalike, b"somebody's file", BASE_MTIME);
    }

    let second = fx.restarted().reconcile().await.unwrap();

    assert_eq!(second.stray_parts_swept, 1, "{second:?}");
    assert!(!fx.tb.path(old).exists());
    for kept in [young, not_hex, too_short, theirs] {
        assert!(fx.tb.path(kept).exists(), "{kept} was deleted");
    }
    let again = fx.restarted().reconcile().await.unwrap();
    assert_eq!(again.stray_parts_swept, 0, "{again:?}");
}

#[tokio::test]
async fn parts_are_only_looked_for_in_folders_the_journal_names() {
    let fx = Fx::new().await;
    fx.ta
        .write("elsewhere/.opm-0123456789abcdef.opm-part", b"debris");
    fx.ta.write("IMG_1.jpg", PAYLOAD);
    fx.scan().await;

    let report = fx.restarted().reconcile().await.unwrap();

    assert_eq!(report.stray_parts_swept, 0, "{report:?}");
    assert!(
        fx.ta
            .path("elsewhere/.opm-0123456789abcdef.opm-part")
            .exists()
    );
}

// --- sidecar links ----------------------------------------------------------------------------

/// A crashed group move can leave a link between a file and a sidecar that stayed behind in
/// another folder. Recovery drops it and the owner reads its metadata again.
#[tokio::test]
async fn links_that_cross_folders_are_dropped_and_the_folders_relinked() {
    let fx = Fx::new().await;
    fx.ta.write("d1/IMG_1.jpg", PAYLOAD);
    fx.ta.write("d1/IMG_1.xmp", b"<xmp one/>");
    fx.ta.write("d2/IMG_2.jpg", OTHER);
    fx.ta.write("d2/IMG_2.xmp", b"<xmp two/>");
    fx.scan().await;
    let before = fx.links().await;
    assert_eq!(before.len(), 2, "{before:?}");

    sqlx::query(
        "INSERT INTO file_sidecars (file_id, sidecar_id, kind) \
         SELECT o.id, s.id, 'stem' FROM files o, files s \
         WHERE o.name = 'IMG_1.jpg' AND s.name = 'IMG_2.xmp'",
    )
    .execute(fx.app.state.db.write())
    .await
    .unwrap();
    assert_eq!(fx.links().await.len(), 3);

    let report = fx.restarted().reconcile().await.unwrap();

    assert_eq!(report.links_dropped, 1, "{report:?}");
    assert_eq!(fx.links().await, before);
    let again = fx.restarted().reconcile().await.unwrap();
    assert_eq!(again.links_dropped, 0, "{again:?}");
    fx.assert_in_sync().await;
}

// --- a volume that is away --------------------------------------------------------------------

#[tokio::test]
async fn an_offline_volume_keeps_its_rows_until_it_comes_back() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    fx.ta.write("s/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    crash_move(
        &fx,
        &trap,
        Step::AfterFinalRename,
        loc(fx.a, "s/IMG_1.jpg"),
        loc(fx.a, "d/IMG_1.jpg"),
    )
    .await;

    let root = fx.ta.root().to_path_buf();
    let away = root.with_file_name("a.away");
    fs::rename(&root, &away).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();
    let offline = fx.restarted().reconcile().await.unwrap();
    fs::rename(&away, &root).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();

    let counts = (
        offline.examined,
        offline.still_pending,
        offline.completed,
        offline.abandoned,
    );
    assert_eq!(counts, (1, 1, 0, 0), "{offline:?}");
    assert_eq!(fx.pending_rows().await, 1);

    let back = fx.restarted().reconcile().await.unwrap();
    assert_eq!((back.completed, back.still_pending), (1, 0), "{back:?}");
    fx.present(fx.a, "d/IMG_1.jpg", PAYLOAD, "back online")
        .await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_read_only_volume_keeps_its_rows_until_it_is_writable() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    if !permissions_are_enforced(fx.ta.root()) {
        return;
    }
    fx.ta.write("s/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    crash_move(
        &fx,
        &trap,
        Step::AfterFinalRename,
        loc(fx.a, "s/IMG_1.jpg"),
        loc(fx.a, "d/IMG_1.jpg"),
    )
    .await;

    let root = fx.ta.root().to_path_buf();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();
    let read_only = fx.restarted().reconcile().await.unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();

    assert_eq!(
        (read_only.still_pending, read_only.completed),
        (1, 0),
        "{read_only:?}"
    );
    assert_eq!(fx.pending_rows().await, 1);
    let writable = fx.restarted().reconcile().await.unwrap();
    assert_eq!(
        (writable.completed, writable.still_pending),
        (1, 0),
        "{writable:?}"
    );
    fx.assert_in_sync().await;
}

// --- startup ----------------------------------------------------------------------------------

#[tokio::test]
async fn a_cancelled_startup_recovery_leaves_the_rows_pending() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    fx.ta.write("s/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    crash_move(
        &fx,
        &trap,
        Step::AfterJournal,
        loc(fx.a, "s/IMG_1.jpg"),
        loc(fx.a, "d/IMG_1.jpg"),
    )
    .await;

    let shutdown = CancellationToken::new();
    shutdown.cancel();
    fx.restarted().reconcile_at_startup(shutdown).await;
    assert_eq!(fx.pending_rows().await, 1);

    fx.restarted()
        .reconcile_at_startup(CancellationToken::new())
        .await;
    assert_eq!(fx.pending_rows().await, 0);
    fx.assert_journal(&[("move", "abandoned")], "").await;
}

#[tokio::test]
async fn startup_recovery_indexes_a_restored_folder_in_the_background() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    fx.ta.write("x/a.jpg", PAYLOAD);
    fx.ta.write("x/sub/b.jpg", OTHER);
    fx.scan().await;
    let trashed = fx
        .ops
        .trash_file(
            TrashRequest {
                file_id: None,
                loc: loc(fx.a, "x"),
                expect: None,
                batch: TrashBatch::adhoc(),
                reason: "user".to_owned(),
            },
            &cx(),
        )
        .await
        .unwrap();
    trap.arm(Step::BeforeIndexTx, 1);
    let restored = fx
        .ops
        .restore(trashed.trash_item_id, ConflictPolicy::Smart, &cx())
        .await;
    assert_eq!(restored, Err(FsOpError::Aborted));
    assert!(fx.ta.path("x/sub/b.jpg").exists());
    assert!(fx.row(fx.a, "x/sub/b.jpg").await.is_none());

    fx.restarted()
        .reconcile_at_startup(CancellationToken::new())
        .await;

    eventually("the restored folder is indexed", || async {
        fx.row(fx.a, "x/sub/b.jpg").await.is_some() && fx.row(fx.a, "x/a.jpg").await.is_some()
    })
    .await;
    fx.assert_in_sync().await;
}

// --- hooks ------------------------------------------------------------------------------------

struct Recorder(Mutex<Vec<AppliedOp>>);

impl OpHook for Recorder {
    fn on_applied<'a>(
        &'a self,
        _tx: &'a mut SqliteConnection,
        op: &'a AppliedOp,
    ) -> BoxFuture<'a, Result<(), sqlx::Error>> {
        self.0.lock().unwrap().push(op.clone());
        Box::pin(async { Ok(()) })
    }
}

struct FailsOnce(AtomicBool);

impl OpHook for FailsOnce {
    fn on_applied<'a>(
        &'a self,
        _tx: &'a mut SqliteConnection,
        _op: &'a AppliedOp,
    ) -> BoxFuture<'a, Result<(), sqlx::Error>> {
        let failing = self.0.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if failing {
                Err(sqlx::Error::Protocol("the hook failed".into()))
            } else {
                Ok(())
            }
        })
    }
}

/// A hook that fails rolls the index change back, so the row stays pending and the next pass
/// finishes it; the hooks see the recovered operation exactly once.
#[tokio::test]
async fn a_failing_hook_leaves_the_row_pending_and_the_next_pass_finishes_it() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    fx.ta.write("s/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "s/IMG_1.jpg").await;
    crash_move(
        &fx,
        &trap,
        Step::AfterFinalRename,
        loc(fx.a, "s/IMG_1.jpg"),
        loc(fx.a, "d/IMG_1.jpg"),
    )
    .await;

    let failing = fx
        .restarted()
        .with_hook(Arc::new(FailsOnce(AtomicBool::new(true))));
    let first = failing.reconcile().await.unwrap();
    assert_eq!((first.completed, first.still_pending), (0, 1), "{first:?}");
    assert_eq!(first.errors.len(), 1, "{first:?}");
    assert_eq!(fx.pending_rows().await, 1);
    assert!(
        fx.row(fx.a, "s/IMG_1.jpg").await.is_some(),
        "the index change was rolled back"
    );

    let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
    let second = fx
        .restarted()
        .with_hook(Arc::clone(&recorder) as Arc<dyn OpHook>)
        .reconcile()
        .await
        .unwrap();
    assert_eq!(
        (second.completed, second.still_pending),
        (1, 0),
        "{second:?}"
    );
    assert_eq!(
        *recorder.0.lock().unwrap(),
        [AppliedOp::Moved {
            file_id: id,
            from: loc(fx.a, "s/IMG_1.jpg"),
            to: loc(fx.a, "d/IMG_1.jpg"),
        }]
    );
    assert_eq!(fx.present(fx.a, "d/IMG_1.jpg", PAYLOAD, "").await, id);
    fx.assert_in_sync().await;
}

// --- purging ----------------------------------------------------------------------------------

/// Recovery never deletes a trash item's content: a purge that died halfway through a folder is
/// abandoned with what is left, and purging again finishes it.
#[tokio::test]
async fn an_interrupted_purge_of_a_folder_is_finished_by_purging_again() {
    let trap = Trap::default();
    let fx = crash_fx(&trap, false).await;
    fx.ta.write("x/a.jpg", PAYLOAD);
    fx.ta.write("x/sub/b.jpg", OTHER);
    fx.scan().await;
    let trashed = fx
        .ops
        .trash_file(
            TrashRequest {
                file_id: None,
                loc: loc(fx.a, "x"),
                expect: None,
                batch: TrashBatch::adhoc(),
                reason: "user".to_owned(),
            },
            &cx(),
        )
        .await
        .unwrap();
    trap.arm(Step::AfterJournal, 1);
    assert_eq!(
        fx.ops.purge(trashed.trash_item_id, &cx()).await,
        Err(FsOpError::Aborted)
    );
    // The process died while it was deleting the content.
    fs::remove_file(fx.ta.path(&format!("{}/a.jpg", trashed.trash_path))).unwrap();

    let report = fx.restarted().reconcile().await.unwrap();

    assert_eq!((report.completed, report.abandoned), (0, 1), "{report:?}");
    assert_eq!(fx.trash_items().await.len(), 1);
    assert_eq!(
        fs::read(fx.ta.path(&format!("{}/sub/b.jpg", trashed.trash_path))).unwrap(),
        OTHER
    );

    let purged = fx
        .restarted()
        .purge(trashed.trash_item_id, &cx())
        .await
        .unwrap();
    assert_eq!(purged.files, 1);
    assert_eq!(fx.trash_items().await, []);
    assert!(fx.content_census().is_empty(), "{:?}", fx.content_census());
    fx.assert_in_sync().await;
}

// --- operations that are running ---------------------------------------------------------------

/// Recovery that runs while a copy is in flight (the server is up, a volume came back, an admin
/// triggered it) must not touch the copy's row or its part file, even an old-looking one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_leaves_alone_an_operation_that_is_running() {
    let fx = Fx::with(|ops, trees| ops.with_mover(Arc::new(SplitMover::new(&trees)))).await;
    let big: Vec<u8> = (0..16_u32 * 1024 * 1024)
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    fx.ta.write("big.jpg", &big);
    fx.scan().await;

    let during: Arc<Mutex<Option<ReconcileReport>>> = Arc::default();
    let (ops, handle, root, slot) = (
        fx.ops.clone(),
        tokio::runtime::Handle::current(),
        fx.tb.root().to_path_buf(),
        Arc::clone(&during),
    );
    let context = cx().with_on_bytes(Arc::new(move |_| {
        let mut slot = slot.lock().unwrap();
        if slot.is_some() {
            return;
        }
        // Make the part look like an old crash's debris, so only its owner protects it.
        for name in names_in(&root) {
            if name.starts_with(".opm-") && name.ends_with(".opm-part") {
                let time = UNIX_EPOCH + Duration::from_secs(u64::try_from(BASE_MTIME).unwrap());
                fs::File::options()
                    .write(true)
                    .open(root.join(name))
                    .unwrap()
                    .set_modified(time)
                    .unwrap();
            }
        }
        *slot = Some(handle.block_on(ops.reconcile()).unwrap());
    }));
    let moved = fx
        .ops
        .move_file(
            request(
                loc(fx.a, "big.jpg"),
                loc(fx.b, "big.jpg"),
                ConflictPolicy::Smart,
            ),
            &context,
        )
        .await;
    assert!(moved.is_ok(), "{moved:?}");

    let report = during
        .lock()
        .unwrap()
        .take()
        .expect("recovery ran during the copy");
    let counts = (
        report.examined,
        report.still_pending,
        report.completed,
        report.abandoned,
    );
    assert_eq!(counts, (1, 1, 0, 0), "{report:?}");
    assert_eq!(
        (report.parts_removed, report.stray_parts_swept),
        (0, 0),
        "{report:?}"
    );
    assert_eq!(fs::read(fx.tb.path("big.jpg")).unwrap(), big);
    assert!(!fx.ta.path("big.jpg").exists());
    fx.assert_journal(&[("move", "done")], "").await;
    assert!(fx.leftover_parts().is_empty());
    fx.assert_in_sync().await;
}
