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

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use apm_core::dto::common::ConflictPolicy;
use apm_core::sidecar::LinkKind;
use apm_server::fsops::{
    AppliedOp, FailAction, Failpoints, Loc, MoveGroup, MoveOutcome, OpContext, OpHook,
    RestoreOutcome, SidecarAction, SidecarItem, SidecarOutcome, Step, TrashBatch, TrashOutcome,
    TrashRequest,
};
use apm_server::indexer::{Indexer, RunContext, ScanRequest, ScanSummary, ScanTarget};
use common::fsops::{Fx, Memory, SplitMover, loc, rel, request};
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
