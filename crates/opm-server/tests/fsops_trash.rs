//! Trashing, restoring, purging, making folders and removing them: what each does to the disk, the
//! index and the journal, how a restore settles a name that was taken meanwhile, what is refused,
//! and the guards around the trash folder itself. How these operations survive a crash is in
//! `fsops_crash_trash.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::BASE_MTIME;
use common::fsops::{
    Fx, Memory, TrashRow, disk_dirs, disk_files, loc, permissions_are_enforced, rel,
};
use futures_util::future::BoxFuture;
use opm_core::dto::common::ConflictPolicy;
use opm_core::paths::RelPath;
use opm_server::fsops::{
    AppliedOp, Expect, FailAction, Failpoints, FsOpError, Loc, OpContext, OpHook, PurgeOutcome,
    RestoreOutcome, RmdirOutcome, SkipReason, Step, TrashBatch, TrashOutcome, TrashRequest,
};
use opm_server::indexer::RecordingChanges;
use sqlx::SqliteConnection;
use tokio_util::sync::CancellationToken;

const PAYLOAD: &[u8] = b"the bytes of one photograph";
const OTHER: &[u8] = b"the bytes of another one";
const SIDECAR: &[u8] = b"<x:xmpmeta/>";
const LITTER: &[u8] = b"finder litter";

fn cx() -> OpContext {
    OpContext::detached()
}

fn len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap()
}

fn request(loc: Loc, batch: &TrashBatch) -> TrashRequest {
    TrashRequest {
        file_id: None,
        loc,
        expect: None,
        batch: batch.clone(),
        reason: "user".to_owned(),
    }
}

async fn trash_in(fx: &Fx, path: &str, batch: &TrashBatch) -> TrashOutcome {
    fx.ops
        .trash_file(request(loc(fx.a, path), batch), &cx())
        .await
        .unwrap()
}

async fn trash(fx: &Fx, path: &str) -> TrashOutcome {
    trash_in(fx, path, &TrashBatch::adhoc()).await
}

async fn restore(fx: &Fx, item: i64, policy: ConflictPolicy) -> RestoreOutcome {
    fx.ops.restore(item, policy, &cx()).await.unwrap()
}

/// `(dst, renamed, file_id, bytes)` of a restore that did restore.
fn restored(outcome: RestoreOutcome) -> (Loc, bool, Option<i64>, u64) {
    match outcome {
        RestoreOutcome::Restored {
            dst,
            renamed,
            file_id,
            bytes,
        } => (dst, renamed, file_id, bytes),
        RestoreOutcome::Skipped(reason) => panic!("expected a restore, got {reason:?}"),
    }
}

/// Every file and folder below the trash folder, as paths from the volume root. The trash folder
/// itself is not listed.
fn in_trash(fx: &Fx, volume: i64) -> Vec<String> {
    let root = fx.tree(volume).root();
    let mut all: BTreeSet<String> = disk_dirs(root).into_iter().collect();
    all.extend(disk_files(root).into_keys());
    all.into_iter()
        .filter(|path| path.starts_with(".opm-trash/"))
        .collect()
}

async fn thumb_state(fx: &Fx, id: i64) -> i64 {
    fx.scalar(&format!("SELECT thumb_state FROM files WHERE id = {id}"))
        .await
}

/// The batch names are `YYYYMMDD-HHMMSS-<kind><who>`.
fn batch_parts(batch: &TrashBatch) -> (bool, bool, String) {
    let mut parts = batch.name().splitn(3, '-');
    let date = parts.next().unwrap();
    let time = parts.next().unwrap();
    let tail = parts.next().unwrap().to_owned();
    let digits = |text: &str, count: usize| {
        text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit())
    };
    (digits(date, 8), digits(time, 6), tail)
}

// --- round trips -------------------------------------------------------------------------------

#[tokio::test]
async fn a_file_goes_to_the_trash_and_comes_back_with_what_the_index_knew() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let known = fx.decorate(id, PAYLOAD).await;
    let batch = TrashBatch::new(7);

    let outcome = trash_in(&fx, "x/IMG_1.jpg", &batch).await;

    let trash_path = format!(".opm-trash/{}/x/IMG_1.jpg", batch.name());
    assert_eq!(outcome.trash_path.as_str(), trash_path);
    assert_eq!(
        (outcome.is_dir, outcome.bytes, outcome.files_removed),
        (false, len(PAYLOAD), 1)
    );
    fx.gone(fx.a, "x/IMG_1.jpg", "trashed").await;
    assert_eq!(fs::read(fx.ta.path(&trash_path)).unwrap(), PAYLOAD);
    assert_eq!(
        fx.trash_items().await,
        [TrashRow {
            id: outcome.trash_item_id,
            volume_id: fx.a,
            batch: batch.name().to_owned(),
            orig_path: "x/IMG_1.jpg".to_owned(),
            trash_path: trash_path.clone(),
            is_dir: false,
            reason: Some("user".to_owned()),
            has_meta: true,
        }]
    );
    assert_eq!(
        fx.scalar("SELECT size FROM trash_items").await,
        i64::try_from(PAYLOAD.len()).unwrap()
    );
    fx.assert_in_sync().await;

    let outcome = restore(&fx, outcome.trash_item_id, ConflictPolicy::Smart).await;

    let (dst, renamed, new_id, bytes) = restored(outcome);
    assert_eq!(
        (dst, renamed, bytes),
        (loc(fx.a, "x/IMG_1.jpg"), false, len(PAYLOAD))
    );
    let new_id = new_id.expect("a restored file has a new row");
    assert_eq!(fx.row_id(fx.a, "x/IMG_1.jpg").await, new_id);
    assert_eq!(fx.memory_of(new_id).await, known);
    assert_eq!(thumb_state(&fx, new_id).await, 0, "thumbnails are redone");
    assert!(fx.trash_items().await.is_empty());
    assert_eq!(in_trash(&fx, fx.a), Vec::<String>::new());
    assert!(fx.ta.path(".opm-trash").is_dir());
    fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, "restored").await;
    fx.assert_journal(&[("trash", "done"), ("restore", "done")], "round trip")
        .await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn sidecar_links_come_back_with_the_file_or_the_sidecar() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.ta.write("x/IMG_1.xmp", SIDECAR);
    fx.ta.write("x/IMG_1.jpg.xmp", SIDECAR);
    fx.scan().await;
    let linked = fx.links().await;
    let link = |sidecar: &str, kind: &str| {
        (
            "a:x/IMG_1.jpg".to_owned(),
            format!("a:x/{sidecar}"),
            kind.to_owned(),
        )
    };
    assert_eq!(
        linked,
        BTreeSet::from([link("IMG_1.jpg.xmp", "exact"), link("IMG_1.xmp", "stem")])
    );

    let sidecar = trash(&fx, "x/IMG_1.xmp").await;
    assert_eq!(
        fx.links().await,
        BTreeSet::from([link("IMG_1.jpg.xmp", "exact")])
    );
    restore(&fx, sidecar.trash_item_id, ConflictPolicy::Smart).await;
    assert_eq!(fx.links().await, linked, "the sidecar is linked again");

    let owner = trash(&fx, "x/IMG_1.jpg").await;
    assert_eq!(fx.links().await, BTreeSet::new());
    let still_indexed: BTreeSet<String> =
        ["x/IMG_1.jpg.xmp", "x/IMG_1.xmp"].map(str::to_owned).into();
    assert_eq!(fx.paths(fx.a).await, still_indexed);
    restore(&fx, owner.trash_item_id, ConflictPolicy::Smart).await;
    assert_eq!(fx.links().await, linked, "the owner is linked again");
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_file_changed_in_the_trash_keeps_only_the_users_marks() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    fx.decorate(id, PAYLOAD).await;
    let item = trash(&fx, "x/IMG_1.jpg").await;
    fs::write(fx.ta.path(item.trash_path.as_str()), OTHER).unwrap();

    let (_, _, new_id, _) = restored(restore(&fx, item.trash_item_id, ConflictPolicy::Smart).await);

    let new_id = new_id.unwrap();
    let memory = fx.memory_of(new_id).await;
    assert_eq!(
        memory,
        Memory {
            rating: 4,
            flag: 1,
            ..Memory::default()
        },
        "what the pipeline found no longer describes the file"
    );
    let (_, size, _) = fx.row(fx.a, "x/IMG_1.jpg").await.unwrap();
    assert_eq!(size, i64::try_from(OTHER.len()).unwrap());
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_file_the_index_never_saw_can_be_trashed_and_restored() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    sqlx::query("DELETE FROM files")
        .execute(fx.app.state.db.write())
        .await
        .unwrap();

    let item = trash(&fx, "x/IMG_1.jpg").await;

    assert_eq!((item.files_removed, item.bytes), (0, len(PAYLOAD)));
    let rows = fx.trash_items().await;
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].has_meta, "there was nothing to remember");
    let (_, _, new_id, _) = restored(restore(&fx, item.trash_item_id, ConflictPolicy::Smart).await);
    assert_eq!(fx.memory_of(new_id.unwrap()).await, Memory::default());
    fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, "restored").await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_folder_goes_to_the_trash_whole_and_comes_back_indexed() {
    let outside = common::scratch_dir();
    let fx = Fx::new().await;
    fx.ta.write("x/a.jpg", PAYLOAD);
    fx.ta.write("x/sub/b.jpg", OTHER);
    fx.ta.write("x/sub/b.xmp", SIDECAR);
    fx.ta.write("x/.DS_Store", b"finder litter");
    symlink(outside.path(), fx.ta.path("x/sub/away")).unwrap();
    fx.scan().await;
    let linked = fx.links().await;
    assert_eq!(linked.len(), 1);
    let indexed = fx.scalar("SELECT SUM(size) FROM files").await;
    let census = fx.content_census();
    let batch = TrashBatch::new(3);

    let outcome = trash_in(&fx, "x", &batch).await;

    assert_eq!(
        outcome.trash_path.as_str(),
        format!(".opm-trash/{}/x", batch.name())
    );
    assert!(outcome.is_dir);
    assert_eq!(outcome.files_removed, 3);
    assert_eq!(i64::try_from(outcome.bytes).unwrap(), indexed);
    assert_eq!(fx.paths(fx.a).await, BTreeSet::new());
    assert!(!fx.dir_row(fx.a, "x").await && !fx.dir_row(fx.a, "x/sub").await);
    assert_eq!(fx.links().await, BTreeSet::new());
    assert!(!fx.ta.path("x").exists());
    let inside = format!(".opm-trash/{}/x", batch.name());
    for file in ["a.jpg", "sub/b.jpg", "sub/b.xmp", ".DS_Store"] {
        assert!(fx.ta.path(&format!("{inside}/{file}")).exists(), "{file}");
    }
    let items = fx.trash_items().await;
    assert_eq!(items.len(), 1);
    assert!(items[0].is_dir && items[0].has_meta);
    assert_eq!(items[0].reason.as_deref(), Some("user"));
    assert_eq!(fx.scalar("SELECT size FROM trash_items").await, indexed);
    fx.assert_in_sync().await;

    let outcome = restore(&fx, outcome.trash_item_id, ConflictPolicy::Smart).await;

    let (dst, renamed, file_id, bytes) = restored(outcome);
    assert_eq!((dst, renamed, file_id), (loc(fx.a, "x"), false, None));
    assert_eq!(i64::try_from(bytes).unwrap(), indexed);
    let back: BTreeSet<String> = ["x/a.jpg", "x/sub/b.jpg", "x/sub/b.xmp"]
        .map(str::to_owned)
        .into();
    assert_eq!(fx.paths(fx.a).await, back);
    assert!(fx.dir_row(fx.a, "x").await && fx.dir_row(fx.a, "x/sub").await);
    assert_eq!(fx.links().await, linked, "the sidecar is linked again");
    assert_eq!(
        fs::read(fx.ta.path("x/.DS_Store")).unwrap(),
        b"finder litter"
    );
    assert_eq!(
        fs::read_link(fx.ta.path("x/sub/away")).unwrap(),
        outside.path()
    );
    assert!(fx.trash_items().await.is_empty());
    assert_eq!(in_trash(&fx, fx.a), Vec::<String>::new());
    assert_eq!(fx.content_census(), census);
    fx.assert_journal(&[("trash", "done"), ("restore", "done")], "folder")
        .await;
    fx.assert_in_sync().await;
}

// --- a name that was taken while the item was in the trash -------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Skipped(SkipReason),
    Renamed,
    Replaced,
}

#[tokio::test]
async fn restoring_a_file_onto_a_taken_name_follows_the_policy() {
    use ConflictPolicy::{Rename, Replace, Skip, Smart};
    let identical = Want::Skipped(SkipReason::IdenticalAtDestination);
    let exists = Want::Skipped(SkipReason::DestinationExists);
    let cases = [
        (Smart, PAYLOAD, identical),
        (Smart, OTHER, Want::Renamed),
        (Rename, PAYLOAD, Want::Renamed),
        (Rename, OTHER, Want::Renamed),
        (Skip, PAYLOAD, exists),
        (Skip, OTHER, exists),
        (Replace, PAYLOAD, Want::Replaced),
        (Replace, OTHER, Want::Replaced),
    ];
    for (policy, occupant, want) in cases {
        let label = format!("{policy:?} over {} bytes ({want:?})", occupant.len());
        let fx = Fx::new().await;
        fx.ta.write("x/IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        let item = trash(&fx, "x/IMG_1.jpg").await;
        fx.ta.write("x/IMG_1.jpg", occupant);
        fx.scan().await;
        let census = fx.content_census();

        let outcome = restore(&fx, item.trash_item_id, policy).await;

        match want {
            Want::Skipped(reason) => {
                assert_eq!(outcome, RestoreOutcome::Skipped(reason), "{label}");
                fx.present(fx.a, "x/IMG_1.jpg", occupant, &label).await;
                let kept = fs::read(fx.ta.path(item.trash_path.as_str())).unwrap();
                assert_eq!(kept, PAYLOAD, "{label}: the item stays in the trash");
                assert_eq!(fx.trash_items().await.len(), 1, "{label}");
            }
            Want::Renamed => {
                let (dst, renamed, ..) = restored(outcome);
                assert_eq!(
                    (dst, renamed),
                    (loc(fx.a, "x/IMG_1_1.jpg"), true),
                    "{label}"
                );
                fx.present(fx.a, "x/IMG_1.jpg", occupant, &label).await;
                fx.present(fx.a, "x/IMG_1_1.jpg", PAYLOAD, &label).await;
                assert!(fx.trash_items().await.is_empty(), "{label}");
            }
            Want::Replaced => {
                let (dst, renamed, ..) = restored(outcome);
                assert_eq!((dst, renamed), (loc(fx.a, "x/IMG_1.jpg"), false), "{label}");
                fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, &label).await;
                let items = fx.trash_items().await;
                assert_eq!(items.len(), 1, "{label}: {items:?}");
                let (orig, reason) = (items[0].orig_path.as_str(), items[0].reason.as_deref());
                assert_eq!((orig, reason), ("x/IMG_1.jpg", Some("replaced")), "{label}");
                let trashed = fs::read(fx.ta.path(&items[0].trash_path)).unwrap();
                assert_eq!(
                    trashed, occupant,
                    "{label}: the replaced file is recoverable"
                );
            }
        }
        assert_eq!(fx.content_census(), census, "{label}: content was lost");
        assert_eq!(fx.pending_rows().await, 0, "{label}");
        fx.assert_in_sync_for(&label).await;
    }
}

#[tokio::test]
async fn a_folder_or_a_link_at_the_destination_is_never_replaced() {
    let outside = common::scratch_dir();
    let target = outside.path().join("target.jpg");
    fs::write(&target, OTHER).unwrap();
    for kind in ["folder", "link"] {
        let fx = Fx::new().await;
        fx.ta.write("x/IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        let item = trash(&fx, "x/IMG_1.jpg").await;
        let taken = fx.ta.path("x/IMG_1.jpg");
        if kind == "folder" {
            fx.ta.write("x/IMG_1.jpg/inner.jpg", OTHER);
        } else {
            symlink(&target, &taken).unwrap();
        }
        fx.scan().await;

        let skip = restore(&fx, item.trash_item_id, ConflictPolicy::Skip).await;
        let replace = fx
            .ops
            .restore(item.trash_item_id, ConflictPolicy::Replace, &cx())
            .await;

        assert_eq!(skip, RestoreOutcome::Skipped(SkipReason::DestinationExists));
        assert_eq!(replace.unwrap_err().code(), "destination_invalid", "{kind}");
        assert_eq!(fx.trash_items().await.len(), 1, "{kind}");
        fx.assert_journal(&[("trash", "done")], kind).await;

        let smart = restore(&fx, item.trash_item_id, ConflictPolicy::Smart).await;
        let (dst, renamed, ..) = restored(smart);
        assert_eq!((dst, renamed), (loc(fx.a, "x/IMG_1_1.jpg"), true), "{kind}");
        fx.present(fx.a, "x/IMG_1_1.jpg", PAYLOAD, kind).await;
        if kind == "folder" {
            fx.present(fx.a, "x/IMG_1.jpg/inner.jpg", OTHER, kind).await;
        } else {
            assert_eq!(fs::read_link(&taken).unwrap(), target);
        }
        assert_eq!(fs::read(&target).unwrap(), OTHER, "{kind}");
        fx.assert_in_sync_for(kind).await;
    }
}

/// A folder `x` in the trash and a new folder `x` where it was.
async fn folder_taken() -> (Fx, TrashOutcome) {
    let fx = Fx::new().await;
    fx.ta.write("x/a.jpg", PAYLOAD);
    fx.scan().await;
    let item = trash(&fx, "x").await;
    fx.ta.write("x/other.jpg", OTHER);
    fx.scan().await;
    (fx, item)
}

#[tokio::test]
async fn a_restored_folder_is_renamed_never_merged_into_the_one_that_took_its_name() {
    for policy in [ConflictPolicy::Smart, ConflictPolicy::Rename] {
        let (fx, item) = folder_taken().await;
        let added = format!("{}/added.jpg", item.trash_path);
        fs::write(fx.ta.path(&added), b"added in the trash").unwrap();
        let census = fx.content_census();

        let outcome = restore(&fx, item.trash_item_id, policy).await;

        let (dst, renamed, file_id, _) = restored(outcome);
        assert_eq!((dst, renamed, file_id), (loc(fx.a, "x_1"), true, None));
        let indexed: BTreeSet<String> = ["x/other.jpg", "x_1/a.jpg", "x_1/added.jpg"]
            .map(str::to_owned)
            .into();
        assert_eq!(
            fx.paths(fx.a).await,
            indexed,
            "{policy:?}: the folder is indexed as it is now"
        );
        fx.present(fx.a, "x/other.jpg", OTHER, "the new folder")
            .await;
        assert!(!fx.ta.path("x/a.jpg").exists(), "{policy:?}: no merge");
        assert_eq!(fx.content_census(), census, "{policy:?}");
        assert_eq!(in_trash(&fx, fx.a), Vec::<String>::new());
        fx.assert_in_sync_for(&format!("{policy:?}")).await;
    }
}

#[tokio::test]
async fn skip_and_replace_leave_a_trashed_folder_in_the_trash() {
    let (fx, item) = folder_taken().await;
    let census = fx.content_census();

    let skip = restore(&fx, item.trash_item_id, ConflictPolicy::Skip).await;
    let replace = fx
        .ops
        .restore(item.trash_item_id, ConflictPolicy::Replace, &cx())
        .await;

    assert_eq!(skip, RestoreOutcome::Skipped(SkipReason::DestinationExists));
    assert_eq!(replace.unwrap_err().code(), "destination_invalid");
    assert_eq!(fx.trash_items().await.len(), 1);
    fx.present(fx.a, "x/other.jpg", OTHER, "the new folder")
        .await;
    assert!(fx.ta.path(&format!("{}/a.jpg", item.trash_path)).exists());
    assert_eq!(fx.content_census(), census);
    fx.assert_journal(&[("trash", "done")], "refused").await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn restoring_recreates_the_folders_that_were_removed() {
    let fx = Fx::new().await;
    fx.ta.write("p/q/IMG_1.jpg", PAYLOAD);
    fx.ta.write("m/n/o/b.jpg", OTHER);
    fx.scan().await;
    let file = trash(&fx, "p/q/IMG_1.jpg").await;
    let folder = trash(&fx, "m/n/o").await;
    fx.ta.remove_dir("p");
    fx.ta.remove_dir("m");
    fx.scan().await;
    let dirs = ["p", "p/q", "m", "m/n", "m/n/o"];
    for dir in dirs {
        assert!(!fx.dir_row(fx.a, dir).await, "{dir} is still indexed");
    }

    restore(&fx, file.trash_item_id, ConflictPolicy::Smart).await;
    restore(&fx, folder.trash_item_id, ConflictPolicy::Smart).await;

    for dir in dirs {
        assert!(fx.dir_row(fx.a, dir).await, "{dir} was not indexed again");
    }
    fx.present(fx.a, "p/q/IMG_1.jpg", PAYLOAD, "file").await;
    fx.present(fx.a, "m/n/o/b.jpg", OTHER, "folder").await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_file_where_a_parent_folder_was_stops_the_restore() {
    for is_dir in [false, true] {
        let label = if is_dir { "folder" } else { "file" };
        let fx = Fx::new().await;
        fx.ta.write("p/x/IMG_1.jpg", PAYLOAD);
        fx.scan().await;
        let item = trash(&fx, if is_dir { "p/x" } else { "p/x/IMG_1.jpg" }).await;
        fx.ta.remove_dir("p");
        fx.ta.write("p", b"a file took the place of the folder");

        let refused = fx
            .ops
            .restore(item.trash_item_id, ConflictPolicy::Smart, &cx())
            .await;

        assert_eq!(
            refused.unwrap_err().code(),
            "destination_invalid",
            "{label}"
        );
        assert_eq!(fx.trash_items().await.len(), 1, "{label}");
        assert_eq!(
            fs::read(fx.ta.path("p")).unwrap(),
            b"a file took the place of the folder"
        );
        assert!(fx.ta.path(item.trash_path.as_str()).exists(), "{label}");
        fx.assert_journal(&[("trash", "done")], label).await;
    }
}

// --- purge -------------------------------------------------------------------------------------

#[tokio::test]
async fn purging_a_trashed_file_deletes_it_and_its_item() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.ta.write("keep/IMG_2.jpg", OTHER);
    fx.scan().await;
    let item = trash(&fx, "x/IMG_1.jpg").await;

    let purged = fx.ops.purge(item.trash_item_id, &cx()).await.unwrap();

    let expected = PurgeOutcome {
        bytes: len(PAYLOAD),
        files: 1,
    };
    assert_eq!(purged, expected);
    assert!(fx.trash_items().await.is_empty());
    assert_eq!(in_trash(&fx, fx.a), Vec::<String>::new());
    assert!(fx.ta.path(".opm-trash").is_dir(), "the trash folder stays");
    assert!(!fx.content_census().contains_key(PAYLOAD));
    fx.present(fx.a, "keep/IMG_2.jpg", OTHER, "bystander").await;
    fx.assert_journal(&[("trash", "done"), ("delete", "done")], "purge")
        .await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn purging_a_trashed_folder_deletes_everything_in_it() {
    let fx = Fx::new().await;
    fx.ta.write("x/a.jpg", PAYLOAD);
    fx.ta.write("x/sub/b.jpg", OTHER);
    fx.ta.write("x/.DS_Store", LITTER);
    fx.ta.write("y/c.jpg", SIDECAR);
    fx.scan().await;
    let item = trash(&fx, "x").await;

    let purged = fx.ops.purge(item.trash_item_id, &cx()).await.unwrap();

    let expected = PurgeOutcome {
        bytes: len(PAYLOAD) + len(OTHER) + len(LITTER),
        files: 3,
    };
    assert_eq!(purged, expected);
    assert!(fx.trash_items().await.is_empty());
    assert_eq!(in_trash(&fx, fx.a), Vec::<String>::new());
    assert_eq!(fx.content_census(), [(SIDECAR.to_vec(), 1)].into());
    fx.assert_journal(&[("trash", "done"), ("delete", "done")], "purge")
        .await;
    fx.assert_in_sync().await;
}

/// A `trash_items` row nobody should be able to make, pointing where the test says.
async fn plant(fx: &Fx, trash_path: &str, is_dir: bool) -> i64 {
    sqlx::query(
        "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, is_dir, size, reason, \
         trashed_at) VALUES (?, 'b', 'x', ?, ?, 0, 'user', 1700000000)",
    )
    .bind(fx.a)
    .bind(trash_path)
    .bind(i64::from(is_dir))
    .execute(fx.app.state.db.write())
    .await
    .unwrap()
    .last_insert_rowid()
}

#[tokio::test]
async fn a_trash_row_that_points_outside_an_item_is_refused_and_nothing_is_deleted() {
    let fx = Fx::new().await;
    fx.ta.write(".opm-trash/b/keep.jpg", PAYLOAD);
    fx.ta.write("real/x", OTHER);
    let census = fx.content_census();
    let whole_batch = plant(&fx, ".opm-trash/b", true).await;
    let dotdot = plant(&fx, ".opm-trash/../real/x", false).await;

    let cases = [
        (whole_batch, "outside_trash"),
        (dotdot, "internal"),
        (9999, "source_missing"),
    ];
    for (id, code) in cases {
        let purge = fx.ops.purge(id, &cx()).await.unwrap_err();
        let restore = fx
            .ops
            .restore(id, ConflictPolicy::Smart, &cx())
            .await
            .unwrap_err();
        assert_eq!((purge.code(), restore.code()), (code, code), "item {id}");
    }

    assert_eq!(fx.content_census(), census);
    assert_eq!(fx.trash_items().await.len(), 2);
    fx.assert_journal(&[], "nothing was attempted").await;
}

#[tokio::test]
async fn an_item_whose_content_is_gone_cannot_be_restored_but_can_be_purged() {
    for is_dir in [false, true] {
        let label = if is_dir { "folder" } else { "file" };
        let fx = Fx::new().await;
        fx.ta.write("x/a.jpg", PAYLOAD);
        fx.scan().await;
        let item = trash(&fx, if is_dir { "x" } else { "x/a.jpg" }).await;
        fs::remove_dir_all(fx.ta.path(".opm-trash")).unwrap();

        let restore = fx
            .ops
            .restore(item.trash_item_id, ConflictPolicy::Smart, &cx())
            .await;
        assert_eq!(restore.unwrap_err().code(), "source_missing", "{label}");
        assert_eq!(fx.trash_items().await.len(), 1, "{label}");

        let purged = fx.ops.purge(item.trash_item_id, &cx()).await.unwrap();
        assert_eq!(purged, PurgeOutcome { bytes: 0, files: 0 }, "{label}");
        assert!(fx.trash_items().await.is_empty(), "{label}");
        assert_eq!(fx.pending_rows().await, 0, "{label}");
    }
}

#[tokio::test]
async fn purge_unlinks_the_links_in_an_item_and_never_follows_them() {
    let outside = common::scratch_dir();
    fs::create_dir(outside.path().join("precious")).unwrap();
    fs::write(outside.path().join("precious/p.jpg"), PAYLOAD).unwrap();
    fs::write(outside.path().join("file.jpg"), OTHER).unwrap();
    let fx = Fx::new().await;
    fx.ta.write("x/a.jpg", SIDECAR);
    fx.scan().await;
    let item = trash(&fx, "x").await;
    let inside = fx.ta.path(item.trash_path.as_str());
    symlink(outside.path().join("precious"), inside.join("dir-link")).unwrap();
    symlink(outside.path().join("file.jpg"), inside.join("file-link")).unwrap();

    let purged = fx.ops.purge(item.trash_item_id, &cx()).await.unwrap();

    let expected = PurgeOutcome {
        bytes: len(SIDECAR),
        files: 3,
    };
    assert_eq!(
        purged, expected,
        "the links count as files, not as their size"
    );
    assert_eq!(
        fs::read(outside.path().join("precious/p.jpg")).unwrap(),
        PAYLOAD
    );
    assert_eq!(fs::read(outside.path().join("file.jpg")).unwrap(), OTHER);
    assert!(fx.trash_items().await.is_empty());
    assert_eq!(in_trash(&fx, fx.a), Vec::<String>::new());
}

#[tokio::test]
async fn a_link_where_the_trash_folder_should_be_stops_purge_restore_and_trash() {
    let outside = common::scratch_dir();
    let fx = Fx::new().await;
    fx.ta.write("x/a.jpg", PAYLOAD);
    fx.ta.write("y/b.jpg", OTHER);
    fx.scan().await;
    let item = trash(&fx, "x/a.jpg").await;
    let elsewhere = outside.path().join("trash");
    fs::rename(fx.ta.path(".opm-trash"), &elsewhere).unwrap();
    symlink(&elsewhere, fx.ta.path(".opm-trash")).unwrap();
    let held = elsewhere.join(
        item.trash_path
            .as_str()
            .strip_prefix(".opm-trash/")
            .unwrap(),
    );
    let census = disk_files(outside.path());

    let purge = fx.ops.purge(item.trash_item_id, &cx()).await;
    let restore = fx
        .ops
        .restore(item.trash_item_id, ConflictPolicy::Smart, &cx())
        .await;
    let another = fx
        .ops
        .trash_file(request(loc(fx.a, "y/b.jpg"), &TrashBatch::adhoc()), &cx())
        .await;

    for (what, result) in [
        ("purge", purge.map(|_| ())),
        ("restore", restore.map(|_| ())),
        ("trash", another.map(|_| ())),
    ] {
        assert_eq!(result.unwrap_err().code(), "symlink_rejected", "{what}");
    }
    assert_eq!(
        fs::read(&held).unwrap(),
        PAYLOAD,
        "what the link points to is untouched"
    );
    assert_eq!(
        disk_files(outside.path()),
        census,
        "nothing was written through the link"
    );
    fx.present(fx.a, "y/b.jpg", OTHER, "the file that was to be trashed")
        .await;
    assert_eq!(fx.trash_items().await.len(), 1);
    fx.assert_journal(&[("trash", "done"), ("delete", "failed")], "the link")
        .await;
}

#[tokio::test]
async fn a_cancelled_purge_keeps_its_item_and_a_repeat_finishes_it() {
    let fx = Fx::new().await;
    for name in ["a", "b", "c"] {
        fx.ta
            .write(&format!("x/{name}.jpg"), name.repeat(4).as_bytes());
    }
    fx.scan().await;
    let item = trash(&fx, "x").await;
    let context = cx();
    let cancel = context.cancel.clone();
    let context = context.with_on_bytes(Arc::new(move |_| cancel.cancel()));

    let cancelled = fx.ops.purge(item.trash_item_id, &context).await;

    assert_eq!(cancelled.unwrap_err(), FsOpError::Cancelled);
    assert_eq!(fx.trash_items().await.len(), 1, "the item is still listed");
    let left = in_trash(&fx, fx.a)
        .into_iter()
        .filter(|path| path.ends_with(".jpg"))
        .count();
    assert_eq!(left, 2, "what was deleted stays deleted");
    fx.assert_journal(&[("trash", "done"), ("delete", "failed")], "cancelled")
        .await;

    let purged = fx.ops.purge(item.trash_item_id, &cx()).await.unwrap();

    assert_eq!(purged, PurgeOutcome { bytes: 8, files: 2 });
    assert!(fx.trash_items().await.is_empty());
    assert_eq!(in_trash(&fx, fx.a), Vec::<String>::new());
    fx.assert_journal(
        &[("trash", "done"), ("delete", "failed"), ("delete", "done")],
        "repeated",
    )
    .await;
}

// --- batches -----------------------------------------------------------------------------------

#[test]
fn an_ad_hoc_batch_is_named_after_the_moment_and_a_random_suffix() {
    let batch = TrashBatch::adhoc();

    let (date_ok, time_ok, tail) = batch_parts(&batch);

    assert!(date_ok && time_ok, "{batch}");
    let random = tail.strip_prefix('r').unwrap();
    assert_eq!(random.len(), 8, "{batch}");
    assert!(
        random.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{batch}"
    );
    assert_ne!(batch, TrashBatch::adhoc());
}

#[tokio::test]
async fn what_one_task_trashes_lands_in_one_batch_that_mirrors_the_paths() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.ta.write("x/IMG_2.jpg", OTHER);
    fx.ta.write("y/IMG_3.jpg", SIDECAR);
    fx.scan().await;
    let context = OpContext::new(
        Some(7),
        CancellationToken::new(),
        Arc::new(|_| {}),
        Arc::new(RecordingChanges::new()),
    );
    let batch = context.batch().clone();
    let (date_ok, time_ok, tail) = batch_parts(&batch);
    assert!(date_ok && time_ok, "{batch}");
    assert_eq!(tail, "t7");
    let clone = context.clone();
    assert_eq!(clone.batch(), &batch, "a clone shares the batch");

    let mut ids = Vec::new();
    for path in ["x/IMG_1.jpg", "x/IMG_2.jpg", "y/IMG_3.jpg"] {
        let request = request(loc(fx.a, path), context.batch());
        let item = fx.ops.trash_file(request, &context).await.unwrap();
        assert_eq!(
            item.trash_path.as_str(),
            format!(".opm-trash/{batch}/{path}")
        );
        ids.push(item.trash_item_id);
    }

    let counted = |sql: &'static str| fx.scalar(sql);
    assert_eq!(
        counted("SELECT COUNT(DISTINCT batch) FROM trash_items").await,
        1
    );
    assert_eq!(
        counted("SELECT COUNT(*) FROM trash_items WHERE task_id = 7").await,
        3
    );
    assert_eq!(
        counted("SELECT COUNT(*) FROM fs_journal WHERE task_id = 7").await,
        3
    );

    // A folder of the batch goes with its last item, never before; the trash folder itself stays.
    let inside = |sub: &str| fx.ta.path(&format!(".opm-trash/{batch}/{sub}"));
    restore(&fx, ids[0], ConflictPolicy::Smart).await;
    assert!(inside("x/IMG_2.jpg").exists() && inside("y/IMG_3.jpg").exists());
    restore(&fx, ids[1], ConflictPolicy::Smart).await;
    assert!(
        !inside("x").exists(),
        "an emptied folder of the batch is pruned"
    );
    assert!(inside("y/IMG_3.jpg").exists());
    restore(&fx, ids[2], ConflictPolicy::Smart).await;
    assert!(
        !inside("").exists(),
        "the batch is pruned with its last item"
    );
    assert!(fx.ta.path(".opm-trash").is_dir());
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn the_same_path_trashed_twice_in_one_batch_gets_a_suffix_and_both_come_back() {
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let batch = TrashBatch::adhoc();
    let first = trash_in(&fx, "x/IMG_1.jpg", &batch).await;
    fx.ta.write("x/IMG_1.jpg", OTHER);
    fx.scan().await;

    let second = trash_in(&fx, "x/IMG_1.jpg", &batch).await;

    assert_eq!(
        first.trash_path.as_str(),
        format!(".opm-trash/{batch}/x/IMG_1.jpg")
    );
    assert_eq!(
        second.trash_path.as_str(),
        format!(".opm-trash/{batch}/x/IMG_1_1.jpg")
    );
    let items = fx.trash_items().await;
    assert!(items.iter().all(|item| item.orig_path == "x/IMG_1.jpg"));
    assert_eq!(items.len(), 2);

    let (dst, renamed, ..) =
        restored(restore(&fx, first.trash_item_id, ConflictPolicy::Smart).await);
    assert_eq!((dst, renamed), (loc(fx.a, "x/IMG_1.jpg"), false));
    let (dst, renamed, ..) =
        restored(restore(&fx, second.trash_item_id, ConflictPolicy::Smart).await);
    assert_eq!((dst, renamed), (loc(fx.a, "x/IMG_1_1.jpg"), true));
    fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, "first").await;
    fx.present(fx.a, "x/IMG_1_1.jpg", OTHER, "second").await;
    assert_eq!(in_trash(&fx, fx.a), Vec::<String>::new());
    fx.assert_in_sync().await;
}

// --- what is refused ---------------------------------------------------------------------------

#[tokio::test]
async fn trashing_what_cannot_be_trashed_is_refused_and_nothing_changes() {
    let outside = common::scratch_dir();
    fs::write(outside.path().join("target.jpg"), OTHER).unwrap();
    let fx = Fx::new().await;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.ta.write("x/IMG_2.jpg", OTHER);
    fx.scan().await;
    let held = trash(&fx, "x/IMG_2.jpg").await;
    symlink(outside.path().join("target.jpg"), fx.ta.path("x/link.jpg")).unwrap();
    symlink(outside.path(), fx.ta.path("x/dirlink")).unwrap();
    let pipe = fx.ta.path("x/pipe");
    let have_pipe = Command::new("mkfifo")
        .arg(&pipe)
        .status()
        .is_ok_and(|status| status.success());
    let snapshot = |fx: &Fx| (disk_files(fx.ta.root()), disk_dirs(fx.ta.root()));
    let (disk, journal) = (snapshot(&fx), fx.journal().await);
    let stale = Expect {
        size: len(PAYLOAD) + 1,
        mtime: BASE_MTIME,
    };
    let mut cases = vec![
        (
            "the volume root",
            loc(fx.a, ""),
            None,
            "destination_invalid",
        ),
        (
            "something already in the trash",
            loc(fx.a, held.trash_path.as_str()),
            None,
            "destination_invalid",
        ),
        (
            "the trash folder",
            loc(fx.a, ".opm-trash"),
            None,
            "destination_invalid",
        ),
        (
            "a missing file",
            loc(fx.a, "x/none.jpg"),
            None,
            "source_missing",
        ),
        (
            "a file in a file",
            loc(fx.a, "x/IMG_1.jpg/in"),
            None,
            "source_missing",
        ),
        (
            "a changed file",
            loc(fx.a, "x/IMG_1.jpg"),
            Some(stale),
            "source_changed",
        ),
        (
            "a link to a file",
            loc(fx.a, "x/link.jpg"),
            None,
            "symlink_rejected",
        ),
        (
            "a link to a folder",
            loc(fx.a, "x/dirlink"),
            None,
            "symlink_rejected",
        ),
        (
            "a path through a link",
            loc(fx.a, "x/dirlink/target.jpg"),
            None,
            "symlink_rejected",
        ),
    ];
    if have_pipe {
        cases.push((
            "a named pipe",
            loc(fx.a, "x/pipe"),
            None,
            "not_a_regular_file",
        ));
    }

    for (what, location, expect, code) in cases {
        let request = TrashRequest {
            expect,
            ..request(location, &TrashBatch::adhoc())
        };
        let refused = fx.ops.trash_file(request, &cx()).await;
        assert_eq!(refused.unwrap_err().code(), code, "{what}");
    }

    assert_eq!(
        snapshot(&fx),
        disk,
        "a refusal must leave the disk as it was"
    );
    assert_eq!(
        fx.journal().await,
        journal,
        "a refusal leaves no journal row"
    );
    assert_eq!(fx.trash_items().await.len(), 1);
    let unknown = fx
        .ops
        .trash_file(
            request(loc(9999, "x/IMG_1.jpg"), &TrashBatch::adhoc()),
            &cx(),
        )
        .await;
    assert_eq!(unknown.unwrap_err().code(), "unknown_volume");

    let seen = fx.ops.stat(&loc(fx.a, "x/IMG_1.jpg")).await.unwrap();
    let expected = Expect {
        size: seen.size,
        mtime: seen.mtime,
    };
    let request = TrashRequest {
        expect: Some(expected),
        ..request(loc(fx.a, "x/IMG_1.jpg"), &TrashBatch::adhoc())
    };
    fx.ops.trash_file(request, &cx()).await.unwrap();
    fx.gone(
        fx.a,
        "x/IMG_1.jpg",
        "the file that was what the caller expected",
    )
    .await;
}

#[tokio::test]
async fn a_volume_that_is_offline_or_read_only_refuses_every_change() {
    for state in ["offline", "read_only"] {
        let fx = Fx::new().await;
        if state == "read_only" && !permissions_are_enforced(fx.ta.root()) {
            continue;
        }
        fx.ta.write("x/IMG_1.jpg", PAYLOAD);
        fx.ta.write("y/IMG_2.jpg", OTHER);
        fx.ta.write("junk/.DS_Store", LITTER);
        fx.scan().await;
        let kept = trash(&fx, "y/IMG_2.jpg").await;
        let snapshot = |fx: &Fx| (disk_files(fx.ta.root()), disk_dirs(fx.ta.root()));
        let (disk, journal) = (snapshot(&fx), fx.journal().await);
        let root = fx.ta.root().to_path_buf();
        let away = root.with_file_name("a.away");
        if state == "offline" {
            fs::rename(&root, &away).unwrap();
        } else {
            fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
        }
        fx.app.state.volumes.refresh().await.unwrap();

        let batch = TrashBatch::adhoc();
        let results = [
            (
                "trash",
                fx.ops
                    .trash_file(request(loc(fx.a, "x/IMG_1.jpg"), &batch), &cx())
                    .await
                    .map(|_| ()),
            ),
            (
                "restore",
                fx.ops
                    .restore(kept.trash_item_id, ConflictPolicy::Smart, &cx())
                    .await
                    .map(|_| ()),
            ),
            (
                "purge",
                fx.ops.purge(kept.trash_item_id, &cx()).await.map(|_| ()),
            ),
            (
                "mkdir",
                fx.ops.mkdir_all(loc(fx.a, "new/dir")).await.map(|_| ()),
            ),
            (
                "rmdir",
                fx.ops
                    .remove_dir_if_removable(loc(fx.a, "junk"), &batch, &cx())
                    .await
                    .map(|_| ()),
            ),
        ];
        if state == "offline" {
            fs::rename(&away, &root).unwrap();
        } else {
            fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fx.app.state.volumes.refresh().await.unwrap();

        let code = if state == "offline" {
            "volume_offline"
        } else {
            "volume_read_only"
        };
        for (what, result) in results {
            assert_eq!(result.unwrap_err().code(), code, "{state}: {what}");
        }
        assert_eq!(snapshot(&fx), disk, "{state}: the disk changed");
        assert_eq!(
            fx.journal().await,
            journal,
            "{state}: a journal row appeared"
        );
        assert_eq!(fx.pending_rows().await, 0, "{state}");
        assert_eq!(fx.trash_items().await.len(), 1, "{state}");
    }
}

// --- mkdir_all ---------------------------------------------------------------------------------

#[tokio::test]
async fn mkdir_all_creates_the_missing_chain_and_indexes_every_folder() {
    let fx = Fx::new().await;
    fx.ta.mkdir("p");
    fx.scan().await;
    let generation = fx
        .scalar(&format!("SELECT scan_gen FROM volumes WHERE id = {}", fx.a))
        .await;

    let created = fx.ops.mkdir_all(loc(fx.a, "p/q/r")).await.unwrap();

    assert_eq!(created, [rel("p/q"), rel("p/q/r")]);
    assert!(fx.ta.path("p/q/r").is_dir());
    for dir in ["p", "p/q", "p/q/r"] {
        let seen = fx
            .scalar(&format!(
                "SELECT seen_gen FROM dirs WHERE volume_id = {} AND path = '{dir}'",
                fx.a
            ))
            .await;
        assert_eq!(
            seen, generation,
            "{dir} must count as seen by the current scan"
        );
    }
    assert_eq!(
        fx.ops.mkdir_all(loc(fx.a, "p/q/r")).await.unwrap(),
        Vec::<RelPath>::new()
    );
    assert_eq!(
        fx.ops.mkdir_all(loc(fx.a, "")).await.unwrap(),
        Vec::<RelPath>::new()
    );
    fx.assert_journal(&[], "mkdir is not journaled").await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn mkdir_all_refuses_what_it_must_not_create_and_leaves_the_disk_alone() {
    let outside = common::scratch_dir();
    let fx = Fx::new().await;
    fx.ta.write("file.jpg", PAYLOAD);
    symlink(outside.path(), fx.ta.path("link")).unwrap();
    fx.scan().await;
    let snapshot = |fx: &Fx| (disk_files(fx.ta.root()), disk_dirs(fx.ta.root()));
    let before = snapshot(&fx);
    let too_long = "n".repeat(256);
    let cases = [
        (".opm-trash", "destination_invalid"),
        (".opm-trash/x", "destination_invalid"),
        ("@eaDir", "destination_invalid"),
        ("x/@eaDir/y", "destination_invalid"),
        (".opm-probe-abc", "destination_invalid"),
        ("file.jpg", "destination_invalid"),
        ("file.jpg/inner", "destination_invalid"),
        ("link", "symlink_rejected"),
        ("link/inner", "symlink_rejected"),
        (too_long.as_str(), "name_too_long"),
    ];

    for (path, code) in cases {
        let refused = fx.ops.mkdir_all(loc(fx.a, path)).await;
        assert_eq!(refused.unwrap_err().code(), code, "{path}");
    }

    assert_eq!(snapshot(&fx), before);
    assert!(
        fs::read_dir(outside.path()).unwrap().next().is_none(),
        "nothing went through the link"
    );
    let unknown = fx.ops.mkdir_all(loc(9999, "x")).await;
    assert_eq!(unknown.unwrap_err().code(), "unknown_volume");
    fx.assert_journal(&[], "mkdir is not journaled").await;
    fx.assert_in_sync().await;
}

// --- remove_dir_if_removable -------------------------------------------------------------------

async fn rmdir(fx: &Fx, path: &str, batch: &TrashBatch) -> Result<RmdirOutcome, FsOpError> {
    fx.ops
        .remove_dir_if_removable(loc(fx.a, path), batch, &cx())
        .await
}

#[tokio::test]
async fn an_empty_folder_is_removed_and_forgotten() {
    let fx = Fx::new().await;
    fx.ta.write("keep/IMG_1.jpg", PAYLOAD);
    fx.ta.mkdir("p/empty");
    fx.scan().await;
    assert!(fx.dir_row(fx.a, "p/empty").await);

    let outcome = rmdir(&fx, "p/empty", &TrashBatch::adhoc()).await.unwrap();

    assert_eq!(outcome, RmdirOutcome::Removed { junk_trashed: 0 });
    assert!(!fx.ta.path("p/empty").exists());
    assert!(fx.ta.path("p").is_dir(), "the parent stays");
    assert!(!fx.dir_row(fx.a, "p/empty").await && fx.dir_row(fx.a, "p").await);
    assert!(fx.trash_items().await.is_empty());
    fx.assert_journal(&[("rmdir", "done")], "empty").await;
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_folder_with_only_junk_is_removed_after_its_junk_was_trashed() {
    let fx = Fx::new().await;
    fx.ta.write("d/.DS_Store", LITTER);
    fx.ta.write("d/._x.jpg", b"apple double");
    fx.ta
        .write("d/@eaDir/foo.jpg/thumb.jpg", b"synology thumbnail");
    fx.ta.write("d/Thumbs.db", b"windows thumbnails");
    fx.ta.write("keep/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let census = fx.content_census();
    let batch = TrashBatch::new(9);

    let outcome = rmdir(&fx, "d", &batch).await.unwrap();

    assert_eq!(outcome, RmdirOutcome::Removed { junk_trashed: 4 });
    assert!(!fx.ta.path("d").exists());
    let items = fx.trash_items().await;
    let described: Vec<(&str, bool, Option<&str>, &str)> = items
        .iter()
        .map(|item| {
            let reason = item.reason.as_deref();
            (
                item.orig_path.as_str(),
                item.is_dir,
                reason,
                item.batch.as_str(),
            )
        })
        .collect();
    let junk = Some("junk");
    let name = batch.name();
    assert_eq!(
        described,
        [
            ("d/.DS_Store", false, junk, name),
            ("d/._x.jpg", false, junk, name),
            ("d/@eaDir", true, junk, name),
            ("d/Thumbs.db", false, junk, name),
        ]
    );
    assert_eq!(
        fx.content_census(),
        census,
        "the junk was trashed, not deleted"
    );
    fx.assert_journal(
        &[
            ("trash", "done"),
            ("trash", "done"),
            ("trash", "done"),
            ("trash", "done"),
            ("rmdir", "done"),
        ],
        "junk",
    )
    .await;
    assert!(!fx.dir_row(fx.a, "d").await);
    fx.assert_in_sync().await;

    let litter = items
        .iter()
        .find(|item| item.orig_path == "d/.DS_Store")
        .unwrap();
    let (dst, renamed, file_id, _) = restored(restore(&fx, litter.id, ConflictPolicy::Smart).await);
    assert_eq!(
        (dst, renamed, file_id),
        (loc(fx.a, "d/.DS_Store"), false, None)
    );
    assert_eq!(fs::read(fx.ta.path("d/.DS_Store")).unwrap(), LITTER);
    fx.assert_in_sync().await;
}

#[tokio::test]
async fn a_folder_that_holds_anything_else_is_left_exactly_as_it_is() {
    let outside = common::scratch_dir();
    let kinds = [
        "a file",
        "a folder",
        "a link named like junk",
        "#recycle",
        "#Snapshot",
        "#snapshot",
        "a name that is not UTF-8",
        "a named pipe",
    ];
    for kind in kinds {
        let fx = Fx::new().await;
        fx.ta.write("d/.DS_Store", LITTER);
        let inside = fx.ta.path("d");
        match kind {
            "a file" => fx.ta.write("d/IMG_1.jpg", PAYLOAD),
            "a folder" => fx.ta.mkdir("d/sub"),
            "a link named like junk" => symlink(outside.path(), inside.join("Thumbs.db")).unwrap(),
            "#recycle" | "#Snapshot" | "#snapshot" => {
                fx.ta.write(&format!("d/{kind}/a.jpg"), PAYLOAD)
            }
            "a name that is not UTF-8" => {
                let name = OsStr::from_bytes(b"bad-\xff.jpg");
                if fs::write(inside.join(name), PAYLOAD).is_err() {
                    continue;
                }
            }
            _ => {
                let made = Command::new("mkfifo").arg(inside.join("pipe")).status();
                if !made.is_ok_and(|status| status.success()) {
                    continue;
                }
            }
        }
        fx.scan().await;
        let (files, dirs) = (disk_files(fx.ta.root()), disk_dirs(fx.ta.root()));

        let refused = rmdir(&fx, "d", &TrashBatch::adhoc()).await;

        assert_eq!(refused.unwrap_err(), FsOpError::NotEmpty, "{kind}");
        assert_eq!(
            disk_files(fx.ta.root()),
            files,
            "{kind}: the junk beside it was touched"
        );
        assert_eq!(disk_dirs(fx.ta.root()), dirs, "{kind}");
        assert!(fx.trash_items().await.is_empty(), "{kind}");
        fx.assert_journal(&[], kind).await;
    }
}

#[tokio::test]
async fn removing_a_folder_that_is_not_there_is_not_an_error() {
    let fx = Fx::new().await;
    fx.ta.write("file.jpg", PAYLOAD);
    fx.scan().await;
    let batch = TrashBatch::adhoc();

    for path in ["missing", "missing/deeper", "file.jpg/inside"] {
        let outcome = rmdir(&fx, path, &batch).await.unwrap();
        assert_eq!(outcome, RmdirOutcome::AlreadyGone, "{path}");
    }

    fx.assert_journal(&[], "nothing was attempted").await;
    fx.present(fx.a, "file.jpg", PAYLOAD, "bystander").await;
}

#[tokio::test]
async fn the_root_the_trash_files_and_links_are_not_removable_folders() {
    let outside = common::scratch_dir();
    let fx = Fx::new().await;
    fx.ta.write("d/IMG_1.jpg", PAYLOAD);
    fx.ta.write(".opm-trash/b/x.jpg", OTHER);
    symlink(outside.path(), fx.ta.path("link")).unwrap();
    let (files, dirs) = (disk_files(fx.ta.root()), disk_dirs(fx.ta.root()));
    let batch = TrashBatch::adhoc();
    let cases = [
        ("", "destination_invalid"),
        (".opm-trash", "destination_invalid"),
        (".opm-trash/b", "destination_invalid"),
        ("d/IMG_1.jpg", "destination_invalid"),
        ("link", "symlink_rejected"),
    ];

    for (path, code) in cases {
        let refused = rmdir(&fx, path, &batch).await;
        assert_eq!(refused.unwrap_err().code(), code, "{path:?}");
    }

    let unknown = fx
        .ops
        .remove_dir_if_removable(loc(9999, "d"), &batch, &cx())
        .await;
    assert_eq!(unknown.unwrap_err().code(), "unknown_volume");
    assert_eq!(
        (disk_files(fx.ta.root()), disk_dirs(fx.ta.root())),
        (files, dirs)
    );
    fx.assert_journal(&[], "nothing was attempted").await;
}

#[tokio::test]
async fn a_file_that_arrives_while_the_junk_is_trashed_keeps_the_folder() {
    let hits = Arc::new(AtomicUsize::new(0));
    let fx = Fx::with(|ops, trees| {
        let late = trees[0].path("d/late.jpg");
        let hits = Arc::clone(&hits);
        ops.with_failpoints(Failpoints::new(move |step| {
            // The first journaled step is the junk's trash, the second is the removal itself.
            if step == Step::AfterJournal && hits.fetch_add(1, Ordering::SeqCst) == 1 {
                fs::write(&late, b"arrived in the meantime").unwrap();
            }
            FailAction::Continue
        }))
    })
    .await;
    fx.ta.write("d/.DS_Store", LITTER);
    fx.scan().await;

    let refused = rmdir(&fx, "d", &TrashBatch::adhoc()).await;

    assert_eq!(refused.unwrap_err(), FsOpError::NotEmpty);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        fs::read(fx.ta.path("d/late.jpg")).unwrap(),
        b"arrived in the meantime"
    );
    assert_eq!(fx.trash_items().await.len(), 1, "the junk stays trashed");
    fx.assert_journal(&[("trash", "done"), ("rmdir", "failed")], "raced")
        .await;
    assert_eq!(fx.pending_rows().await, 0);
}

// --- what the rest of the application sees -----------------------------------------------------

/// Remembers every announced operation and how many `files` rows the hook's own transaction
/// could still see.
#[derive(Default)]
struct Spy(Mutex<Vec<(AppliedOp, i64)>>);

impl OpHook for Spy {
    fn on_applied<'a>(
        &'a self,
        tx: &'a mut SqliteConnection,
        op: &'a AppliedOp,
    ) -> BoxFuture<'a, Result<(), sqlx::Error>> {
        Box::pin(async move {
            let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
                .fetch_one(&mut *tx)
                .await?;
            let mut op = op.clone();
            if let AppliedOp::Trashed { file_ids, .. } = &mut op {
                file_ids.sort_unstable();
            }
            self.0.lock().unwrap().push((op, visible));
            Ok(())
        })
    }
}

#[tokio::test]
async fn hooks_changes_and_self_writes_see_trash_restore_and_purge() {
    let spy = Arc::new(Spy::default());
    let hook = Arc::clone(&spy);
    let fx = Fx::with(|ops, _| ops.with_hook(hook)).await;
    fx.ta.write("x/a.jpg", PAYLOAD);
    fx.ta.write("x/sub/b.jpg", OTHER);
    fx.ta.write("z/c.jpg", SIDECAR);
    fx.scan().await;
    let mut in_folder = vec![
        fx.row_id(fx.a, "x/a.jpg").await,
        fx.row_id(fx.a, "x/sub/b.jpg").await,
    ];
    in_folder.sort_unstable();
    let id_c = fx.row_id(fx.a, "z/c.jpg").await;
    let changes = Arc::new(RecordingChanges::new());
    let context = cx().with_changes(changes.clone());
    let batch = TrashBatch::adhoc();
    let ops = &fx.ops;

    let folder = ops
        .trash_file(request(loc(fx.a, "x"), &batch), &context)
        .await
        .unwrap();
    let file = ops
        .trash_file(request(loc(fx.a, "z/c.jpg"), &batch), &context)
        .await
        .unwrap();
    let back = ops
        .restore(file.trash_item_id, ConflictPolicy::Smart, &context)
        .await
        .unwrap();
    let (_, _, new_c, _) = restored(back);
    let new_c = new_c.unwrap();
    ops.restore(folder.trash_item_id, ConflictPolicy::Smart, &context)
        .await
        .unwrap();
    let again = ops
        .trash_file(request(loc(fx.a, "z/c.jpg"), &batch), &context)
        .await
        .unwrap();
    ops.purge(again.trash_item_id, &context).await.unwrap();

    let seen = spy.0.lock().unwrap().clone();
    let trashed = |file_ids: Vec<i64>, from: &str, item: &TrashOutcome| AppliedOp::Trashed {
        file_ids,
        from: loc(fx.a, from),
        trash_item_id: item.trash_item_id,
        trash_path: item.trash_path.clone(),
        is_dir: item.is_dir,
    };
    let expected = [
        trashed(in_folder, "x", &folder),
        trashed(vec![id_c], "z/c.jpg", &file),
        AppliedOp::Restored {
            file_id: Some(new_c),
            trash_item_id: file.trash_item_id,
            from: loc(fx.a, file.trash_path.as_str()),
            to: loc(fx.a, "z/c.jpg"),
            is_dir: false,
        },
        AppliedOp::Restored {
            file_id: None,
            trash_item_id: folder.trash_item_id,
            from: loc(fx.a, folder.trash_path.as_str()),
            to: loc(fx.a, "x"),
            is_dir: true,
        },
        trashed(vec![new_c], "z/c.jpg", &again),
        AppliedOp::Purged {
            trash_item_id: again.trash_item_id,
            volume_id: fx.a,
            trash_path: again.trash_path.clone(),
            is_dir: false,
            bytes: len(SIDECAR),
        },
    ];
    let ops_seen: Vec<AppliedOp> = seen.iter().map(|(op, _)| op.clone()).collect();
    assert_eq!(ops_seen, expected);
    let visible: Vec<i64> = [0, 1, 4]
        .into_iter()
        .map(|position| seen[position].1)
        .collect();
    assert_eq!(
        visible,
        [3, 1, 3],
        "a Trashed hook runs while the rows still exist"
    );

    let dirs = changes.dirs(fx.a);
    for dir in ["", "z"] {
        assert!(dirs.contains(dir), "{dir:?} is missing from {dirs:?}");
        let recent = fx
            .ops
            .self_writes()
            .is_recent(fx.a, &rel(dir), Duration::from_secs(60));
        assert!(recent, "{dir:?} is not remembered as written by us");
    }
}
