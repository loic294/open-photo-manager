//! Moving and copying single files: the happy paths, the conflict policies, the refusals, the
//! cross-device copy-verify-delete path and what the index, the hooks and the change events see.
//!
//! The two volumes are directories of one file system, so a [`SplitMover`] makes a rename between
//! them fail the way a real cross-device rename does. Nothing here depends on the case or the
//! normalisation behaviour of the file system, except where a test checks it first.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::BASE_MTIME;
use common::fsops::{
    FixedFreeSpace, Fx, SplitMover, case_insensitive, loc, permissions_are_enforced, request,
};
use futures_util::future::BoxFuture;
use opm_core::dto::common::ConflictPolicy;
use opm_core::hash::sha1_bytes;
use opm_server::fsops::{
    AppliedOp, Expect, FailAction, Failpoints, MoveOutcome, OpContext, OpHook, SkipReason, Step,
};
use opm_server::indexer::RecordingChanges;
use sqlx::SqliteConnection;

fn cx() -> OpContext {
    OpContext::detached()
}

/// A fixture whose volumes `a` and `b` are on different "devices".
async fn two_devices() -> Fx {
    Fx::with(|ops, trees| ops.with_mover(Arc::new(SplitMover::new(&trees)))).await
}

fn moved(outcome: &MoveOutcome) -> (&opm_server::fsops::Loc, bool, bool) {
    match outcome {
        MoveOutcome::Moved {
            dst,
            renamed,
            cross_device,
            ..
        } => (dst, *renamed, *cross_device),
        MoveOutcome::Skipped(reason) => panic!("expected a move, got {reason:?}"),
    }
}

// --- the happy paths ---------------------------------------------------------------------------

#[tokio::test]
async fn resumed_transfers_refuse_a_deleted_and_reused_destination_identity() {
    let fx = Fx::new().await;
    fx.ta.write("source.jpg", b"one");
    fx.scan().await;
    let original = fx.row_id(fx.a, "source.jpg").await;
    let id = fx
        .app
        .state
        .tasks
        .submit(
            opm_server::tasks::TaskSpec::new(opm_core::dto::TaskKind::FsMove, "Recovery receipt")
                .resumable(),
            |_| async { Ok(serde_json::json!({})) },
        )
        .await
        .unwrap();
    fx.app.state.tasks.wait_finished(id).await.unwrap();
    let mut context = cx();
    context.task_id = Some(id);
    let make_request = || {
        request(
            loc(fx.a, "source.jpg"),
            loc(fx.a, "destination.jpg"),
            ConflictPolicy::Rename,
        )
    };
    fx.ops.move_file(make_request(), &context).await.unwrap();
    let replayed = fx.ops.move_file(make_request(), &context).await.unwrap();
    assert_eq!(moved(&replayed).0, &loc(fx.a, "destination.jpg"));
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(original)
        .execute(fx.app.state.db.write())
        .await
        .unwrap();
    fx.scan().await;
    assert_eq!(
        fx.row_id(fx.a, "destination.jpg").await,
        original,
        "SQLite reused the id"
    );
    let outcome = fx
        .ops
        .move_file(make_request(), &context)
        .await
        .unwrap_err();
    assert_eq!(outcome.code(), "source_changed");
    assert_eq!(fs::read(fx.ta.path("destination.jpg")).unwrap(), b"one");
}

#[tokio::test]
async fn a_move_keeps_the_row_and_the_index_matches_the_disk() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", b"one");
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;

    let request = request(
        loc(fx.a, "x/IMG_1.jpg"),
        loc(fx.a, "y/z/IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    let outcome = fx.ops.move_file(request, &cx()).await.unwrap();

    assert_eq!(
        outcome,
        MoveOutcome::Moved {
            dst: loc(fx.a, "y/z/IMG_1.jpg"),
            renamed: false,
            cross_device: false,
            bytes: 3,
            file_id: id,
        }
    );
    assert!(!fx.ta.path("x/IMG_1.jpg").exists());
    assert_eq!(fs::read(fx.ta.path("y/z/IMG_1.jpg")).unwrap(), b"one");
    assert_eq!(fx.row_id(fx.a, "y/z/IMG_1.jpg").await, id);
    assert!(fx.row(fx.a, "x/IMG_1.jpg").await.is_none());
    fx.assert_in_sync().await;
    assert_eq!(fx.journal().await, [("move".into(), "done".into())]);
}

#[tokio::test]
async fn a_move_to_another_volume_on_the_same_device_is_a_rename() {
    let fx = Fx::new().await;
    fx.ta.write("IMG_1.jpg", b"one");
    fx.scan().await;
    let id = fx.row_id(fx.a, "IMG_1.jpg").await;

    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "in/IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    let outcome = fx.ops.move_file(request, &cx()).await.unwrap();

    assert_eq!(moved(&outcome), (&loc(fx.b, "in/IMG_1.jpg"), false, false));
    assert_eq!(fx.row_id(fx.b, "in/IMG_1.jpg").await, id);
    assert_eq!(fx.scalar("SELECT COUNT(*) FROM files").await, 1);
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_move_across_devices_copies_verifies_and_deletes_the_source() {
    let fx = two_devices().await;
    fx.ta.write("IMG_1.jpg", b"payload");
    fs::set_permissions(fx.ta.path("IMG_1.jpg"), fs::Permissions::from_mode(0o640)).unwrap();
    fx.scan().await;
    let id = fx.row_id(fx.a, "IMG_1.jpg").await;

    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "in/IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    let outcome = fx.ops.move_file(request, &cx()).await.unwrap();

    assert_eq!(
        outcome,
        MoveOutcome::Moved {
            dst: loc(fx.b, "in/IMG_1.jpg"),
            renamed: false,
            cross_device: true,
            bytes: 7,
            file_id: id,
        }
    );
    assert!(!fx.ta.path("IMG_1.jpg").exists());
    let copy = fx.tb.path("in/IMG_1.jpg");
    assert_eq!(fs::read(&copy).unwrap(), b"payload");
    let meta = fs::metadata(&copy).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o640);
    assert_eq!(
        fx.row(fx.b, "in/IMG_1.jpg").await,
        Some((id, 7, BASE_MTIME))
    );
    let hash: Option<Vec<u8>> = sqlx::query_scalar("SELECT hash FROM files WHERE id = ?")
        .bind(id)
        .fetch_one(fx.app.state.db.read())
        .await
        .unwrap();
    assert_eq!(
        hash,
        Some(sha1_bytes(b"payload").to_vec()),
        "the verified hash is adopted"
    );
    assert!(fx.leftover_parts().is_empty());
    fx.assert_in_sync().await;
    assert_eq!(fx.journal().await, [("move".into(), "done".into())]);
}

#[tokio::test]
async fn a_cross_device_move_can_keep_the_source_in_the_trash() {
    let fx = two_devices().await;
    let mut settings = (*fx.app.state.settings.get()).clone();
    settings.keep_cross_volume_source_in_trash = true;
    fx.app.state.settings.update(settings).await.unwrap();
    fx.ta.write("deep/IMG_1.jpg", b"payload");
    fx.scan().await;

    let request = request(
        loc(fx.a, "deep/IMG_1.jpg"),
        loc(fx.b, "IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    fx.ops.move_file(request, &cx()).await.unwrap();

    let kept: Vec<_> = common::fsops::disk_files(fx.ta.root())
        .into_iter()
        .filter(|(path, _)| path.starts_with(".opm-trash/") && path.ends_with("/deep/IMG_1.jpg"))
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "the original stays in the trash with its layout"
    );
    assert_eq!(kept[0].1, b"payload");
    let items: i64 = fx
        .scalar("SELECT COUNT(*) FROM trash_items WHERE reason = 'moved'")
        .await;
    assert_eq!(items, 1);
    assert_eq!(fx.paths(fx.a).await.len(), 0, "the trash is not indexed");
    assert_eq!(fx.paths(fx.b).await.len(), 1);
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_copy_makes_a_new_row_and_leaves_the_source_alone() {
    let fx = Fx::new().await;
    fx.ta.write("IMG_1.jpg", b"payload");
    fx.scan().await;
    let source_id = fx.row_id(fx.a, "IMG_1.jpg").await;

    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "copies/IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    let outcome = fx.ops.copy_file(request, &cx()).await.unwrap();

    let MoveOutcome::Moved {
        file_id,
        cross_device,
        bytes,
        ..
    } = outcome
    else {
        panic!("expected a copy");
    };
    assert_ne!(file_id, source_id);
    assert!(!cross_device, "a copy never reports a device crossing");
    assert_eq!(bytes, 7);
    assert_eq!(fs::read(fx.ta.path("IMG_1.jpg")).unwrap(), b"payload");
    assert_eq!(
        fs::read(fx.tb.path("copies/IMG_1.jpg")).unwrap(),
        b"payload"
    );
    assert_eq!(fx.row_id(fx.a, "IMG_1.jpg").await, source_id);
    assert_eq!(
        fx.row(fx.b, "copies/IMG_1.jpg").await,
        Some((file_id, 7, BASE_MTIME))
    );
    fx.assert_in_sync().await;
    assert_eq!(fx.journal().await, [("copy".into(), "done".into())]);
}

#[tokio::test]
async fn a_case_only_rename_changes_the_name() {
    let fx = Fx::new().await;
    fx.ta.write("IMG_1.jpg", b"one");
    fx.scan().await;
    let id = fx.row_id(fx.a, "IMG_1.jpg").await;
    println!(
        "case-insensitive file system: {}",
        case_insensitive(fx.ta.root())
    );

    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.a, "img_1.jpg"),
        ConflictPolicy::Smart,
    );
    let outcome = fx.ops.move_file(request, &cx()).await.unwrap();

    assert_eq!(moved(&outcome), (&loc(fx.a, "img_1.jpg"), false, false));
    let names: Vec<_> = common::fsops::disk_files(fx.ta.root())
        .into_keys()
        .collect();
    assert_eq!(names, ["img_1.jpg"]);
    assert_eq!(fx.row_id(fx.a, "img_1.jpg").await, id);
    fx.assert_in_sync().await;
}

// --- conflicts ---------------------------------------------------------------------------------

/// `s/IMG_1.jpg` (content `new`) and `d/IMG_1.jpg` (content `old`, or `new` when `same`).
async fn conflict(same: bool) -> Fx {
    let fx = Fx::new().await;
    fx.ta.write("s/IMG_1.jpg", b"new");
    fx.ta
        .write("d/IMG_1.jpg", if same { b"new" } else { b"old" });
    fx.scan().await;
    fx
}

async fn move_into_d(fx: &Fx, policy: ConflictPolicy) -> MoveOutcome {
    let request = request(loc(fx.a, "s/IMG_1.jpg"), loc(fx.a, "d/IMG_1.jpg"), policy);
    fx.ops.move_file(request, &cx()).await.unwrap()
}

#[tokio::test]
async fn smart_skips_an_identical_destination() {
    let fx = conflict(true).await;
    let outcome = move_into_d(&fx, ConflictPolicy::Smart).await;
    assert_eq!(
        outcome,
        MoveOutcome::Skipped(SkipReason::IdenticalAtDestination)
    );
    assert!(
        fx.ta.path("s/IMG_1.jpg").exists(),
        "the source is untouched"
    );
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn smart_numbers_a_different_destination() {
    let fx = conflict(false).await;
    let outcome = move_into_d(&fx, ConflictPolicy::Smart).await;
    assert_eq!(moved(&outcome), (&loc(fx.a, "d/IMG_1_1.jpg"), true, false));
    assert_eq!(fs::read(fx.ta.path("d/IMG_1.jpg")).unwrap(), b"old");
    assert_eq!(fs::read(fx.ta.path("d/IMG_1_1.jpg")).unwrap(), b"new");
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn skip_leaves_everything_where_it_is() {
    let fx = conflict(false).await;
    let outcome = move_into_d(&fx, ConflictPolicy::Skip).await;
    assert_eq!(outcome, MoveOutcome::Skipped(SkipReason::DestinationExists));
    assert!(fx.ta.path("s/IMG_1.jpg").exists());
    assert_eq!(fs::read(fx.ta.path("d/IMG_1.jpg")).unwrap(), b"old");
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn rename_numbers_even_an_identical_destination() {
    let fx = conflict(true).await;
    let outcome = move_into_d(&fx, ConflictPolicy::Rename).await;
    assert_eq!(moved(&outcome), (&loc(fx.a, "d/IMG_1_1.jpg"), true, false));
    assert!(fx.ta.path("d/IMG_1.jpg").exists());
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn replace_trashes_the_existing_file_and_takes_its_place() {
    let fx = conflict(false).await;
    let old_id = fx.row_id(fx.a, "d/IMG_1.jpg").await;
    let new_id = fx.row_id(fx.a, "s/IMG_1.jpg").await;
    let census = fx.content_census();

    let outcome = move_into_d(&fx, ConflictPolicy::Replace).await;

    assert_eq!(moved(&outcome), (&loc(fx.a, "d/IMG_1.jpg"), false, false));
    assert_eq!(fs::read(fx.ta.path("d/IMG_1.jpg")).unwrap(), b"new");
    assert_eq!(fx.row_id(fx.a, "d/IMG_1.jpg").await, new_id);
    assert_eq!(
        fx.scalar(&format!("SELECT COUNT(*) FROM files WHERE id = {old_id}"))
            .await,
        0
    );
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM trash_items WHERE reason = 'replaced'")
            .await,
        1
    );
    assert_eq!(
        fx.content_census(),
        census,
        "the replaced file is in the trash, not gone"
    );
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn replace_across_devices_copies_first_and_trashes_the_old_file() {
    let fx = two_devices().await;
    fx.ta.write("IMG_1.jpg", b"new");
    fx.tb.write("IMG_1.jpg", b"old");
    fx.scan().await;
    let census = fx.content_census();

    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "IMG_1.jpg"),
        ConflictPolicy::Replace,
    );
    let outcome = fx.ops.move_file(request, &cx()).await.unwrap();

    assert_eq!(moved(&outcome), (&loc(fx.b, "IMG_1.jpg"), false, true));
    assert_eq!(fs::read(fx.tb.path("IMG_1.jpg")).unwrap(), b"new");
    assert!(!fx.ta.path("IMG_1.jpg").exists());
    assert_eq!(fx.content_census(), census);
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM trash_items WHERE reason = 'replaced'")
            .await,
        1
    );
    assert!(fx.leftover_parts().is_empty());
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_copy_onto_a_different_file_with_replace_trashes_it_too() {
    let fx = conflict(false).await;
    let request = request(
        loc(fx.a, "s/IMG_1.jpg"),
        loc(fx.a, "d/IMG_1.jpg"),
        ConflictPolicy::Replace,
    );
    fx.ops.copy_file(request, &cx()).await.unwrap();

    assert_eq!(fs::read(fx.ta.path("d/IMG_1.jpg")).unwrap(), b"new");
    assert_eq!(fs::read(fx.ta.path("s/IMG_1.jpg")).unwrap(), b"new");
    assert_eq!(
        fx.scalar("SELECT COUNT(*) FROM trash_items WHERE reason = 'replaced'")
            .await,
        1
    );
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn moving_a_file_onto_itself_changes_nothing() {
    let fx = Fx::new().await;
    fx.ta.write("IMG_1.jpg", b"one");
    fx.scan().await;
    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.a, "IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    let outcome = fx.ops.move_file(request, &cx()).await.unwrap();
    assert_eq!(outcome, MoveOutcome::Skipped(SkipReason::SameLocation));
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn racing_moves_to_one_name_never_overwrite_each_other() {
    let fx = Fx::new().await;
    for n in 0..8 {
        fx.ta
            .write(&format!("s/{n}.jpg"), format!("photo {n}").as_bytes());
    }
    fx.scan().await;
    let census = fx.content_census();

    let mut tasks = Vec::new();
    for n in 0..8 {
        let ops = fx.ops.clone();
        let request = request(
            loc(fx.a, &format!("s/{n}.jpg")),
            loc(fx.a, "d/IMG.jpg"),
            ConflictPolicy::Rename,
        );
        tasks.push(tokio::spawn(async move {
            ops.move_file(request, &OpContext::detached()).await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    let landed = common::fsops::disk_files(fx.ta.root())
        .into_keys()
        .filter(|path| path.starts_with("d/"))
        .count();
    assert_eq!(landed, 8, "every file got its own name");
    assert_eq!(fx.content_census(), census);
    fx.assert_in_sync().await;
}

// --- refusals ----------------------------------------------------------------------------------

async fn move_code(fx: &Fx, src: &str, dst: &str) -> &'static str {
    let request = request(loc(fx.a, src), loc(fx.a, dst), ConflictPolicy::Smart);
    fx.ops
        .move_file(request, &cx())
        .await
        .expect_err("the move must be refused")
        .code()
}

#[tokio::test]
async fn refusals_touch_nothing() {
    let fx = Fx::new().await;
    fx.ta.write("IMG_1.jpg", b"one");
    fx.ta.mkdir("folder");
    fx.ta.write("real/keep.jpg", b"keep");
    symlink(fx.ta.path("IMG_1.jpg"), fx.ta.path("link.jpg")).unwrap();
    symlink(fx.ta.path("real"), fx.ta.path("dirlink")).unwrap();
    fx.scan().await;
    let census = fx.content_census();
    let before = common::fsops::disk_files(fx.ta.root());

    assert_eq!(
        move_code(&fx, "missing.jpg", "x.jpg").await,
        "source_missing"
    );
    assert_eq!(
        move_code(&fx, "no/such/dir/x.jpg", "x.jpg").await,
        "source_missing"
    );
    assert_eq!(
        move_code(&fx, "folder", "x.jpg").await,
        "not_a_regular_file"
    );
    assert_eq!(
        move_code(&fx, "link.jpg", "x.jpg").await,
        "symlink_rejected"
    );
    assert_eq!(
        move_code(&fx, "IMG_1.jpg", "dirlink/x.jpg").await,
        "symlink_rejected"
    );
    assert_eq!(
        move_code(&fx, "dirlink/keep.jpg", "x.jpg").await,
        "symlink_rejected"
    );
    assert_eq!(
        move_code(&fx, "IMG_1.jpg", ".DS_Store").await,
        "destination_invalid"
    );
    assert_eq!(
        move_code(&fx, "IMG_1.jpg", ".opm-trash/x.jpg").await,
        "destination_invalid"
    );
    let long = format!("{}.jpg", "n".repeat(252));
    assert_eq!(move_code(&fx, "IMG_1.jpg", &long).await, "name_too_long");

    assert_eq!(common::fsops::disk_files(fx.ta.root()), before);
    assert_eq!(fx.content_census(), census);
    assert!(fx.leftover_parts().is_empty());
    fx.assert_in_sync().await;
    assert_eq!(fx.pending_rows().await, 0);
}

#[tokio::test]
async fn a_source_that_changed_since_the_plan_is_not_touched() {
    let fx = Fx::new().await;
    fx.ta.write("IMG_1.jpg", b"one");
    fx.scan().await;
    let mut stale = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.a, "moved.jpg"),
        ConflictPolicy::Smart,
    );
    stale.expect = Some(Expect {
        size: 99,
        mtime: BASE_MTIME,
    });

    let err = fx.ops.move_file(stale.clone(), &cx()).await.unwrap_err();
    assert_eq!(err.code(), "source_changed");
    assert!(fx.ta.path("IMG_1.jpg").exists() && !fx.ta.path("moved.jpg").exists());

    let current = fx.ops.stat(&loc(fx.a, "IMG_1.jpg")).await.unwrap();
    stale.expect = Some(Expect {
        size: current.size,
        mtime: current.mtime,
    });
    fx.ops.move_file(stale, &cx()).await.unwrap();
    assert!(fx.ta.path("moved.jpg").exists());
}

#[tokio::test]
async fn a_read_only_volume_is_refused() {
    let fx = Fx::new().await;
    if !permissions_are_enforced(fx.ta.root()) {
        return;
    }
    fx.ta.write("IMG_1.jpg", b"one");
    fx.tb.write("IMG_2.jpg", b"two");
    fx.scan().await;

    let root = fx.tb.root().to_path_buf();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();
    let into_it = fx
        .ops
        .move_file(
            request(
                loc(fx.a, "IMG_1.jpg"),
                loc(fx.b, "IMG_1.jpg"),
                ConflictPolicy::Smart,
            ),
            &cx(),
        )
        .await;
    let out_of_it = fx
        .ops
        .move_file(
            request(
                loc(fx.b, "IMG_2.jpg"),
                loc(fx.a, "IMG_2.jpg"),
                ConflictPolicy::Smart,
            ),
            &cx(),
        )
        .await;
    let copy_out = fx
        .ops
        .copy_file(
            request(
                loc(fx.b, "IMG_2.jpg"),
                loc(fx.a, "copy.jpg"),
                ConflictPolicy::Smart,
            ),
            &cx(),
        )
        .await;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(into_it.unwrap_err().code(), "volume_read_only");
    assert_eq!(out_of_it.unwrap_err().code(), "volume_read_only");
    assert!(copy_out.is_ok(), "reading from a read-only volume is fine");
    assert!(fx.tb.path("IMG_2.jpg").exists());
    assert!(fx.ta.path("IMG_1.jpg").exists());
    assert_eq!(fx.pending_rows().await, 0);
    let census = fx.content_census();
    assert_eq!(census[&b"one".to_vec()], 1);
    assert_eq!(census[&b"two".to_vec()], 2);
}

#[tokio::test]
async fn an_offline_volume_is_refused() {
    let fx = Fx::new().await;
    fx.ta.write("IMG_1.jpg", b"one");
    fx.scan().await;
    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "IMG_1.jpg"),
        ConflictPolicy::Smart,
    );

    let root = fx.tb.root().to_path_buf();
    let away = root.with_file_name("b.away");
    fs::rename(&root, &away).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();
    let offline = fx.ops.move_file(request, &cx()).await;
    fs::rename(&away, &root).unwrap();
    fx.app.state.volumes.refresh().await.unwrap();

    assert_eq!(offline.unwrap_err().code(), "volume_offline");
    assert!(fx.ta.path("IMG_1.jpg").exists());
    assert_eq!(fx.pending_rows().await, 0);
}

// --- failures of the copy ----------------------------------------------------------------------

#[tokio::test]
async fn a_corrupted_copy_is_caught_and_nothing_is_lost() {
    let corrupt = Failpoints::new(|step| {
        if step == Step::AfterCopy {
            FailAction::Corrupt
        } else {
            FailAction::Continue
        }
    });
    let fx = Fx::with(|ops, trees| {
        ops.with_mover(Arc::new(SplitMover::new(&trees)))
            .with_failpoints(corrupt)
    })
    .await;
    fx.ta.write("IMG_1.jpg", b"payload");
    fx.scan().await;

    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    let err = fx.ops.move_file(request, &cx()).await.unwrap_err();

    assert_eq!(err.code(), "verify_failed");
    assert_eq!(fs::read(fx.ta.path("IMG_1.jpg")).unwrap(), b"payload");
    assert!(!fx.tb.path("IMG_1.jpg").exists());
    assert!(fx.leftover_parts().is_empty());
    assert_eq!(fx.pending_rows().await, 0);
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_full_destination_is_no_space_and_creates_no_file() {
    let fx = Fx::with(|ops, trees| {
        ops.with_mover(Arc::new(SplitMover::new(&trees)))
            .with_free_space(Arc::new(FixedFreeSpace(0)))
    })
    .await;
    fx.ta.write("IMG_1.jpg", b"payload");
    fx.scan().await;

    let request = request(
        loc(fx.a, "IMG_1.jpg"),
        loc(fx.b, "IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    let err = fx.ops.move_file(request, &cx()).await.unwrap_err();

    assert_eq!(err.code(), "no_space");
    assert!(fx.ta.path("IMG_1.jpg").exists());
    assert!(!fx.tb.path("IMG_1.jpg").exists());
    assert!(fx.leftover_parts().is_empty());
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn cancelling_a_copy_halfway_removes_the_part_and_keeps_the_source() {
    let fx = two_devices().await;
    let big: Vec<u8> = (0..16_u32 * 1024 * 1024)
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    fx.ta.write("big.jpg", &big);
    fx.scan().await;

    let context = cx();
    let cancel = context.cancel.clone();
    let reports = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&reports);
    let context = context.with_on_bytes(Arc::new(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        cancel.cancel();
    }));
    let request = request(
        loc(fx.a, "big.jpg"),
        loc(fx.b, "big.jpg"),
        ConflictPolicy::Smart,
    );
    let err = fx.ops.move_file(request, &context).await.unwrap_err();

    assert_eq!(err.code(), "cancelled");
    assert!(reports.load(Ordering::SeqCst) >= 1);
    assert_eq!(fs::read(fx.ta.path("big.jpg")).unwrap(), big);
    assert!(!fx.tb.path("big.jpg").exists());
    assert!(fx.leftover_parts().is_empty());
    assert_eq!(fx.pending_rows().await, 0);
    fx.assert_in_sync().await;
}

// --- what the rest of the application sees -----------------------------------------------------

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

#[tokio::test]
async fn hooks_changes_and_self_writes_see_the_move() {
    let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
    let hook = Arc::clone(&recorder);
    let fx = Fx::with(|ops, _| ops.with_hook(hook)).await;
    fx.ta.write("x/IMG_1.jpg", b"one");
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let changes = Arc::new(RecordingChanges::new());
    let context = cx().with_changes(changes.clone());

    let request = request(
        loc(fx.a, "x/IMG_1.jpg"),
        loc(fx.a, "y/z/IMG_1.jpg"),
        ConflictPolicy::Smart,
    );
    fx.ops.move_file(request, &context).await.unwrap();

    assert_eq!(
        *recorder.0.lock().unwrap(),
        [AppliedOp::Moved {
            file_id: id,
            from: loc(fx.a, "x/IMG_1.jpg"),
            to: loc(fx.a, "y/z/IMG_1.jpg"),
        }]
    );
    let dirs = changes.dirs(fx.a);
    for dir in ["x", "y", "y/z", ""] {
        assert!(dirs.contains(dir), "{dir:?} is missing from {dirs:?}");
    }
    let recent = |dir: &str| {
        fx.ops
            .self_writes()
            .is_recent(fx.a, &common::fsops::rel(dir), Duration::from_secs(60))
    };
    assert!(recent("x") && recent("y/z"));
}
