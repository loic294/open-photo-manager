//! What the tests of the outbox of file operations (`immich/outbox/`) need on top of the
//! [`Fixture`]: volumes whose files the fake Immich sees, moves, copies and trashes through the
//! real `FsOps` (with the outbox hook), passes of the outbox and the table the outbox keeps.
//!
//! A [`Site`] is one volume of the application, the library of the fake over it and the path the
//! volume is mounted at in the fake (`/mnt/<name>`): the fake imports the real files under it
//! when its library is scanned. The tests never wait for a loop: they start a pass
//! ([`Fixture::pass`]) and make the rows due again themselves ([`Fixture::make_due`]), which is
//! the only thing the real waits between two passes are replaced by.
//!
//! The table is read with plain SQL on purpose, like in the asset sync tests.

use std::time::Duration;

use apm_core::dto::common::ConflictPolicy;
use apm_core::dto::{TaskDto, TaskKind, TaskState};
use apm_core::paths::TaskId;
use apm_core::sidecar::LinkKind;
use apm_server::fsops::{
    MoveGroup, MoveOutcome, OpContext, RestoreOutcome, SidecarAction, SidecarItem, TrashBatch,
    TrashRequest,
};
use apm_server::immich::crypto::key_path;
use apm_server::indexer::{RunContext, ScanRequest};
use serde_json::Value;

use super::{Fixture, entry, eventually};
use crate::common::Tree;
use crate::common::fsops::{loc, request};

/// A row of `immich_outbox`.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct OutboxRow {
    pub id: i64,
    pub file_id: i64,
    pub old_volume_id: Option<i64>,
    pub old_dir: Option<String>,
    pub old_name: Option<String>,
    pub new_volume_id: Option<i64>,
    pub new_dir: Option<String>,
    pub new_name: Option<String>,
    pub asset_id: Option<String>,
    pub new_asset_id: Option<String>,
    pub snapshot_json: Option<String>,
    pub stage: i64,
    pub state: String,
    pub carried: bool,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub offline_seen_at: Option<i64>,
    pub scan_requested_at: Option<i64>,
    pub next_attempt_at: i64,
    pub created_at: i64,
}

impl OutboxRow {
    /// `dir/name` of the old place, relative to its volume.
    pub fn old_path(&self) -> Option<String> {
        join(self.old_dir.as_deref()?, self.old_name.as_deref()?)
    }

    /// `dir/name` of the new place, relative to its volume.
    pub fn new_path(&self) -> Option<String> {
        join(self.new_dir.as_deref()?, self.new_name.as_deref()?)
    }

    pub fn is_pending(&self) -> bool {
        self.state == "pending"
    }

    pub fn needs_attention(&self) -> bool {
        self.state == "attention"
    }
}

fn join(dir: &str, name: &str) -> Option<String> {
    Some(if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    })
}

/// The singleton row of `immich_outbox_state`.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct PassState {
    pub last_run_at: Option<i64>,
    pub last_error: Option<String>,
    pub consecutive_failures: i64,
}

const ROWS_SQL: &str = "SELECT id, file_id, old_volume_id, old_dir, old_name, new_volume_id, \
     new_dir, new_name, asset_id, new_asset_id, snapshot_json, stage, state, carried, attempts, \
     last_error, offline_seen_at, scan_requested_at, next_attempt_at, created_at \
     FROM immich_outbox ORDER BY id";

/// One volume of the application that the fake Immich sees.
pub struct Site {
    pub name: String,
    pub volume_id: i64,
    pub tree: Tree,
    pub library_id: String,
    /// The path the fake knows the root of the volume by: `/mnt/<name>`.
    pub prefix: String,
}

impl Site {
    /// The path Immich reports for the file `rel` of the volume.
    pub fn immich_path(&self, rel: &str) -> String {
        format!("{}/{rel}", self.prefix)
    }
}

impl Fixture {
    /// Registers the volume `name`, makes a library of the fake over `/mnt/<name>` and mounts
    /// the volume there. The mapping is not saved yet: see [`Fixture::map_sites`].
    pub async fn site(&self, name: &str) -> Site {
        let (volume_id, tree) = self.app.add_volume(name).await;
        let prefix = format!("/mnt/{name}");
        let library_id = self.fake.add_library(name, &[prefix.as_str()]);
        self.fake.mount(&prefix, tree.root());
        Site {
            name: name.to_owned(),
            volume_id,
            tree,
            library_id,
            prefix,
        }
    }

    /// Saves the mappings of `sites` (all at once, as the endpoint replaces them all).
    pub async fn map_sites(&self, sites: &[&Site]) {
        let entries = sites
            .iter()
            .map(|site| entry(site.volume_id, Some(&site.library_id), &site.prefix))
            .collect();
        self.map(entries).await;
    }

    /// Saves the mappings of `sites` as path prefixes only, which is all an account that is not
    /// an administrator can do: it cannot name a library.
    pub async fn map_prefixes(&self, sites: &[&Site]) {
        let entries = sites
            .iter()
            .map(|site| entry(site.volume_id, None, &site.prefix))
            .collect();
        self.map(entries).await;
    }

    /// Scans every volume into the index.
    pub async fn index(&self) {
        self.app
            .state
            .indexer
            .scan(&ScanRequest::default(), &RunContext::noop())
            .await
            .unwrap();
    }

    /// Writes the photos `rels` into `site` (each with its own content), indexes them and lets
    /// the fake scan the library, so that Immich holds an asset for each. The ids of the assets,
    /// in the order of `rels`.
    pub async fn seed(&self, site: &Site, rels: &[&str]) -> Vec<String> {
        for rel in rels {
            site.tree
                .write(rel, format!("photo {} {rel}", site.name).as_bytes());
        }
        self.index().await;
        self.fake.scan_library(&site.library_id);
        rels.iter()
            .map(|rel| self.fake.asset_at(&site.immich_path(rel)).id)
            .collect()
    }

    /// [`Fixture::seed`] of one photo.
    pub async fn seed_one(&self, site: &Site, rel: &str) -> String {
        self.seed(site, &[rel]).await.remove(0)
    }

    /// The id of the indexed file at `rel` of `site`.
    pub async fn file_id(&self, site: &Site, rel: &str) -> i64 {
        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        sqlx::query_scalar("SELECT id FROM files WHERE volume_id = ? AND dir_path = ? AND name = ?")
            .bind(site.volume_id)
            .bind(dir)
            .bind(name)
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap_or_else(|err| panic!("{rel} is not indexed in {}: {err}", site.name))
    }

    // ----- file operations, through the FsOps of the application (with the outbox hook) -----

    /// Moves `from` to `to` (`(site, path relative to its volume)`); the move must succeed.
    pub async fn move_file(&self, from: (&Site, &str), to: (&Site, &str)) -> MoveOutcome {
        let moved = self
            .app
            .state
            .fsops
            .move_file(
                request(
                    loc(from.0.volume_id, from.1),
                    loc(to.0.volume_id, to.1),
                    ConflictPolicy::Skip,
                ),
                &OpContext::detached(),
            )
            .await
            .unwrap();
        assert!(matches!(moved, MoveOutcome::Moved { .. }), "{moved:?}");
        moved
    }

    /// Moves `from` together with its stem sidecar `sidecar` (a file of the same folder) to `to`:
    /// the sidecar follows the final name of the photo, like in the move of the application.
    pub async fn move_with_sidecar(
        &self,
        from: (&Site, &str),
        sidecar: &str,
        to: (&Site, &str),
    ) -> MoveOutcome {
        let group = MoveGroup {
            owner: request(
                loc(from.0.volume_id, from.1),
                loc(to.0.volume_id, to.1),
                ConflictPolicy::Skip,
            ),
            sidecars: vec![SidecarItem {
                file_id: None,
                src: loc(from.0.volume_id, sidecar),
                kind: LinkKind::Stem,
                action: SidecarAction::Move,
            }],
        };
        let outcome = self
            .app
            .state
            .fsops
            .move_group(group, &OpContext::detached())
            .await
            .unwrap();
        assert!(
            matches!(outcome.owner, MoveOutcome::Moved { .. }),
            "{outcome:?}"
        );
        outcome.owner
    }

    /// Copies `from` to `to`; the copy must succeed.
    pub async fn copy_file(&self, from: (&Site, &str), to: (&Site, &str)) -> MoveOutcome {
        let copied = self
            .app
            .state
            .fsops
            .copy_file(
                request(
                    loc(from.0.volume_id, from.1),
                    loc(to.0.volume_id, to.1),
                    ConflictPolicy::Skip,
                ),
                &OpContext::detached(),
            )
            .await
            .unwrap();
        assert!(matches!(copied, MoveOutcome::Moved { .. }), "{copied:?}");
        copied
    }

    /// Trashes the file; the id of the trash item.
    pub async fn trash_file(&self, site: &Site, rel: &str) -> i64 {
        self.app
            .state
            .fsops
            .trash_file(
                TrashRequest {
                    file_id: None,
                    loc: loc(site.volume_id, rel),
                    expect: None,
                    batch: TrashBatch::adhoc(),
                    reason: "user".to_owned(),
                },
                &OpContext::detached(),
            )
            .await
            .unwrap()
            .trash_item_id
    }

    /// Restores a trash item to where it came from; the place it went to.
    pub async fn restore(&self, trash_item_id: i64) -> RestoreOutcome {
        let restored = self
            .app
            .state
            .fsops
            .restore(trash_item_id, ConflictPolicy::Smart, &OpContext::detached())
            .await
            .unwrap();
        assert!(
            matches!(restored, RestoreOutcome::Restored { .. }),
            "{restored:?}"
        );
        restored
    }

    // ----- the outbox ----------------------------------------------------------------------

    /// Every row of the outbox, oldest first.
    pub async fn rows(&self) -> Vec<OutboxRow> {
        sqlx::query_as(ROWS_SQL)
            .fetch_all(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// The one row of the outbox, which must be the only one.
    pub async fn only_row(&self) -> OutboxRow {
        let mut rows = self.rows().await;
        assert_eq!(rows.len(), 1, "the outbox holds {rows:?}");
        rows.remove(0)
    }

    pub async fn row_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM immich_outbox")
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// How the worker did, the singleton row.
    pub async fn pass_state(&self) -> PassState {
        sqlx::query_as(
            "SELECT last_run_at, last_error, consecutive_failures FROM immich_outbox_state",
        )
        .fetch_one(self.app.state.db.read())
        .await
        .unwrap()
    }

    /// Makes every waiting row due now: what the wait between two passes comes to.
    pub async fn make_due(&self) {
        sqlx::query("UPDATE immich_outbox SET next_attempt_at = 0 WHERE state = 'pending'")
            .execute(self.app.state.db.write())
            .await
            .unwrap();
    }

    /// A person pressing Retry: the rows that need attention or wait are due again with a fresh
    /// count of attempts, and the next pass may ask Immich to scan a library again at once. How
    /// many rows it changed.
    pub async fn retry_all(&self) -> i64 {
        self.app
            .state
            .immich_outbox
            .retry(None)
            .await
            .unwrap()
            .retried
    }

    /// Makes the moment a pass first saw an old asset offline `secs` seconds older.
    pub async fn age_offline_seen(&self, secs: i64) {
        sqlx::query("UPDATE immich_outbox SET offline_seen_at = offline_seen_at - ?")
            .bind(secs)
            .execute(self.app.state.db.write())
            .await
            .unwrap();
    }

    /// Makes the rows `secs` seconds older: the change they record, their scan request and the
    /// moment an old asset was first seen offline.
    pub async fn age_rows(&self, secs: i64) {
        sqlx::query(
            "UPDATE immich_outbox SET created_at = created_at - ?1, \
             scan_requested_at = scan_requested_at - ?1, offline_seen_at = offline_seen_at - ?1",
        )
        .bind(secs)
        .execute(self.app.state.db.write())
        .await
        .unwrap();
    }

    /// Starts a pass now and waits for it, whatever way it ends. A row must be due.
    pub async fn pass(&self) -> TaskDto {
        let task = self
            .app
            .state
            .immich_outbox
            .submit_now()
            .await
            .unwrap()
            .expect("a pass is due");
        self.finish(task).await
    }

    /// A pass that must end well.
    pub async fn pass_ok(&self) -> TaskDto {
        let task = self.pass().await;
        assert_eq!(task.state, TaskState::Done, "{task:?}");
        assert_eq!(task.error, None);
        task
    }

    /// Starts a pass whose read of Immich hangs (the fake takes 30 s to answer a search) and
    /// returns its task once the request is in, so the pass is running and can be cancelled.
    /// The calls the fake recorded so far are forgotten.
    pub async fn held_pass(&self) -> TaskId {
        self.fake.clear_calls();
        self.fake.hold_searches(Duration::from_secs(30));
        let task = self
            .app
            .state
            .immich_outbox
            .submit_now()
            .await
            .unwrap()
            .expect("a pass is due");
        self.fake.wait_for_searches(1).await;
        task
    }

    /// Cancels the task, as a person does, and waits for it to end.
    pub async fn cancel(&self, task: TaskId) -> TaskDto {
        self.app.state.tasks.cancel(task).await.unwrap();
        self.finish(task).await
    }

    /// Makes the rows due and runs a pass, as many times as rows are left, up to `limit` passes.
    /// Returns the number of passes. A move inside a library needs two: one asks for the scan,
    /// the next sees what came of it.
    pub async fn drain(&self, limit: usize) -> usize {
        let mut ran = 0;
        while ran < limit {
            self.make_due().await;
            let Some(task) = self.app.state.immich_outbox.submit_now().await.unwrap() else {
                break;
            };
            let task = self.finish(task).await;
            assert_eq!(task.state, TaskState::Done, "{task:?}");
            ran += 1;
        }
        ran
    }

    /// What the task returned, as the report of a pass (`rows`, `scans`, `carried`, `deleted`...).
    pub async fn report(&self, task: &TaskDto) -> Value {
        self.task_result(task.id)
            .await
            .expect("the pass has a report")
    }

    /// Waits until no task is queued or running: the sync a drained queue starts included.
    pub async fn quiesce(&self) {
        eventually("the tasks to end", || async {
            self.app
                .state
                .tasks
                .list(0)
                .await
                .unwrap()
                .iter()
                .all(|task| !matches!(task.state, TaskState::Queued | TaskState::Running))
        })
        .await;
    }

    /// The key file is lost in a restarted application: the service keeps no client in memory, so
    /// the next call has to read the file. (The service builds a new client when the version of
    /// the server changed, which is how this forgets the one it holds.)
    pub async fn lose_the_key(&self) {
        std::fs::remove_file(key_path(&self.data_dir())).unwrap();
        sqlx::query("UPDATE immich_connection SET server_version = 'v0.0.0' WHERE id = 1")
            .execute(self.app.state.db.write())
            .await
            .unwrap();
    }

    /// How many tasks of `kind` the application has run or runs (the newest 500 finished ones).
    pub async fn tasks_of(&self, kind: TaskKind) -> usize {
        self.app
            .state
            .tasks
            .list(500)
            .await
            .unwrap()
            .iter()
            .filter(|task| task.kind == kind)
            .count()
    }

    /// The bodies of the `DELETE /assets` requests the fake received, in order.
    pub fn deletes(&self) -> Vec<Value> {
        self.fake
            .calls_to("DELETE", "/api/assets")
            .into_iter()
            .map(|call| call.body.unwrap_or(Value::Null))
            .collect()
    }

    /// The ids of the libraries the application asked the fake to scan, in order.
    pub fn scans_asked(&self) -> Vec<String> {
        self.fake
            .calls()
            .into_iter()
            .filter(|call| call.method == "POST" && call.status == 204)
            .filter_map(|call| {
                call.path
                    .strip_prefix("/api/libraries/")?
                    .strip_suffix("/scan")
                    .map(str::to_owned)
            })
            .collect()
    }

    /// How many requests changed what Immich knows about an asset: the favorite, rating and
    /// description, the albums and the tags (what a carry-over does).
    pub fn carries(&self) -> usize {
        self.fake
            .calls()
            .into_iter()
            .filter(|call| call.method == "PUT" && call.status < 300)
            .filter(|call| {
                call.path == "/api/assets"
                    || call.path == "/api/tags/assets"
                    || (call.path.starts_with("/api/albums/") && call.path.ends_with("/assets"))
            })
            .count()
    }

    /// The `outbox` of `GET /api/immich/status`.
    pub async fn outbox_status(&self) -> Value {
        self.status(false).await["outbox"].clone()
    }
}

/// How long a test lets a loop run before it looks at what it did.
pub const SHORT: Duration = Duration::from_millis(50);

/// The id of a task of a started pass.
pub fn id_of(task: &TaskDto) -> TaskId {
    task.id
}

/// A connected fixture (an administrator) with one library holding one photo, `2020/a.jpg`, that
/// Immich knows: the start of most scenarios. The site, and the id of the asset.
pub async fn one_photo() -> (Fixture, Site, String) {
    let fx = Fixture::connected().await;
    let photos = fx.site("photos").await;
    fx.map_sites(&[&photos]).await;
    let id = fx.seed_one(&photos, "2020/a.jpg").await;
    fx.sync_ok(false).await;
    (fx, photos, id)
}

/// [`one_photo`] after the photo moved to `2021/a.jpg`: one change waits for its first pass.
pub async fn moved_photo() -> (Fixture, Site, String) {
    let (fx, photos, id) = one_photo().await;
    fx.move_file((&photos, "2020/a.jpg"), (&photos, "2021/a.jpg"))
        .await;
    (fx, photos, id)
}

/// A connected fixture (an administrator) with `count` photos, `2020/p000.jpg` and on, that Immich
/// knows. The site, and the ids of the assets in the order of the photos.
pub async fn many_photos(count: usize) -> (Fixture, Site, Vec<String>) {
    let fx = Fixture::connected().await;
    let (photos, ids) = photos_in(&fx, count).await;
    (fx, photos, ids)
}

/// [`many_photos`] in a fixture that is connected already (one whose configuration the test
/// chose). The site, and the ids of the assets in the order of the photos.
pub async fn photos_in(fx: &Fixture, count: usize) -> (Site, Vec<String>) {
    let photos = fx.site("photos").await;
    fx.map_sites(&[&photos]).await;
    let names: Vec<String> = (0..count).map(|n| format!("2020/p{n:03}.jpg")).collect();
    let rels: Vec<&str> = names.iter().map(String::as_str).collect();
    let ids = fx.seed(&photos, &rels).await;
    fx.sync_ok(false).await;
    (photos, ids)
}

/// [`one_photo`] for an account that is not an administrator: no library can be named or scanned.
pub async fn one_photo_limited() -> (Fixture, Site, String) {
    let fx = Fixture::connected_limited().await;
    let photos = fx.site("photos").await;
    fx.map_prefixes(&[&photos]).await;
    let id = fx.seed_one(&photos, "2020/a.jpg").await;
    fx.sync_ok(false).await;
    (fx, photos, id)
}

/// The first pass after a change: the rows are due at once. It must end well; the report of what
/// it did.
pub async fn first_pass(fx: &Fixture) -> Value {
    let task = fx.pass_ok().await;
    fx.report(&task).await
}

/// The next pass: the rows are due again (what the wait between two passes comes to). It must end
/// well; the report of what it did.
pub async fn next_pass(fx: &Fixture) -> Value {
    fx.make_due().await;
    first_pass(fx).await
}

/// The body of the `DELETE /assets` request that removes the record of `id`.
pub fn delete_of(id: &str) -> Value {
    serde_json::json!({ "ids": [id], "force": true })
}
