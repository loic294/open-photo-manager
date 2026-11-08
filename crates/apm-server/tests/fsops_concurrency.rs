//! File operations that overlap with scans, with each other and with recovery. A scan can run in
//! the moment between an operation's change on the disk and the transaction that records it: the
//! index must still end up with one row per file, and the rating, flag, tags and extracted data
//! of a file the user touched must survive. The deterministic cases run a scan at exactly that
//! moment; the stress tests let many operations, a scan loop and a recovery loop run at once.
//!
//! The `#[ignore]` tests measure throughput:
//! `cargo test --release -p apm-server --test fsops_concurrency -- --ignored --nocapture`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apm_core::dto::common::ConflictPolicy;
use apm_core::hash::sha1_reader;
use apm_core::paths::VolumeId;
use apm_core::sidecar::LinkKind;
use apm_server::fsops::{
    AppliedOp, FailAction, Failpoints, FsOps, Loc, MoveGroup, MoveOutcome, OpContext, OpHook,
    ReconcileReport, RestoreOutcome, SidecarAction, SidecarItem, SidecarOutcome, Step, TrashBatch,
    TrashOutcome, TrashRequest,
};
use apm_server::indexer::{Indexer, RunContext, ScanRequest, ScanSummary, ScanTarget};
use common::BASE_MTIME;
use common::fsops::{Fx, Memory, SplitMover, disk_files, loc, rel, request, shared};
use futures_util::future::BoxFuture;
use sqlx::SqliteConnection;

const PAYLOAD: &[u8] = b"the bytes of one photograph";
const EXACT: &[u8] = b"<x:xmpmeta>exact</x:xmpmeta>";
const STEM: &[u8] = b"<x:xmpmeta>stem</x:xmpmeta>";

fn cx() -> OpContext {
    OpContext::detached()
}

fn len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap()
}

// --- a scan at the worst moment --------------------------------------------------------------

#[derive(Default)]
struct RaceState {
    /// How many index transactions to let pass before the scan runs.
    skip: usize,
    scan: Option<(Indexer, ScanRequest)>,
    ran: Option<ScanSummary>,
}

/// Runs a scan inside an operation, at its `BeforeIndexTx` step: the change on the disk is done
/// and no database connection is held, exactly what a scan that happens to be running at that
/// moment sees. Once per arming.
#[derive(Clone, Default)]
struct Race(Arc<Mutex<RaceState>>);

impl Race {
    /// The scan runs at the `skip + 1`-th index transaction from now on.
    fn arm(&self, indexer: &Indexer, skip: usize, targets: Vec<ScanTarget>) {
        let mut state = self.0.lock().unwrap();
        state.skip = skip;
        state.scan = Some((
            indexer.clone(),
            ScanRequest {
                targets,
                ..ScanRequest::default()
            },
        ));
        state.ran = None;
    }

    fn failpoints(&self) -> Failpoints {
        let state = Arc::clone(&self.0);
        Failpoints::new(move |step| {
            if step != Step::BeforeIndexTx {
                return FailAction::Continue;
            }
            let due = {
                let mut state = state.lock().unwrap();
                if state.skip > 0 {
                    state.skip -= 1;
                    None
                } else {
                    state.scan.take()
                }
            };
            if let Some((indexer, request)) = due {
                let summary = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async move { indexer.scan(&request, &RunContext::noop()).await })
                })
                .unwrap();
                state.lock().unwrap().ran = Some(summary);
            }
            FailAction::Continue
        })
    }

    /// What the injected scan did. Fails when it did not run.
    fn ran(&self) -> ScanSummary {
        self.0
            .lock()
            .unwrap()
            .ran
            .clone()
            .expect("the scan did not run inside the operation")
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<AppliedOp>>);

impl Recorder {
    fn events(&self) -> Vec<AppliedOp> {
        self.0.lock().unwrap().clone()
    }
}

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

struct Rig {
    fx: Fx,
    race: Race,
    recorder: Arc<Recorder>,
}

impl Rig {
    /// Two volumes with a race and a recorder; `split` makes moves between them copy.
    async fn new(split: bool) -> Self {
        let race = Race::default();
        let recorder = Arc::new(Recorder::default());
        let (points, hook) = (race.failpoints(), Arc::clone(&recorder));
        let fx = Fx::with(move |ops, trees| {
            let ops = ops.with_failpoints(points).with_hook(hook);
            if split {
                ops.with_mover(Arc::new(SplitMover::new(&trees)))
            } else {
                ops
            }
        })
        .await;
        Self { fx, race, recorder }
    }

    fn arm(&self, skip: usize, targets: Vec<ScanTarget>) {
        self.race.arm(&self.fx.app.state.indexer, skip, targets);
    }

    /// Both volumes, completely.
    fn everything(&self) -> Vec<ScanTarget> {
        vec![ScanTarget::volume(self.fx.a), ScanTarget::volume(self.fx.b)]
    }

    /// Just the files of one folder of volume `a`.
    fn folder(&self, path: &str) -> Vec<ScanTarget> {
        vec![ScanTarget::dir(self.fx.a, rel(path))]
    }
}

fn moved(dst: Loc, cross_device: bool, content: &[u8], file_id: i64) -> MoveOutcome {
    MoveOutcome::Moved {
        dst,
        renamed: false,
        cross_device,
        bytes: len(content),
        file_id,
    }
}

fn trash_request(loc: Loc, batch: &TrashBatch) -> TrashRequest {
    TrashRequest {
        file_id: None,
        loc,
        expect: None,
        batch: batch.clone(),
        reason: "user".to_owned(),
    }
}

async fn trash(fx: &Fx, path: &str) -> TrashOutcome {
    fx.ops
        .trash_file(trash_request(loc(fx.a, path), &TrashBatch::adhoc()), &cx())
        .await
        .unwrap()
}

/// `(dst, file_id, bytes)` of a restore that did restore.
fn restored(outcome: RestoreOutcome) -> (Loc, Option<i64>, u64) {
    match outcome {
        RestoreOutcome::Restored {
            dst,
            file_id,
            bytes,
            ..
        } => (dst, file_id, bytes),
        RestoreOutcome::Skipped(reason) => panic!("expected a restore, got {reason:?}"),
    }
}

// --- moves -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_that_finds_the_moved_file_first_hands_its_row_to_the_move() {
    let rig = Rig::new(false).await;
    let fx = &rig.fx;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let known = fx.decorate(id, PAYLOAD).await;
    rig.arm(0, rig.everything());

    let outcome = fx
        .ops
        .move_file(
            request(
                loc(fx.a, "x/IMG_1.jpg"),
                loc(fx.a, "y/IMG_1.jpg"),
                ConflictPolicy::Smart,
            ),
            &cx(),
        )
        .await
        .unwrap();

    let scan = rig.race.ran();
    assert_eq!((scan.added, scan.removed), (1, 1), "{scan:?}");
    let new_id = fx.present(fx.a, "y/IMG_1.jpg", PAYLOAD, "after").await;
    assert_ne!(new_id, id, "the scanner's row is the one that stayed");
    assert_eq!(
        outcome,
        moved(loc(fx.a, "y/IMG_1.jpg"), false, PAYLOAD, new_id)
    );
    assert_eq!(
        fx.memory_of(new_id).await,
        known,
        "what the index knew about the file came along"
    );
    assert_eq!(fx.paths(fx.a).await, BTreeSet::from(["y/IMG_1.jpg".into()]));
    fx.assert_journal(&[("move", "done")], "after").await;
    assert_eq!(
        rig.recorder.events(),
        [AppliedOp::Moved {
            file_id: new_id,
            from: loc(fx.a, "x/IMG_1.jpg"),
            to: loc(fx.a, "y/IMG_1.jpg"),
        }]
    );
    fx.assert_in_sync().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_race_after_a_copy_across_devices() {
    let rig = Rig::new(true).await;
    let fx = &rig.fx;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let known = fx.decorate(id, PAYLOAD).await;
    rig.arm(0, rig.everything());

    let outcome = fx
        .ops
        .move_file(
            request(
                loc(fx.a, "x/IMG_1.jpg"),
                loc(fx.b, "y/IMG_1.jpg"),
                ConflictPolicy::Smart,
            ),
            &cx(),
        )
        .await
        .unwrap();

    let scan = rig.race.ran();
    assert_eq!((scan.added, scan.removed), (1, 1), "{scan:?}");
    let new_id = fx.present(fx.b, "y/IMG_1.jpg", PAYLOAD, "after").await;
    assert_eq!(
        outcome,
        moved(loc(fx.b, "y/IMG_1.jpg"), true, PAYLOAD, new_id)
    );
    assert_eq!(fx.memory_of(new_id).await, known);
    fx.gone(fx.a, "x/IMG_1.jpg", "after").await;
    assert!(fx.leftover_parts().is_empty());
    fx.assert_journal(&[("move", "done")], "after").await;
    assert_eq!(
        rig.recorder.events(),
        [AppliedOp::Moved {
            file_id: new_id,
            from: loc(fx.a, "x/IMG_1.jpg"),
            to: loc(fx.b, "y/IMG_1.jpg"),
        }]
    );
    fx.assert_in_sync().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_that_saw_only_the_destination_leaves_the_moved_row_where_it_is_wanted() {
    let rig = Rig::new(false).await;
    let fx = &rig.fx;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let known = fx.decorate(id, PAYLOAD).await;
    rig.arm(0, rig.folder("y"));

    let outcome = fx
        .ops
        .move_file(
            request(
                loc(fx.a, "x/IMG_1.jpg"),
                loc(fx.a, "y/IMG_1.jpg"),
                ConflictPolicy::Smart,
            ),
            &cx(),
        )
        .await
        .unwrap();

    let scan = rig.race.ran();
    assert_eq!((scan.added, scan.removed), (1, 0), "{scan:?}");
    assert_eq!(
        fx.present(fx.a, "y/IMG_1.jpg", PAYLOAD, "after").await,
        id,
        "the row that was at the source is the one that moved"
    );
    assert_eq!(outcome, moved(loc(fx.a, "y/IMG_1.jpg"), false, PAYLOAD, id));
    assert_eq!(fx.memory_of(id).await, known);
    assert_eq!(fx.paths(fx.a).await, BTreeSet::from(["y/IMG_1.jpg".into()]));
    fx.assert_journal(&[("move", "done")], "after").await;
    fx.assert_in_sync().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_that_saw_only_the_source_vanish_leaves_the_move_to_index_the_file() {
    let rig = Rig::new(false).await;
    let fx = &rig.fx;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let known = fx.decorate(id, PAYLOAD).await;
    rig.arm(0, rig.folder("x"));

    let outcome = fx
        .ops
        .move_file(
            request(
                loc(fx.a, "x/IMG_1.jpg"),
                loc(fx.a, "y/IMG_1.jpg"),
                ConflictPolicy::Smart,
            ),
            &cx(),
        )
        .await
        .unwrap();

    let scan = rig.race.ran();
    assert_eq!((scan.added, scan.removed), (0, 1), "{scan:?}");
    let new_id = fx.present(fx.a, "y/IMG_1.jpg", PAYLOAD, "after").await;
    assert_eq!(
        outcome,
        moved(loc(fx.a, "y/IMG_1.jpg"), false, PAYLOAD, new_id)
    );
    assert_eq!(
        fx.memory_of(new_id).await,
        known,
        "the data came from what the move saw before it renamed the file"
    );
    assert_eq!(fx.paths(fx.a).await, BTreeSet::from(["y/IMG_1.jpg".into()]));
    fx.assert_journal(&[("move", "done")], "after").await;
    fx.assert_in_sync().await;
}

// --- copies ------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_that_finds_a_copy_first_gives_it_what_its_source_knows() {
    let rig = Rig::new(false).await;
    let fx = &rig.fx;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let known = fx.decorate(id, PAYLOAD).await;
    rig.arm(0, rig.folder("y"));

    let outcome = fx
        .ops
        .copy_file(
            request(
                loc(fx.a, "x/IMG_1.jpg"),
                loc(fx.a, "y/IMG_1.jpg"),
                ConflictPolicy::Smart,
            ),
            &cx(),
        )
        .await
        .unwrap();

    let scan = rig.race.ran();
    assert_eq!((scan.added, scan.removed), (1, 0), "{scan:?}");
    let copy_id = fx.present(fx.a, "y/IMG_1.jpg", PAYLOAD, "copy").await;
    assert_ne!(copy_id, id);
    assert_eq!(
        outcome,
        moved(loc(fx.a, "y/IMG_1.jpg"), false, PAYLOAD, copy_id)
    );
    assert_eq!(fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, "source").await, id);
    assert_eq!(fx.memory_of(id).await, known, "the source is untouched");
    assert_eq!(fx.memory_of(copy_id).await, known);
    fx.assert_journal(&[("copy", "done")], "after").await;
    assert_eq!(
        rig.recorder.events(),
        [AppliedOp::Copied {
            source_file_id: Some(id),
            file_id: copy_id,
            from: loc(fx.a, "x/IMG_1.jpg"),
            to: loc(fx.a, "y/IMG_1.jpg"),
        }]
    );
    fx.assert_in_sync().await;
}

// --- restores ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_that_finds_a_restored_file_first_gives_it_what_the_trash_remembered() {
    let rig = Rig::new(false).await;
    let fx = &rig.fx;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let known = fx.decorate(id, PAYLOAD).await;
    let item = trash(fx, "x/IMG_1.jpg").await;
    assert!(fx.trash_items().await[0].has_meta);
    rig.arm(0, rig.folder("x"));

    let outcome = fx
        .ops
        .restore(item.trash_item_id, ConflictPolicy::Smart, &cx())
        .await
        .unwrap();

    let scan = rig.race.ran();
    assert_eq!((scan.added, scan.removed), (1, 0), "{scan:?}");
    let (dst, file_id, bytes) = restored(outcome);
    assert_eq!((dst, bytes), (loc(fx.a, "x/IMG_1.jpg"), len(PAYLOAD)));
    let new_id = fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, "restored").await;
    assert_eq!(file_id, Some(new_id));
    assert_eq!(fx.memory_of(new_id).await, known);
    assert!(fx.trash_items().await.is_empty());
    assert_eq!(
        rig.recorder.events().last(),
        Some(&AppliedOp::Restored {
            file_id: Some(new_id),
            trash_item_id: item.trash_item_id,
            from: loc(fx.a, item.trash_path.as_str()),
            to: loc(fx.a, "x/IMG_1.jpg"),
            is_dir: false,
        })
    );
    fx.assert_journal(&[("trash", "done"), ("restore", "done")], "after")
        .await;
    fx.assert_in_sync().await;
}

// --- the trash ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_that_drops_a_file_while_it_is_being_trashed_loses_nothing() {
    let rig = Rig::new(false).await;
    let fx = &rig.fx;
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.scan().await;
    let id = fx.row_id(fx.a, "x/IMG_1.jpg").await;
    let known = fx.decorate(id, PAYLOAD).await;
    rig.arm(0, rig.folder("x"));

    let item = trash(fx, "x/IMG_1.jpg").await;

    let scan = rig.race.ran();
    assert_eq!((scan.added, scan.removed), (0, 1), "{scan:?}");
    assert_eq!(
        (item.files_removed, item.bytes),
        (0, len(PAYLOAD)),
        "the scan removed the row, not the operation"
    );
    let rows = fx.trash_items().await;
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].has_meta,
        "what the index knew about the file is remembered although the row was gone"
    );
    assert_eq!(
        rig.recorder.events(),
        [AppliedOp::Trashed {
            file_ids: Vec::new(),
            from: loc(fx.a, "x/IMG_1.jpg"),
            trash_item_id: item.trash_item_id,
            trash_path: item.trash_path.clone(),
            is_dir: false,
        }],
        "a row a scan already removed is not listed"
    );

    let (_, new_id, _) = restored(
        fx.ops
            .restore(item.trash_item_id, ConflictPolicy::Smart, &cx())
            .await
            .unwrap(),
    );
    let new_id = new_id.unwrap();
    assert_eq!(fx.row_id(fx.a, "x/IMG_1.jpg").await, new_id);
    assert_eq!(
        fx.memory_of(new_id).await,
        known,
        "the rating, the flag, the tags and the extracted data came back"
    );
    fx.present(fx.a, "x/IMG_1.jpg", PAYLOAD, "restored").await;
    fx.assert_in_sync().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_that_drops_a_folder_while_it_is_being_trashed_leaves_its_size_known() {
    let rig = Rig::new(false).await;
    let fx = &rig.fx;
    fx.ta.write("x/a.jpg", b"0123456789");
    fx.ta.write("x/b.jpg", b"01234567890123456789");
    fx.scan().await;
    rig.arm(0, rig.everything());

    let item = trash(fx, "x").await;

    let scan = rig.race.ran();
    assert_eq!((scan.added, scan.removed), (0, 2), "{scan:?}");
    assert_eq!(
        (item.files_removed, item.bytes),
        (0, 30),
        "the scan removed the rows, the folder still had 30 bytes"
    );
    assert_eq!(fx.scalar("SELECT size FROM trash_items").await, 30);
    let meta: String = sqlx::query_scalar("SELECT meta_json FROM trash_items")
        .fetch_one(fx.app.state.db.read())
        .await
        .unwrap();
    let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
    assert_eq!((&meta["files"], &meta["bytes"]), (&2.into(), &30.into()));
    assert_eq!(
        rig.recorder.events(),
        [AppliedOp::Trashed {
            file_ids: Vec::new(),
            from: loc(fx.a, "x"),
            trash_item_id: item.trash_item_id,
            trash_path: item.trash_path.clone(),
            is_dir: true,
        }]
    );

    let (dst, _, bytes) = restored(
        fx.ops
            .restore(item.trash_item_id, ConflictPolicy::Smart, &cx())
            .await
            .unwrap(),
    );
    assert_eq!((dst, bytes), (loc(fx.a, "x"), 30));
    fx.scan().await;
    assert_eq!(
        fx.paths(fx.a).await,
        BTreeSet::from(["x/a.jpg".into(), "x/b.jpg".into()])
    );
    fx.assert_in_sync().await;
}

// --- groups ------------------------------------------------------------------------------------

/// `x/IMG_1.jpg`, its exact sidecar and its stem sidecar, scanned: `(owner, exact, stem)` rows.
async fn trio(fx: &Fx) -> (i64, i64, i64) {
    fx.ta.write("x/IMG_1.jpg", PAYLOAD);
    fx.ta.write("x/IMG_1.jpg.xmp", EXACT);
    fx.ta.write("x/IMG_1.xmp", STEM);
    fx.scan().await;
    (
        fx.row_id(fx.a, "x/IMG_1.jpg").await,
        fx.row_id(fx.a, "x/IMG_1.jpg.xmp").await,
        fx.row_id(fx.a, "x/IMG_1.xmp").await,
    )
}

fn trio_group(fx: &Fx) -> MoveGroup {
    let sidecar = |name: &str, kind| SidecarItem {
        file_id: None,
        src: loc(fx.a, &format!("x/{name}")),
        kind,
        action: SidecarAction::Move,
    };
    MoveGroup {
        owner: request(
            loc(fx.a, "x/IMG_1.jpg"),
            loc(fx.a, "y/IMG_1.jpg"),
            ConflictPolicy::Smart,
        ),
        sidecars: vec![
            sidecar("IMG_1.jpg.xmp", LinkKind::Exact),
            sidecar("IMG_1.xmp", LinkKind::Stem),
        ],
    }
}

#[derive(Debug, Clone, Copy)]
enum Scan {
    Everything,
    SourceFolder,
    DestinationFolder,
}

/// A group of three files moves while a scan runs right after its `nth` member (1-based) changed
/// on the disk.
async fn group_meets_a_scan(nth: usize, scan: Scan) {
    let label = format!("member {nth}, {scan:?}");
    let rig = Rig::new(false).await;
    let fx = &rig.fx;
    let (owner, ..) = trio(fx).await;
    let known = fx.decorate(owner, PAYLOAD).await;
    let targets = match scan {
        Scan::Everything => rig.everything(),
        Scan::SourceFolder => rig.folder("x"),
        Scan::DestinationFolder => rig.folder("y"),
    };
    rig.arm(nth - 1, targets);

    let outcome = fx.ops.move_group(trio_group(fx), &cx()).await.unwrap();

    let ran = rig.race.ran();
    assert!(ran.added + ran.removed >= 1, "{label}: {ran:?}");
    let owner_row = fx.present(fx.a, "y/IMG_1.jpg", PAYLOAD, &label).await;
    let exact_row = fx.present(fx.a, "y/IMG_1.jpg.xmp", EXACT, &label).await;
    let stem_row = fx.present(fx.a, "y/IMG_1.xmp", STEM, &label).await;
    assert_eq!(
        outcome.owner,
        moved(loc(fx.a, "y/IMG_1.jpg"), false, PAYLOAD, owner_row),
        "{label}"
    );
    assert_eq!(
        outcome.sidecars,
        [
            SidecarOutcome::Moved {
                dst: loc(fx.a, "y/IMG_1.jpg.xmp"),
                file_id: exact_row
            },
            SidecarOutcome::Moved {
                dst: loc(fx.a, "y/IMG_1.xmp"),
                file_id: stem_row
            },
        ],
        "{label}"
    );
    // A scan that links the sidecars to the owner by itself asks the pipeline to read the owner
    // again (`meta_state` pending): that is its rule, and the only thing that may differ.
    let after = fx.memory_of(owner_row).await;
    assert_eq!(
        Memory {
            meta_state: known.meta_state,
            ..after
        },
        known,
        "{label}"
    );
    assert_eq!(
        fx.paths(fx.a).await,
        BTreeSet::from([
            "y/IMG_1.jpg".to_owned(),
            "y/IMG_1.jpg.xmp".to_owned(),
            "y/IMG_1.xmp".to_owned()
        ]),
        "{label}"
    );
    assert_eq!(
        fx.links().await,
        BTreeSet::from([
            (
                "a:y/IMG_1.jpg".to_owned(),
                "a:y/IMG_1.jpg.xmp".to_owned(),
                "exact".to_owned()
            ),
            (
                "a:y/IMG_1.jpg".to_owned(),
                "a:y/IMG_1.xmp".to_owned(),
                "stem".to_owned()
            ),
        ]),
        "{label}: the sidecars follow their owner"
    );
    fx.assert_journal(
        &[("move", "done"), ("move", "done"), ("move", "done")],
        &label,
    )
    .await;
    let events = rig.recorder.events();
    assert_eq!(events.len(), 3, "{label}: {events:?}");
    for event in &events {
        let AppliedOp::Moved { file_id, to, .. } = event else {
            panic!("{label}: expected a move, got {event:?}");
        };
        assert_eq!(
            Some(*file_id),
            fx.row(to.volume_id, to.rel.as_str()).await.map(|row| row.0),
            "{label}: the hook saw the row that is at {}",
            to.rel
        );
    }
    fx.assert_in_sync_for(&label).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_after_any_member_of_a_group_leaves_the_group_whole() {
    for nth in 1..=3 {
        for scan in [
            Scan::Everything,
            Scan::SourceFolder,
            Scan::DestinationFolder,
        ] {
            group_meets_a_scan(nth, scan).await;
        }
    }
}

// --- many operations at once ---------------------------------------------------------------------

/// What happens to one file of a storm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Renamed into new folders of its own volume.
    Renamed,
    /// Moved to the other volume.
    MovedAway,
    /// Copied to the other volume; the original stays.
    Copied,
    /// Trashed and restored right away.
    Bounced,
    /// Trashed; the test restores it when the storm is over.
    Dropped,
}

fn fate(i: i64) -> Fate {
    match i % 5 {
        0 => Fate::Renamed,
        1 => Fate::MovedAway,
        2 => Fate::Copied,
        3 => Fate::Bounced,
        _ => Fate::Dropped,
    }
}

/// Where the files of a storm are before and after. Every file has a number and a content of its own.
struct Layout {
    folders: i64,
    a: VolumeId,
    b: VolumeId,
}

impl Layout {
    fn folder(&self, i: i64) -> String {
        format!("d{:02}", i % self.folders)
    }

    fn origin(&self, i: i64) -> Loc {
        loc(self.a, &format!("in/{}/photo-{i}.jpg", self.folder(i)))
    }

    fn renamed(&self, i: i64) -> Loc {
        loc(
            self.a,
            &format!("sub/{}/e{}/photo-{i}.jpg", self.folder(i), i % 3),
        )
    }

    fn away(&self, i: i64) -> Loc {
        loc(self.b, &format!("moved/{}/photo-{i}.jpg", self.folder(i)))
    }

    fn copy(&self, i: i64) -> Loc {
        loc(self.b, &format!("copies/{}/photo-{i}.jpg", self.folder(i)))
    }

    /// Where the row that existed before the storm ends up.
    fn home(&self, i: i64) -> Loc {
        match fate(i) {
            Fate::Renamed => self.renamed(i),
            Fate::MovedAway => self.away(i),
            Fate::Copied | Fate::Bounced | Fate::Dropped => self.origin(i),
        }
    }

    /// Every file the storm leaves, with its number. `dropped_back` says whether the files that
    /// were trashed for good are restored.
    fn left(&self, files: i64, dropped_back: bool) -> BTreeMap<(VolumeId, String), i64> {
        let mut left = BTreeMap::new();
        let mut put = |at: Loc, i: i64| left.insert((at.volume_id, at.rel.as_str().to_owned()), i);
        for i in 0..files {
            match fate(i) {
                Fate::Renamed => put(self.renamed(i), i),
                Fate::MovedAway => put(self.away(i), i),
                Fate::Copied => {
                    put(self.origin(i), i);
                    put(self.copy(i), i)
                }
                Fate::Bounced => put(self.origin(i), i),
                Fate::Dropped if dropped_back => put(self.origin(i), i),
                Fate::Dropped => None,
            };
        }
        left
    }
}

fn content(i: i64) -> Vec<u8> {
    format!("photo {i}, one of the files of a storm").into_bytes()
}

/// What the test gives file `i` before the storm: user data, and what the pipeline would have found.
struct Seed {
    rating: i64,
    flag: i64,
    width: i64,
    tag: String,
}

fn seed(i: i64) -> Seed {
    Seed {
        rating: i % 5 + 1,
        flag: i % 3 - 1,
        width: 1000 + i,
        tag: format!("tag{}", i % 4),
    }
}

async fn give_seeds(fx: &Fx, layout: &Layout, files: i64) {
    let mut tx = fx.app.state.db.begin_write().await.unwrap();
    for tag in 0..4 {
        sqlx::query("INSERT OR IGNORE INTO tags (name) VALUES (?)")
            .bind(format!("tag{tag}"))
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    for i in 0..files {
        let at = layout.origin(i);
        let (volume, dir, name) = (
            at.volume_id,
            at.dir().as_str().to_owned(),
            at.name().to_owned(),
        );
        let seed = seed(i);
        sqlx::query(
            "UPDATE files SET rating = ?, flag = ?, width = ?, height = 600, meta_state = 1 \
             WHERE volume_id = ? AND dir_path = ? AND name = ?",
        )
        .bind(seed.rating)
        .bind(seed.flag)
        .bind(seed.width)
        .bind(volume)
        .bind(&dir)
        .bind(&name)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO file_tags (file_id, tag_id) SELECT f.id, t.id FROM files f, tags t \
             WHERE f.volume_id = ? AND f.dir_path = ? AND f.name = ? AND t.name = ?",
        )
        .bind(volume)
        .bind(&dir)
        .bind(&name)
        .bind(seed.tag)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

/// The row is what `give_seeds` made it, whatever happened to the file since. The one thing it may
/// have lost is the pipeline state: a scan that listed the file before an operation's index
/// transaction and inserts it after it hits the row in the scanner's upsert, which sends the row
/// through the pipeline again (`meta_state` pending, no hash) and touches nothing else. Returns
/// whether that happened.
fn assert_remembers(memory: &Memory, i: i64, label: &str) -> bool {
    let seed = seed(i);
    assert_eq!(
        (memory.rating, memory.flag, memory.width, memory.height),
        (seed.rating, seed.flag, Some(seed.width), Some(600)),
        "{label}: file {i} forgot what it knew"
    );
    assert_eq!(memory.tags, [seed.tag], "{label}: file {i} lost its tag");
    match memory.meta_state {
        1 => false,
        0 => {
            assert_eq!(
                (memory.hash.as_deref(), memory.hash_state),
                (None, 0),
                "{label}: file {i} is pending but kept a hash"
            );
            true
        }
        other => panic!("{label}: file {i} has the pipeline state {other}"),
    }
}

async fn id_at(fx: &Fx, at: &Loc) -> Option<i64> {
    fx.row(at.volume_id, at.rel.as_str()).await.map(|row| row.0)
}

/// Shared by the failpoints and the loops of a storm.
struct Pacer {
    /// Stand still at every step until two recovery passes have looked at the operation.
    hold: bool,
    /// Set when the storm is over: the failpoints do nothing.
    calm: AtomicBool,
    scanning: AtomicBool,
    scans: AtomicUsize,
    /// How often an operation reached its index transaction while a scan was running.
    overlapped: AtomicUsize,
    /// How often an operation stood still at a step.
    held: AtomicUsize,
    reports: Mutex<Vec<ReconcileReport>>,
}

impl Pacer {
    fn new(hold: bool) -> Self {
        Self {
            hold,
            calm: AtomicBool::new(false),
            scanning: AtomicBool::new(false),
            scans: AtomicUsize::new(0),
            overlapped: AtomicUsize::new(0),
            held: AtomicUsize::new(0),
            reports: Mutex::default(),
        }
    }

    fn failpoints(self: &Arc<Self>) -> Failpoints {
        let pacer = Arc::clone(self);
        Failpoints::new(move |step| {
            if pacer.calm.load(Ordering::SeqCst) {
                return FailAction::Continue;
            }
            if pacer.hold {
                pacer.stand_still();
            } else if step == Step::BeforeIndexTx {
                pacer.wait_for_a_scan();
            }
            if step == Step::BeforeIndexTx && pacer.scanning.load(Ordering::SeqCst) {
                pacer.overlapped.fetch_add(1, Ordering::SeqCst);
            }
            FailAction::Continue
        })
    }

    /// Lets an operation wait, before its index transaction, for a scan that is running, and gives
    /// the scan a moment to get into its work.
    fn wait_for_a_scan(&self) {
        tokio::task::block_in_place(|| {
            let began = Instant::now();
            while !self.scanning.load(Ordering::SeqCst) && began.elapsed() < Duration::from_secs(2)
            {
                std::thread::sleep(Duration::from_micros(100));
            }
            std::thread::sleep(Duration::from_micros(300));
        });
    }

    /// Holds the operation (its journal row is pending) until two recovery passes have completed:
    /// the second began after the operation got here, so it saw the row.
    fn stand_still(&self) {
        let start = self.reports.lock().unwrap().len();
        tokio::task::block_in_place(|| {
            let began = Instant::now();
            loop {
                {
                    let reports = self.reports.lock().unwrap();
                    if let Some(second) = reports.get(start + 1) {
                        assert!(
                            second.still_pending >= 1,
                            "a recovery pass that began while an operation stood still saw no \
                             pending journal row: {second:?}"
                        );
                        return;
                    }
                }
                assert!(
                    began.elapsed() < Duration::from_secs(30),
                    "the recovery loop stopped"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        self.held.fetch_add(1, Ordering::SeqCst);
    }
}

/// Runs the operation file `i` goes through; every one of them must succeed.
async fn go_through(ops: &FsOps, layout: &Layout, i: i64) {
    let cx = OpContext::detached();
    let from = layout.origin(i);
    let transfer = |outcome: MoveOutcome| {
        assert!(
            matches!(outcome, MoveOutcome::Moved { .. }),
            "file {i}: {outcome:?}"
        );
    };
    match fate(i) {
        Fate::Renamed => {
            let request = request(from, layout.renamed(i), ConflictPolicy::Smart);
            transfer(ops.move_file(request, &cx).await.unwrap());
        }
        Fate::MovedAway => {
            let request = request(from, layout.away(i), ConflictPolicy::Smart);
            transfer(ops.move_file(request, &cx).await.unwrap());
        }
        Fate::Copied => {
            let request = request(from, layout.copy(i), ConflictPolicy::Smart);
            transfer(ops.copy_file(request, &cx).await.unwrap());
        }
        Fate::Bounced => {
            let trashed = ops
                .trash_file(trash_request(from, &TrashBatch::adhoc()), &cx)
                .await
                .unwrap();
            let outcome = ops
                .restore(trashed.trash_item_id, ConflictPolicy::Smart, &cx)
                .await
                .unwrap();
            assert!(
                matches!(outcome, RestoreOutcome::Restored { .. }),
                "file {i}: {outcome:?}"
            );
        }
        Fate::Dropped => {
            ops.trash_file(trash_request(from, &TrashBatch::adhoc()), &cx)
                .await
                .unwrap();
        }
    }
}

struct Storm {
    files: i64,
    folders: i64,
    /// Moves between the volumes copy and verify.
    split: bool,
    /// Every operation stands still at every step until two recovery passes have seen it.
    hold: bool,
    workers: usize,
}

/// The paths of `disk` without the trash, and the trash on its own.
fn without_trash(
    disk: BTreeMap<String, Vec<u8>>,
) -> (BTreeMap<String, Vec<u8>>, BTreeMap<String, Vec<u8>>) {
    disk.into_iter()
        .partition(|(path, _)| !path.starts_with(".apm-trash/"))
}

/// What differs between two sets of files, in a few words.
fn differences(got: &BTreeMap<String, Vec<u8>>, want: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut found = Vec::new();
    for (path, bytes) in want {
        match got.get(path) {
            None => found.push(format!("{path} is missing")),
            Some(other) if other != bytes => found.push(format!("{path} has other content")),
            Some(_) => {}
        }
    }
    for path in got.keys().filter(|path| !want.contains_key(*path)) {
        found.push(format!("{path} should not be there"));
    }
    found.truncate(10);
    found
}

/// Many files go through many operations at once while scans and recovery passes run over them
/// without a pause. Whatever the timing, the end is one row per file, the user's data on it, a
/// journal whose every row is done and a recovery that never touched a running operation.
async fn storm(config: Storm) {
    let Storm {
        files,
        folders,
        split,
        hold,
        workers,
    } = config;
    let pacer = Arc::new(Pacer::new(hold));
    let recorder = Arc::new(Recorder::default());
    let fx = {
        let (points, hook) = (pacer.failpoints(), Arc::clone(&recorder));
        Fx::with(move |ops, trees| {
            let ops = ops.with_failpoints(points).with_hook(hook);
            if split {
                ops.with_mover(Arc::new(SplitMover::new(&trees)))
            } else {
                ops
            }
        })
        .await
    };
    let layout = Arc::new(Layout {
        folders,
        a: fx.a,
        b: fx.b,
    });
    for i in 0..files {
        fx.ta
            .write_at(layout.origin(i).rel.as_str(), &content(i), BASE_MTIME);
    }
    fx.scan().await;
    give_seeds(&fx, &layout, files).await;
    let mut first_ids = BTreeMap::new();
    for i in 0..files {
        first_ids.insert(i, id_at(&fx, &layout.origin(i)).await.unwrap());
    }

    let ops = shared(&fx.ops);
    let stop = Arc::new(AtomicBool::new(false));
    let scanner = tokio::spawn({
        let (indexer, pacer, stop) = (
            fx.app.state.indexer.clone(),
            Arc::clone(&pacer),
            Arc::clone(&stop),
        );
        async move {
            while !stop.load(Ordering::SeqCst) {
                pacer.scanning.store(true, Ordering::SeqCst);
                indexer
                    .scan(&ScanRequest::default(), &RunContext::noop())
                    .await
                    .unwrap();
                pacer.scanning.store(false, Ordering::SeqCst);
                pacer.scans.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
            }
        }
    });
    let recovery = tokio::spawn({
        let (ops, pacer, stop) = (Arc::clone(&ops), Arc::clone(&pacer), Arc::clone(&stop));
        async move {
            while !stop.load(Ordering::SeqCst) {
                let report = ops.reconcile().await.unwrap();
                pacer.reports.lock().unwrap().push(report);
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
    });
    let began = Instant::now();
    let running: Vec<_> = (0..workers)
        .map(|worker| {
            let (ops, layout) = (Arc::clone(&ops), Arc::clone(&layout));
            tokio::spawn(async move {
                let step = i64::try_from(workers).unwrap();
                let mut i = i64::try_from(worker).unwrap();
                while i < files {
                    go_through(&ops, &layout, i).await;
                    i += step;
                }
            })
        })
        .collect();
    for worker in running {
        worker.await.unwrap();
    }
    let took = began.elapsed();
    stop.store(true, Ordering::SeqCst);
    scanner.await.unwrap();
    recovery.await.unwrap();
    pacer.calm.store(true, Ordering::SeqCst);

    // The recovery passes never touched a running operation.
    let reports = pacer.reports.lock().unwrap().clone();
    assert!(!reports.is_empty(), "the recovery loop never ran");
    for (n, report) in reports.iter().enumerate() {
        assert_eq!(
            (
                report.completed,
                report.abandoned,
                report.parts_removed,
                report.stray_parts_swept,
                report.links_dropped,
                report.rescan.len(),
                report.errors.len()
            ),
            (0, 0, 0, 0, 0, 0, 0),
            "recovery pass {n} changed something: {report:?}"
        );
        assert_eq!(
            report.examined, report.still_pending,
            "recovery pass {n} looked at a row that was not running: {report:?}"
        );
    }
    let busy = reports.iter().filter(|r| r.still_pending > 0).count();
    let quiet = ops.reconcile().await.unwrap();
    assert_eq!(
        (
            quiet.examined,
            quiet.completed,
            quiet.abandoned,
            quiet.errors.len()
        ),
        (0, 0, 0, 0),
        "{quiet:?}"
    );
    let scans = pacer.scans.load(Ordering::SeqCst);
    assert!(scans >= 1, "no scan completed while the operations ran");
    if hold {
        assert!(pacer.held.load(Ordering::SeqCst) > 0, "nothing stood still");
        assert!(busy > 0, "no recovery pass met a running operation");
    } else {
        assert!(
            pacer.overlapped.load(Ordering::SeqCst) > 0,
            "no operation met a scan"
        );
    }

    // The disk, before any scan repairs anything.
    let left = layout.left(files, false);
    let want_on = |volume: VolumeId| -> BTreeMap<String, Vec<u8>> {
        left.iter()
            .filter(|((at, _), _)| *at == volume)
            .map(|((_, path), i)| (path.clone(), content(*i)))
            .collect()
    };
    let (disk_a, trash) = without_trash(disk_files(fx.ta.root()));
    let (disk_b, trash_b) = without_trash(disk_files(fx.tb.root()));
    assert_eq!(
        differences(&disk_a, &want_on(fx.a)),
        Vec::<String>::new(),
        "volume a"
    );
    assert_eq!(
        differences(&disk_b, &want_on(fx.b)),
        Vec::<String>::new(),
        "volume b"
    );
    assert!(trash_b.is_empty(), "volume b has a trash: {trash_b:?}");
    let dropped: BTreeMap<String, i64> = (0..files)
        .filter(|&i| fate(i) == Fate::Dropped)
        .map(|i| (layout.origin(i).rel.as_str().to_owned(), i))
        .collect();
    let trashed: BTreeSet<Vec<u8>> = trash.into_values().collect();
    let expected_trash: BTreeSet<Vec<u8>> = dropped.values().map(|&i| content(i)).collect();
    assert_eq!(trashed, expected_trash, "the trash holds the dropped files");
    assert_eq!(fx.leftover_parts(), Vec::<String>::new());

    // The index: every file has its row, describing the file as it is now, before a scan could
    // add anything.
    let mut redone = 0;
    for ((volume, path), &i) in &left {
        let label = format!("{}:{path}", fx.label(*volume));
        let (id, size, mtime) = fx
            .row(*volume, path)
            .await
            .unwrap_or_else(|| panic!("{label}: file {i} has no row"));
        assert_eq!(
            (size, mtime),
            (i64::try_from(content(i).len()).unwrap(), BASE_MTIME),
            "{label}: the row describes another file"
        );
        redone += usize::from(assert_remembers(&fx.memory_of(id).await, i, &label));
    }

    // A last scan removes what the storm's scans left behind, and adds nothing.
    let last = fx
        .app
        .state
        .indexer
        .scan(
            &ScanRequest {
                confirm_removals: true,
                ..ScanRequest::default()
            },
            &RunContext::noop(),
        )
        .await
        .unwrap();
    assert_eq!(
        (last.added, last.changed, last.held),
        (0, 0, 0),
        "the last scan found work to add: {last:?}"
    );
    for volume in [fx.a, fx.b] {
        let want: BTreeSet<String> = left
            .keys()
            .filter(|(at, _)| *at == volume)
            .map(|(_, path)| path.clone())
            .collect();
        let got = fx.paths(volume).await;
        assert_eq!(
            got.symmetric_difference(&want).take(10).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "the rows of volume {}",
            fx.label(volume)
        );
    }

    // What was trashed for good remembers everything, and comes back whole.
    let items = fx.trash_items().await;
    assert_eq!(items.len(), dropped.len(), "{items:?}");
    for item in &items {
        let i = *dropped
            .get(&item.orig_path)
            .unwrap_or_else(|| panic!("an unknown trash item: {item:?}"));
        assert!(
            item.has_meta,
            "the trash item of file {i} forgot what the index knew: {item:?}"
        );
        assert_eq!(
            (item.volume_id, item.is_dir, item.reason.as_deref()),
            (fx.a, false, Some("user")),
            "file {i}"
        );
    }
    for item in &items {
        let i = dropped[&item.orig_path];
        let outcome = ops
            .restore(item.id, ConflictPolicy::Smart, &OpContext::detached())
            .await
            .unwrap();
        let (dst, file_id, bytes) = restored(outcome);
        assert_eq!(
            (&dst, bytes),
            (&layout.origin(i), u64::try_from(content(i).len()).unwrap())
        );
        assert_eq!(file_id, id_at(&fx, &dst).await, "file {i}");
    }
    assert!(fx.trash_items().await.is_empty());
    let mut new_ids = 0;
    for ((volume, path), &i) in &layout.left(files, true) {
        let label = format!("{}:{path}", fx.label(*volume));
        let id = fx.row_id(*volume, path).await;
        assert_remembers(&fx.memory_of(id).await, i, &label);
    }
    for i in 0..files {
        if id_at(&fx, &layout.home(i)).await != Some(first_ids[&i]) {
            new_ids += 1;
        }
    }
    let (on_a, on_b) = (fx.paths(fx.a).await.len(), fx.paths(fx.b).await.len());
    assert_eq!(
        on_a + on_b,
        layout.left(files, true).len(),
        "one row per file"
    );

    // The journal: every operation once, all done.
    let mut journal: BTreeMap<(String, String), i64> = BTreeMap::new();
    for row in fx.journal().await {
        *journal.entry(row).or_default() += 1;
    }
    let of = |wanted: &[Fate]| {
        i64::try_from((0..files).filter(|&i| wanted.contains(&fate(i))).count()).unwrap()
    };
    let done = |op: &str, count: i64| ((op.to_owned(), "done".to_owned()), count);
    assert_eq!(
        journal,
        BTreeMap::from([
            done("move", of(&[Fate::Renamed, Fate::MovedAway])),
            done("copy", of(&[Fate::Copied])),
            done("trash", of(&[Fate::Bounced, Fate::Dropped])),
            done("restore", of(&[Fate::Bounced, Fate::Dropped])),
        ])
    );
    assert_eq!(fx.pending_rows().await, 0);

    // The hooks: once per operation, each about the row that is there now.
    let mut seen = BTreeMap::<&str, i64>::new();
    for event in recorder.events() {
        match &event {
            AppliedOp::Moved { file_id, to, .. } | AppliedOp::Copied { file_id, to, .. } => {
                let kind = if matches!(event, AppliedOp::Moved { .. }) {
                    "moved"
                } else {
                    "copied"
                };
                *seen.entry(kind).or_default() += 1;
                assert_eq!(Some(*file_id), id_at(&fx, to).await, "{event:?}");
            }
            AppliedOp::Trashed { file_ids, .. } => {
                *seen.entry("trashed").or_default() += 1;
                assert!(file_ids.len() <= 1, "{event:?}");
            }
            AppliedOp::Restored { file_id, to, .. } => {
                *seen.entry("restored").or_default() += 1;
                assert_eq!(*file_id, id_at(&fx, to).await, "{event:?}");
            }
            AppliedOp::Purged { .. } => panic!("unexpected {event:?}"),
        }
    }
    assert_eq!(
        seen,
        BTreeMap::from([
            ("moved", of(&[Fate::Renamed, Fate::MovedAway])),
            ("copied", of(&[Fate::Copied])),
            ("trashed", of(&[Fate::Bounced, Fate::Dropped])),
            ("restored", of(&[Fate::Bounced, Fate::Dropped])),
        ])
    );
    fx.assert_in_sync().await;
    eprintln!(
        "storm of {files} files in {took:?}: {scans} scans ({} met an operation), {} recovery \
         passes ({busy} met a running operation, {} operations stood still), {new_ids} rows got \
         a new id, {redone} rows were sent through the pipeline again by a scan, the last scan \
         removed {} stale rows",
        pacer.overlapped.load(Ordering::SeqCst),
        reports.len(),
        pacer.held.load(Ordering::SeqCst),
        last.removed,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_hundred_files_are_moved_copied_and_trashed_while_scans_and_recovery_run() {
    storm(Storm {
        files: 500,
        folders: 19,
        split: false,
        hold: false,
        workers: 4,
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_leaves_every_running_operation_alone_at_every_step() {
    storm(Storm {
        files: 30,
        folders: 7,
        split: true,
        hold: true,
        workers: 4,
    })
    .await;
}

// --- throughput ----------------------------------------------------------------------------------

fn bulk(volume: VolumeId, i: i64) -> Loc {
    loc(volume, &format!("bulk/d{:02}/img-{i}.jpg", i % 50))
}

fn sorted(volume: VolumeId, i: i64) -> Loc {
    loc(volume, &format!("sorted/d{:02}/img-{i}.jpg", i % 50))
}

fn abroad(volume: VolumeId, i: i64) -> Loc {
    loc(volume, &format!("abroad/d{:02}/img-{i}.jpg", i % 50))
}

/// One line about operations that ran one after the other, or in parallel (`total` is then the
/// time of them all).
fn summarize(what: &str, mut each: Vec<Duration>, total: Duration) -> String {
    each.sort_unstable();
    format!(
        "{what}: {} in {total:.2?} = {:.0}/s (median {:.2?}, p95 {:.2?}, slowest {:.2?})",
        each.len(),
        each.len() as f64 / total.as_secs_f64(),
        each[each.len() / 2],
        each[each.len() * 95 / 100],
        each[each.len() - 1],
    )
}

/// Runs `go` for every number of `files` on `workers` tasks and times each call.
async fn timed_in_parallel<F, Fut>(files: i64, workers: usize, go: F) -> (Vec<Duration>, Duration)
where
    F: Fn(i64) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let began = Instant::now();
    let running: Vec<_> = (0..workers)
        .map(|worker| {
            let go = go.clone();
            tokio::spawn(async move {
                let step = i64::try_from(workers).unwrap();
                let mut times = Vec::new();
                let mut i = i64::try_from(worker).unwrap();
                while i < files {
                    let one = Instant::now();
                    go(i).await;
                    times.push(one.elapsed());
                    i += step;
                }
                times
            })
        })
        .collect();
    let mut all = Vec::new();
    for worker in running {
        all.extend(worker.await.unwrap());
    }
    (all, began.elapsed())
}

/// Not part of the normal run: how fast the module moves thousands of small files, and that
/// nothing is lost on the way. Prints rates; asserts only that every file and row is right.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "throughput: run with --release -- --ignored --nocapture"]
async fn thousands_of_files_move_and_come_back_without_a_trace() {
    const FILES: i64 = 5_000;
    const TRASHED: i64 = 1_000;
    const ABROAD: i64 = 1_000;
    const WORKERS: usize = 8;
    let fx = Fx::with(|ops, trees| ops.with_mover(Arc::new(SplitMover::new(&trees)))).await;
    let (a, b) = (fx.a, fx.b);
    for i in 0..FILES {
        fx.ta
            .write_at(bulk(a, i).rel.as_str(), &content(i), BASE_MTIME);
    }
    let began = Instant::now();
    let summary = fx.scan().await;
    eprintln!(
        "scan of {FILES} new files: {:.2?} ({} added)",
        began.elapsed(),
        summary.added
    );
    assert_eq!(summary.added, u64::try_from(FILES).unwrap());
    let mut first_ids = BTreeMap::new();
    for i in 0..FILES {
        first_ids.insert(i, id_at(&fx, &bulk(a, i)).await.unwrap());
    }

    let ops = shared(&fx.ops);
    let cx = OpContext::detached();
    let mut each = Vec::new();
    let began = Instant::now();
    for i in 0..FILES {
        let one = Instant::now();
        let outcome = ops
            .move_file(
                request(bulk(a, i), sorted(a, i), ConflictPolicy::Smart),
                &cx,
            )
            .await
            .unwrap();
        each.push(one.elapsed());
        assert!(matches!(outcome, MoveOutcome::Moved { .. }), "{outcome:?}");
    }
    eprintln!(
        "{}",
        summarize("moves one at a time", each, began.elapsed())
    );

    let (each, total) = timed_in_parallel(FILES, WORKERS, {
        let ops = Arc::clone(&ops);
        move |i| {
            let ops = Arc::clone(&ops);
            async move {
                let request = request(sorted(a, i), bulk(a, i), ConflictPolicy::Smart);
                let outcome = ops
                    .move_file(request, &OpContext::detached())
                    .await
                    .unwrap();
                assert!(matches!(outcome, MoveOutcome::Moved { .. }), "{outcome:?}");
            }
        }
    })
    .await;
    eprintln!(
        "{}",
        summarize(&format!("moves back on {WORKERS} tasks"), each, total)
    );

    let mut each = Vec::new();
    let began = Instant::now();
    for i in FILES - ABROAD..FILES {
        let one = Instant::now();
        let outcome = ops
            .move_file(
                request(bulk(a, i), abroad(b, i), ConflictPolicy::Smart),
                &cx,
            )
            .await
            .unwrap();
        each.push(one.elapsed());
        match outcome {
            MoveOutcome::Moved { cross_device, .. } => assert!(cross_device),
            other => panic!("{other:?}"),
        }
    }
    eprintln!(
        "{}",
        summarize(
            "moves to the other device (copy, verify, delete)",
            each,
            began.elapsed()
        )
    );

    let mut trashed = Vec::new();
    let mut each = Vec::new();
    let began = Instant::now();
    for i in 0..TRASHED {
        let one = Instant::now();
        let outcome = ops
            .trash_file(trash_request(bulk(a, i), &TrashBatch::adhoc()), &cx)
            .await
            .unwrap();
        each.push(one.elapsed());
        trashed.push(outcome.trash_item_id);
    }
    eprintln!("{}", summarize("trashed", each, began.elapsed()));
    let mut each = Vec::new();
    let began = Instant::now();
    for item in trashed {
        let one = Instant::now();
        let outcome = ops.restore(item, ConflictPolicy::Smart, &cx).await.unwrap();
        each.push(one.elapsed());
        assert!(matches!(outcome, RestoreOutcome::Restored { .. }));
    }
    eprintln!("{}", summarize("restored", each, began.elapsed()));

    let (mut want_a, mut want_b) = (BTreeMap::new(), BTreeMap::new());
    for i in 0..FILES {
        if i >= FILES - ABROAD {
            want_b.insert(abroad(b, i).rel.as_str().to_owned(), content(i));
        } else {
            want_a.insert(bulk(a, i).rel.as_str().to_owned(), content(i));
        }
    }
    let (disk_a, trash_a) = without_trash(disk_files(fx.ta.root()));
    let (disk_b, trash_b) = without_trash(disk_files(fx.tb.root()));
    assert_eq!(differences(&disk_a, &want_a), Vec::<String>::new());
    assert_eq!(differences(&disk_b, &want_b), Vec::<String>::new());
    assert!(trash_a.is_empty() && trash_b.is_empty());
    assert_eq!(fx.leftover_parts(), Vec::<String>::new());
    for i in 0..FILES {
        let home = if i >= FILES - ABROAD {
            abroad(b, i)
        } else {
            bulk(a, i)
        };
        let id = id_at(&fx, &home)
            .await
            .unwrap_or_else(|| panic!("file {i} has no row"));
        if i >= TRASHED {
            assert_eq!(id, first_ids[&i], "file {i} got another id by being moved");
        }
    }
    assert!(fx.trash_items().await.is_empty());

    let mut journal: BTreeMap<(String, String), i64> = BTreeMap::new();
    for row in fx.journal().await {
        *journal.entry(row).or_default() += 1;
    }
    let done = |op: &str, count: i64| ((op.to_owned(), "done".to_owned()), count);
    assert_eq!(
        journal,
        BTreeMap::from([
            done("move", 2 * FILES + ABROAD),
            done("trash", TRASHED),
            done("restore", TRASHED),
        ])
    );
    assert_eq!(fx.pending_rows().await, 0);
    let began = Instant::now();
    fx.assert_in_sync().await;
    eprintln!("a scan over the {FILES} files: {:.2?}", began.elapsed());
}

/// Not part of the normal run (it writes about 1.5 GiB): a large file crosses devices through the
/// copy, fsync, verify and delete path. Prints the time of each step and the rate; asserts that
/// the copy is byte for byte the original.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "throughput: needs 1.5 GiB of free disk; run with --release -- --ignored --nocapture"]
async fn a_large_file_crosses_devices_at_disk_speed() {
    const MIB: u64 = 512;
    const CHUNK: usize = 4 << 20;
    let stamps = Arc::new(Mutex::new(Vec::<(Step, Instant)>::new()));
    let fx = {
        let stamps = Arc::clone(&stamps);
        Fx::with(move |ops, trees| {
            let points = Failpoints::new(move |step| {
                stamps.lock().unwrap().push((step, Instant::now()));
                FailAction::Continue
            });
            ops.with_failpoints(points)
                .with_mover(Arc::new(SplitMover::new(&trees)))
        })
        .await
    };
    fx.ta.mkdir("big");
    let source = fx.ta.path("big/movie.mov");
    {
        let mut file = std::fs::File::create(&source).unwrap();
        let mut chunk: Vec<u8> = (0..CHUNK).map(|n| u8::try_from(n % 251).unwrap()).collect();
        for n in 0..MIB / 4 {
            chunk[..8].copy_from_slice(&n.to_le_bytes());
            file.write_all(&chunk).unwrap();
        }
    }
    fx.ta.set_mtime("big/movie.mov", BASE_MTIME);
    fx.scan().await;
    let bytes = MIB << 20;
    assert_eq!(std::fs::metadata(&source).unwrap().len(), bytes);

    let began = Instant::now();
    let digest = sha1_reader(std::fs::File::open(&source).unwrap(), |_| {
        ControlFlow::Continue(())
    })
    .unwrap()
    .unwrap();
    let hashing = began.elapsed();

    let (src, dst) = (loc(fx.a, "big/movie.mov"), loc(fx.b, "archive/movie.mov"));
    stamps.lock().unwrap().clear();
    let began = Instant::now();
    let outcome = fx
        .ops
        .move_file(
            request(src.clone(), dst.clone(), ConflictPolicy::Smart),
            &OpContext::detached(),
        )
        .await
        .unwrap();
    let took = began.elapsed();
    match outcome {
        MoveOutcome::Moved {
            cross_device,
            bytes: moved,
            ..
        } => {
            assert!(cross_device);
            assert_eq!(moved, bytes);
        }
        other => panic!("{other:?}"),
    }

    let mut last = began;
    let mut steps = Vec::new();
    for (step, at) in stamps.lock().unwrap().iter() {
        steps.push(format!("{step:?} +{:.0?}", at.duration_since(last)));
        last = *at;
    }
    steps.push(format!("end +{:.0?}", (began + took).duration_since(last)));
    let rate = |time: Duration| MIB as f64 / time.as_secs_f64();
    eprintln!(
        "{MIB} MiB across devices in {took:.2?} = {:.0} MiB/s; reading and hashing the original \
         alone takes {hashing:.2?} = {:.0} MiB/s\n  {}",
        rate(took),
        rate(hashing),
        steps.join(", ")
    );

    let copy = fx.tb.path("archive/movie.mov");
    assert_eq!(std::fs::metadata(&copy).unwrap().len(), bytes);
    let copied = sha1_reader(std::fs::File::open(&copy).unwrap(), |_| {
        ControlFlow::Continue(())
    })
    .unwrap()
    .unwrap();
    assert_eq!(copied.to_vec(), digest.to_vec(), "the copy differs");
    assert!(!source.exists(), "the original is still there");
    assert_eq!(fx.leftover_parts(), Vec::<String>::new());
    let (_, size, mtime) = fx.row(fx.b, "archive/movie.mov").await.unwrap();
    assert_eq!((size, mtime), (i64::try_from(bytes).unwrap(), BASE_MTIME));
    assert_eq!(fx.pending_rows().await, 0);
    fx.assert_in_sync().await;
}
