//! Public-safe tests of the executor/adapter seam. These do NOT validate PostgreSQL
//! schema or triggers; the separate real-v3.3.3 gate owns those assertions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use apm_core::dto::{ConflictPolicy, TaskState};
use apm_server::fsops::{FsOps, MoveGroup, MoveOutcome, OpContext, SidecarAction, SidecarItem};
use apm_server::immich::direct_db::{
    DbMoveError, MoveApplied, MoveDatabase, MoveSpec, PreparedMove,
};
use apm_server::immich::outbox::OutboxSignal;
use apm_server::immich::{ImmichOutbox, ImmichService, ImmichSync};
use async_trait::async_trait;
use common::fsops::{loc, request};
use immich_support::{Fixture, Site, one_photo};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Database {
    error: Mutex<Option<DbMoveError>>,
    prepared: Mutex<Vec<PreparedMove>>,
    applied: Mutex<Vec<PreparedMove>>,
    sidecar: bool,
    outage_after_prepare: bool,
}

#[async_trait]
impl MoveDatabase for Database {
    async fn probe(&self, version: &str) -> Result<(), DbMoveError> {
        assert_eq!(version, "v3.3.3");
        self.error.lock().unwrap().map_or(Ok(()), Err)
    }
    async fn prepare(&self, spec: MoveSpec) -> Result<PreparedMove, DbMoveError> {
        let prepared = PreparedMove {
            sidecar: if self.sidecar {
                Some(spec.sidecars.first().ok_or(DbMoveError::Sidecar)?.clone())
            } else {
                None
            },
            sidecar_update_id: self.sidecar.then(|| "sidecar-cursor".to_owned()),
            spec,
            update_id: "asset-cursor".to_owned(),
            checksum: vec![1; 20],
            checksum_algorithm: "sha1".to_owned(),
        };
        self.prepared.lock().unwrap().push(prepared.clone());
        if self.outage_after_prepare {
            *self.error.lock().unwrap() = Some(DbMoveError::Unavailable);
        }
        Ok(prepared)
    }
    async fn apply(&self, prepared: &PreparedMove) -> Result<MoveApplied, DbMoveError> {
        if let Some(error) = *self.error.lock().unwrap() {
            return Err(error);
        }
        self.applied.lock().unwrap().push(prepared.clone());
        Ok(MoveApplied::Updated)
    }
}

fn engines(fx: &Fixture, database: Arc<Database>) -> (FsOps, ImmichOutbox) {
    fx.fake.state().version = "v3.3.3".to_owned();
    let state = &fx.app.state;
    let immich = ImmichService::with_move_database(state.db.clone(), fx.data_dir(), database);
    let sync = ImmichSync::new(state.db.clone(), immich.clone(), state.tasks.clone());
    let outbox = ImmichOutbox::new(
        state.db.clone(),
        immich,
        sync,
        state.tasks.clone(),
        state.volumes.clone(),
        OutboxSignal::new(),
        1,
    );
    let ops = state
        .fsops
        .clone()
        .with_move_guard(Arc::new(outbox.clone()));
    (ops, outbox)
}

async fn move_photo(
    ops: &FsOps,
    site: &Site,
    destination: &str,
    policy: ConflictPolicy,
) -> Result<MoveOutcome, apm_server::fsops::FsOpError> {
    ops.move_file(
        request(
            loc(site.volume_id, "2020/a.jpg"),
            loc(site.volume_id, destination),
            policy,
        ),
        &OpContext::detached(),
    )
    .await
}

async fn pass(fx: &Fixture, outbox: &ImmichOutbox) -> apm_core::dto::TaskDto {
    let id = outbox
        .submit_now()
        .await
        .unwrap()
        .expect("a direct pass is due");
    fx.finish(id).await
}

#[tokio::test]
async fn unavailable_or_missing_database_refuses_tracked_items_without_replacing_any_file() {
    for error in [
        DbMoveError::Unavailable,
        DbMoveError::NotConfigured,
        DbMoveError::Configuration,
        DbMoveError::Incompatible,
    ] {
        let (fx, site, _) = one_photo().await;
        let database = Arc::new(Database::default());
        *database.error.lock().unwrap() = Some(error);
        let (ops, _) = engines(&fx, database);
        site.tree.write("2021/a.jpg", b"do not replace");
        let error = move_photo(&ops, &site, "2021/a.jpg", ConflictPolicy::Replace)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "immich_move_refused");
        assert!(error.to_string().starts_with("No file was moved."));
        assert!(site.tree.root().join("2020/a.jpg").exists());
        assert_eq!(
            std::fs::read(site.tree.root().join("2021/a.jpg")).unwrap(),
            b"do not replace"
        );
        assert_eq!(fx.row_count().await, 0);
        fx.fake.assert_no_violations();
    }
}

#[tokio::test]
async fn untracked_items_in_a_mixed_batch_continue_during_database_outage() {
    let (fx, site, _) = one_photo().await;
    site.tree.write("loose.jpg", b"untracked");
    fx.index().await;
    let database = Arc::new(Database::default());
    *database.error.lock().unwrap() = Some(DbMoveError::Unavailable);
    let (ops, _) = engines(&fx, database);
    assert!(
        move_photo(&ops, &site, "2021/a.jpg", ConflictPolicy::Skip)
            .await
            .is_err()
    );
    assert!(matches!(
        ops.move_file(
            request(
                loc(site.volume_id, "loose.jpg"),
                loc(site.volume_id, "new/loose.jpg"),
                ConflictPolicy::Skip
            ),
            &OpContext::detached()
        )
        .await
        .unwrap(),
        MoveOutcome::Moved { .. }
    ));
    assert!(site.tree.root().join("2020/a.jpg").exists());
}

#[tokio::test]
async fn direct_move_queues_exact_identity_and_final_conflict_name_without_api_deletion_or_scan() {
    let (fx, site, id) = one_photo().await;
    let database = Arc::new(Database::default());
    let (ops, outbox) = engines(&fx, database.clone());
    site.tree.write("2021/a.jpg", b"different");
    let outcome = move_photo(&ops, &site, "2021/a.jpg", ConflictPolicy::Rename)
        .await
        .unwrap();
    let MoveOutcome::Moved { dst, .. } = outcome else {
        panic!("not moved");
    };
    assert_eq!(dst.rel.as_str(), "2021/a_1.jpg");
    assert_eq!(fx.row_count().await, 1);
    assert_eq!(
        fx.only_row().await.last_error.as_deref(),
        Some("database_update_pending")
    );
    assert_eq!(pass(&fx, &outbox).await.state, TaskState::Done);
    let applied = database.applied.lock().unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].spec.asset_id, id);
    assert_eq!(applied[0].spec.old, site.immich_path("2020/a.jpg"));
    assert_eq!(applied[0].spec.new, site.immich_path("2021/a_1.jpg"));
    assert_eq!(applied[0].spec.new_name, "a_1.jpg");
    assert!(fx.deletes().is_empty());
    assert!(fx.scans_asked().is_empty());
    assert_eq!(fx.row_count().await, 0);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn outage_after_move_is_durable_warned_and_retryable_after_engine_restart() {
    let (fx, site, _) = one_photo().await;
    let database = Arc::new(Database::default());
    let (ops, outbox) = engines(&fx, database.clone());
    move_photo(&ops, &site, "2021/a.jpg", ConflictPolicy::Skip)
        .await
        .unwrap();
    *database.error.lock().unwrap() = Some(DbMoveError::Unavailable);
    let task = pass(&fx, &outbox).await;
    assert_eq!(task.state, TaskState::Failed);
    let row = fx.only_row().await;
    assert!(row.is_pending());
    assert!(
        row.last_error
            .unwrap()
            .contains("path update is NOT confirmed")
    );
    assert!(site.tree.root().join("2021/a.jpg").exists());
    assert!(fx.deletes().is_empty());
    *database.error.lock().unwrap() = None;
    let (_, restarted) = engines(&fx, database.clone());
    restarted.retry(None).await.unwrap();
    assert_eq!(pass(&fx, &restarted).await.state, TaskState::Done);
    assert_eq!(database.applied.lock().unwrap().len(), 1);
    assert_eq!(fx.row_count().await, 0);
}

#[tokio::test]
async fn cross_library_in_place_moves_are_refused_but_unmapped_removal_uses_api() {
    let (fx, site, _) = one_photo().await;
    let other = fx.site("other").await;
    fx.map_sites(&[&site, &other]).await;
    let (ops, _) = engines(&fx, Arc::new(Database::default()));
    let error = ops
        .move_file(
            request(
                loc(site.volume_id, "2020/a.jpg"),
                loc(other.volume_id, "a.jpg"),
                ConflictPolicy::Skip,
            ),
            &OpContext::detached(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cross-library"));
    assert!(site.tree.root().join("2020/a.jpg").exists());
    fx.map_sites(&[&site]).await;
    ops.move_file(
        request(
            loc(site.volume_id, "2020/a.jpg"),
            loc(other.volume_id, "a.jpg"),
            ConflictPolicy::Skip,
        ),
        &OpContext::detached(),
    )
    .await
    .unwrap();
    fx.drain(5).await;
    assert_eq!(fx.deletes().len(), 1);
    fx.fake.assert_no_violations();
}

#[tokio::test]
async fn linked_sidecar_copy_is_confirmed_and_original_can_remain_for_shared_owners() {
    let (fx, site, _) = one_photo().await;
    site.tree.write("2020/a.xmp", b"metadata");
    fx.index().await;
    let database = Arc::new(Database {
        sidecar: true,
        ..Database::default()
    });
    let (ops, outbox) = engines(&fx, database.clone());
    ops.move_group(
        MoveGroup {
            owner: request(
                loc(site.volume_id, "2020/a.jpg"),
                loc(site.volume_id, "2021/b.jpg"),
                ConflictPolicy::Skip,
            ),
            sidecars: vec![SidecarItem {
                file_id: None,
                src: loc(site.volume_id, "2020/a.xmp"),
                kind: apm_core::sidecar::LinkKind::Stem,
                action: SidecarAction::Copy,
            }],
        },
        &OpContext::detached(),
    )
    .await
    .unwrap();
    assert!(site.tree.root().join("2020/a.xmp").exists());
    assert!(site.tree.root().join("2021/b.xmp").exists());
    assert_eq!(pass(&fx, &outbox).await.state, TaskState::Done);
    assert_eq!(
        database.applied.lock().unwrap()[0]
            .sidecar
            .as_ref()
            .unwrap()
            .new,
        site.immich_path("2021/b.xmp")
    );
}

#[tokio::test]
async fn case_only_file_rename_updates_the_exact_path_without_api_deletion() {
    let (fx, site, _) = one_photo().await;
    let database = Arc::new(Database::default());
    let (ops, outbox) = engines(&fx, database.clone());
    move_photo(&ops, &site, "2020/A.jpg", ConflictPolicy::Skip)
        .await
        .unwrap();
    assert_eq!(pass(&fx, &outbox).await.state, TaskState::Done);
    assert_eq!(
        database.applied.lock().unwrap()[0].spec.new,
        site.immich_path("2020/A.jpg")
    );
    assert!(fx.deletes().is_empty());
}

#[tokio::test]
async fn second_move_waits_for_the_previous_path_update_instead_of_becoming_untracked() {
    let (fx, site, _) = one_photo().await;
    let (ops, _) = engines(&fx, Arc::new(Database::default()));
    move_photo(&ops, &site, "2021/a.jpg", ConflictPolicy::Skip)
        .await
        .unwrap();
    let error = ops
        .move_file(
            request(
                loc(site.volume_id, "2021/a.jpg"),
                loc(site.volume_id, "2022/a.jpg"),
                ConflictPolicy::Skip,
            ),
            &OpContext::detached(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "immich_move_refused");
    assert!(error.to_string().contains("previous Immich path update"));
    assert!(site.tree.root().join("2021/a.jpg").exists());
    assert_eq!(fx.row_count().await, 1);
}

#[tokio::test]
async fn replacement_does_not_recheck_and_falsely_refuse_after_trashing_the_victim() {
    let (fx, site, _) = one_photo().await;
    site.tree.write("2021/a.jpg", b"replacement victim");
    let database = Arc::new(Database {
        outage_after_prepare: true,
        ..Database::default()
    });
    let (ops, outbox) = engines(&fx, database.clone());
    assert!(matches!(
        move_photo(&ops, &site, "2021/a.jpg", ConflictPolicy::Replace)
            .await
            .unwrap(),
        MoveOutcome::Moved { .. }
    ));
    assert!(!site.tree.root().join("2020/a.jpg").exists());
    assert_eq!(database.prepared.lock().unwrap().len(), 1);
    let queued: Vec<i64> = sqlx::query_scalar("SELECT queued FROM immich_direct_move")
        .fetch_all(fx.app.state.db.read())
        .await
        .unwrap();
    assert_eq!(queued, [1]);
    let task = pass(&fx, &outbox).await;
    assert_eq!(task.state, TaskState::Failed, "{task:?}");
    let warning: String = sqlx::query_scalar(
        "SELECT last_error FROM immich_outbox WHERE id IN (SELECT outbox_id FROM immich_direct_move WHERE queued = 1)")
        .fetch_one(fx.app.state.db.read()).await.unwrap();
    assert!(warning.starts_with("The file moved, but"));
}

#[tokio::test]
async fn a_late_cross_device_conflict_checks_the_actual_final_name_before_placement() {
    use apm_server::fsops::{FailAction, Failpoints, Mover, Step, SystemMover};
    use std::{io, path::Path};
    struct ForcedCopy;
    impl Mover for ForcedCopy {
        fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
            if from
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".apm-part"))
            {
                SystemMover.rename_no_replace(from, to)
            } else {
                Err(io::ErrorKind::CrossesDevices.into())
            }
        }
        fn crosses_devices(&self, _: &Path, _: u64, _: &Path, _: u64) -> bool {
            true
        }
    }
    for outage in [false, true] {
        let (fx, site, _) = one_photo().await;
        let database = Arc::new(Database {
            outage_after_prepare: outage,
            ..Database::default()
        });
        let (ops, outbox) = engines(&fx, database.clone());
        let taken = site.tree.root().join("2021/a.jpg");
        let failing = ops
            .with_mover(Arc::new(ForcedCopy))
            .with_failpoints(Failpoints::new(move |step| {
                if step == Step::AfterCopy {
                    std::fs::write(&taken, b"late conflict").unwrap();
                }
                FailAction::Continue
            }));
        let result = move_photo(&failing, &site, "2021/a.jpg", ConflictPolicy::Rename).await;
        if outage {
            assert_eq!(result.unwrap_err().code(), "immich_move_refused");
            assert!(site.tree.root().join("2020/a.jpg").exists());
            assert!(!site.tree.root().join("2021/a_1.jpg").exists());
            let intents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM immich_direct_move")
                .fetch_one(fx.app.state.db.read())
                .await
                .unwrap();
            assert_eq!(intents, 0);
        } else {
            let MoveOutcome::Moved { dst, .. } = result.unwrap() else {
                panic!("not moved");
            };
            assert_eq!(dst.rel.as_str(), "2021/a_1.jpg");
            assert_eq!(database.prepared.lock().unwrap().len(), 2);
            assert_eq!(pass(&fx, &outbox).await.state, TaskState::Done);
            assert_eq!(database.applied.lock().unwrap()[0].spec.new_name, "a_1.jpg");
        }
        assert_eq!(
            std::fs::read(site.tree.root().join("2021/a.jpg")).unwrap(),
            b"late conflict"
        );
    }
}

#[tokio::test]
async fn ordinary_post_disk_errors_and_crashes_keep_recoverable_intents() {
    use apm_server::fsops::{FailAction, Failpoints, Step};
    for action in [FailAction::Abort, FailAction::Error] {
        let (fx, site, _) = one_photo().await;
        let database = Arc::new(Database::default());
        let (ops, outbox) = engines(&fx, database.clone());
        let failing = ops.clone().with_failpoints(Failpoints::new(move |step| {
            if step == Step::BeforeIndexTx {
                action
            } else {
                FailAction::Continue
            }
        }));
        assert!(
            move_photo(&failing, &site, "2021/a.jpg", ConflictPolicy::Skip)
                .await
                .is_err()
        );
        let intents: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM immich_direct_move WHERE queued = 0")
                .fetch_one(fx.app.state.db.read())
                .await
                .unwrap();
        assert_eq!(
            intents, 1,
            "pending journal recovery needs the prepared intent"
        );
        ops.reconcile().await.unwrap();
        assert_eq!(fx.row_count().await, 1);
        ops.reconcile().await.unwrap();
        assert_eq!(fx.row_count().await, 1, "replay cannot enqueue twice");
        assert_eq!(pass(&fx, &outbox).await.state, TaskState::Done);
        assert_eq!(database.applied.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn restart_cleans_orphan_preparation_but_retains_completed_journal_replay_proof() {
    use apm_server::fsops::{AppliedOp, OpHook};
    use apm_server::immich::outbox::OutboxHook;
    let (fx, site, _) = one_photo().await;
    let database = Arc::new(Database::default());
    let (_, outbox) = engines(&fx, database.clone());
    let source = loc(site.volume_id, "2020/a.jpg");
    let destination = loc(site.volume_id, "2021/a.jpg");
    outbox
        .preflight_move(&source, &destination, &[])
        .await
        .unwrap();
    *database.error.lock().unwrap() = Some(DbMoveError::Unavailable);
    let (ops, restarted) = engines(&fx, database.clone());
    assert!(
        restarted
            .preflight_move(&source, &destination, &[])
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM immich_direct_move")
        .fetch_one(fx.app.state.db.read())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "no journal means the old inert intent is orphaned"
    );
    *database.error.lock().unwrap() = None;
    let outcome = move_photo(&ops, &site, "2021/a.jpg", ConflictPolicy::Skip)
        .await
        .unwrap();
    let MoveOutcome::Moved { file_id, .. } = outcome else {
        panic!("not moved");
    };
    assert_eq!(pass(&fx, &restarted).await.state, TaskState::Done);
    let (_, later) = engines(&fx, database);
    assert!(later.submit_now().await.unwrap().is_none());
    let proof: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM immich_direct_move WHERE queued = 2")
        .fetch_one(fx.app.state.db.read())
        .await
        .unwrap();
    assert_eq!(proof, 1, "the completed journal still exists");
    let hook = OutboxHook::new(OutboxSignal::new());
    let operation = AppliedOp::Moved {
        file_id,
        from: source,
        to: destination,
    };
    let mut tx = fx.app.state.db.begin_write().await.unwrap();
    hook.on_applied(&mut tx, &operation).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        fx.row_count().await,
        0,
        "completed replay cannot enqueue generic scan/delete"
    );
}

#[tokio::test]
async fn failed_or_cancelled_groups_remove_inert_intents_when_disk_work_did_not_happen() {
    use apm_server::fsops::{FailAction, Failpoints, Step};
    for cancel in [false, true] {
        let (fx, site, _) = one_photo().await;
        site.tree.write("2020/a.xmp", b"metadata");
        fx.index().await;
        let database = Arc::new(Database {
            sidecar: true,
            ..Database::default()
        });
        let (ops, _) = engines(&fx, database);
        let cx = OpContext::detached();
        let token = cx.cancel.clone();
        let failing = ops.with_failpoints(Failpoints::new(move |step| {
            if step == Step::AfterJournal {
                if cancel {
                    token.cancel();
                }
                FailAction::Error
            } else {
                FailAction::Continue
            }
        }));
        assert!(
            failing
                .move_group(
                    MoveGroup {
                        owner: request(
                            loc(site.volume_id, "2020/a.jpg"),
                            loc(site.volume_id, "2021/b.jpg"),
                            ConflictPolicy::Skip
                        ),
                        sidecars: vec![SidecarItem {
                            file_id: None,
                            src: loc(site.volume_id, "2020/a.xmp"),
                            kind: apm_core::sidecar::LinkKind::Stem,
                            action: SidecarAction::Copy
                        }],
                    },
                    &cx
                )
                .await
                .is_err()
        );
        let intents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM immich_direct_move")
            .fetch_one(fx.app.state.db.read())
            .await
            .unwrap();
        assert_eq!(intents, 0);
        assert!(site.tree.root().join("2020/a.jpg").exists());
        assert!(site.tree.root().join("2020/a.xmp").exists());
        assert!(!site.tree.root().join("2021/b.xmp").exists());
        assert_eq!(fx.row_count().await, 0);
    }
}
