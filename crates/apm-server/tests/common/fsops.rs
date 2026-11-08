//! Helpers for the file-operation tests: two scratch volumes with an `FsOps` over them, the
//! movers and free-space reports a temporary directory cannot produce, and snapshots of the disk,
//! the index and the journal to compare before and after.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use apm_core::dto::common::ConflictPolicy;
use apm_core::hash::sha1_bytes;
use apm_core::paths::{RelPath, VolumeId};
use apm_server::fsops::{
    FailAction, Failpoints, FreeSpace, FsOpError, FsOps, Loc, MoveRequest, Mover, ReconcileReport,
    Step, SystemMover,
};
use apm_server::indexer::{RunContext, ScanRequest, ScanSummary};

use super::{TestApp, Tree};

pub fn rel(path: &str) -> RelPath {
    RelPath::new(path).unwrap()
}

pub fn loc(volume: VolumeId, path: &str) -> Loc {
    Loc::new(volume, rel(path))
}

pub fn request(src: Loc, dst: Loc, policy: ConflictPolicy) -> MoveRequest {
    MoveRequest {
        file_id: None,
        src,
        dst,
        policy,
        expect: None,
    }
}

/// An `FsOps` over the application's own database, volumes and settings. It has no hooks and no
/// failpoints, and nothing has been recovered by it.
pub fn ops_of(app: &TestApp) -> FsOps {
    FsOps::new(
        app.state.db.clone(),
        app.state.volumes.clone(),
        app.state.settings.clone(),
        app.state.indexer.clone(),
    )
}

/// Pretends that a rename between two volumes crosses devices (they are directories of one file
/// system here), so the copy-verify-delete path runs.
pub struct SplitMover {
    sides: Vec<(usize, PathBuf)>,
}

impl SplitMover {
    pub fn new(trees: &[&Tree]) -> Self {
        let mut sides = Vec::new();
        for (side, tree) in trees.iter().enumerate() {
            sides.push((side, tree.root().to_path_buf()));
            if let Ok(canonical) = tree.root().canonicalize() {
                sides.push((side, canonical));
            }
        }
        Self { sides }
    }

    fn side(&self, path: &Path) -> Option<usize> {
        self.sides
            .iter()
            .find(|(_, prefix)| path.starts_with(prefix))
            .map(|(side, _)| *side)
    }
}

impl Mover for SplitMover {
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.side(from) != self.side(to) {
            return Err(io::ErrorKind::CrossesDevices.into());
        }
        SystemMover.rename_no_replace(from, to)
    }

    fn crosses_devices(
        &self,
        from_root: &Path,
        from_dev: u64,
        to_root: &Path,
        to_dev: u64,
    ) -> bool {
        from_dev != to_dev || self.side(from_root) != self.side(to_root)
    }
}

/// A disk with exactly this much room.
pub struct FixedFreeSpace(pub u64);

impl FreeSpace for FixedFreeSpace {
    fn available(&self, _dir: &Path) -> io::Result<u64> {
        Ok(self.0)
    }
}

/// Two volumes, `a` and `b`, and the file operations over them.
pub struct Fx {
    pub app: TestApp,
    pub a: VolumeId,
    pub b: VolumeId,
    pub ta: Tree,
    pub tb: Tree,
    pub ops: FsOps,
}

impl Fx {
    pub async fn new() -> Self {
        Self::with(|ops, _| ops).await
    }

    /// Builds the `FsOps` from the plain one and the two trees (to configure hooks, a mover...).
    pub async fn with(build: impl FnOnce(FsOps, [&Tree; 2]) -> FsOps) -> Self {
        let app = TestApp::new().await;
        let (a, ta) = app.add_volume("a").await;
        let (b, tb) = app.add_volume("b").await;
        let ops = build(ops_of(&app), [&ta, &tb]);
        Self {
            app,
            a,
            b,
            ta,
            tb,
            ops,
        }
    }

    pub fn tree(&self, volume: VolumeId) -> &Tree {
        if volume == self.a { &self.ta } else { &self.tb }
    }

    /// A second `FsOps` over the same state, without hooks or failpoints: what the process is
    /// after a restart (nothing it remembered survives).
    pub fn restarted(&self) -> FsOps {
        ops_of(&self.app)
    }

    pub async fn scan(&self) -> ScanSummary {
        self.app
            .state
            .indexer
            .scan(&ScanRequest::default(), &RunContext::noop())
            .await
            .unwrap()
    }

    /// A full scan must find the index exactly as the disk is.
    pub async fn assert_in_sync(&self) {
        self.assert_in_sync_for("").await;
    }

    /// [`Fx::assert_in_sync`] with a label for the failure message.
    pub async fn assert_in_sync_for(&self, label: &str) {
        let summary = self.scan().await;
        let changes = (
            summary.added,
            summary.changed,
            summary.removed,
            summary.dirs_added,
            summary.dirs_removed,
            summary.held,
        );
        assert_eq!(
            changes,
            (0, 0, 0, 0, 0, 0),
            "{label}: the scan found work: {summary:?}"
        );
    }

    pub async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// `(id, size, mtime)` of the `files` row at `path`.
    pub async fn row(&self, volume: VolumeId, path: &str) -> Option<(i64, i64, i64)> {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        sqlx::query_as(
            "SELECT id, size, mtime FROM files WHERE volume_id = ? AND dir_path = ? AND name = ?",
        )
        .bind(volume)
        .bind(dir)
        .bind(name)
        .fetch_optional(self.app.state.db.read())
        .await
        .unwrap()
    }

    pub async fn row_id(&self, volume: VolumeId, path: &str) -> i64 {
        self.row(volume, path)
            .await
            .unwrap_or_else(|| panic!("no index row at {volume}:{path}"))
            .0
    }

    /// Every indexed path of a volume.
    pub async fn paths(&self, volume: VolumeId) -> BTreeSet<String> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT CASE WHEN dir_path = '' THEN name ELSE dir_path || '/' || name END \
             FROM files WHERE volume_id = ?",
        )
        .bind(volume)
        .fetch_all(self.app.state.db.read())
        .await
        .unwrap();
        rows.into_iter().collect()
    }

    /// `(op, state)` of every journal row, oldest first.
    pub async fn journal(&self) -> Vec<(String, String)> {
        sqlx::query_as("SELECT op, state FROM fs_journal ORDER BY id")
            .fetch_all(self.app.state.db.read())
            .await
            .unwrap()
    }

    pub async fn pending_rows(&self) -> i64 {
        self.scalar("SELECT COUNT(*) FROM fs_journal WHERE state = 'pending'")
            .await
    }

    /// The same bytes appear as often as before, on both volumes, the trash included and the part
    /// files not counted: nothing was lost and nothing was invented.
    pub fn content_census(&self) -> BTreeMap<Vec<u8>, usize> {
        let mut census = BTreeMap::new();
        for tree in [&self.ta, &self.tb] {
            for (path, bytes) in disk_files(tree.root()) {
                if !is_part(&path) {
                    *census.entry(bytes).or_insert(0) += 1;
                }
            }
        }
        census
    }

    /// Every part file on both volumes.
    pub fn leftover_parts(&self) -> Vec<String> {
        [&self.ta, &self.tb]
            .into_iter()
            .flat_map(|tree| disk_files(tree.root()).into_keys())
            .filter(|path| is_part(path))
            .collect()
    }

    /// `a` or `b`, for messages and comparisons.
    pub fn label(&self, volume: VolumeId) -> &'static str {
        if volume == self.a { "a" } else { "b" }
    }

    /// The SHA-1 recorded for a row.
    pub async fn hash_of(&self, id: i64) -> Option<Vec<u8>> {
        sqlx::query_scalar("SELECT hash FROM files WHERE id = ?")
            .bind(id)
            .fetch_one(self.app.state.db.read())
            .await
            .unwrap()
    }

    /// What the index remembers about a row besides where it is.
    pub async fn memory_of(&self, id: i64) -> Memory {
        type Columns = (
            i64,
            i64,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<Vec<u8>>,
            i64,
            i64,
        );
        let columns: Columns = sqlx::query_as(
            "SELECT f.rating, f.flag, f.capture_ts, f.capture_offset_min, f.capture_src, c.name, \
             f.width, f.height, f.duration_ms, f.mime, f.hash, f.meta_state, f.hash_state \
             FROM files f LEFT JOIN cameras c ON c.id = f.camera_id WHERE f.id = ?",
        )
        .bind(id)
        .fetch_one(self.app.state.db.read())
        .await
        .unwrap();
        let tags = sqlx::query_scalar(
            "SELECT t.name FROM file_tags ft JOIN tags t ON t.id = ft.tag_id \
             WHERE ft.file_id = ? ORDER BY t.name",
        )
        .bind(id)
        .fetch_all(self.app.state.db.read())
        .await
        .unwrap();
        Memory {
            rating: columns.0,
            flag: columns.1,
            capture_ts: columns.2,
            capture_offset_min: columns.3,
            capture_src: columns.4,
            camera: columns.5,
            width: columns.6,
            height: columns.7,
            duration_ms: columns.8,
            mime: columns.9,
            hash: columns.10,
            meta_state: columns.11,
            hash_state: columns.12,
            tags,
        }
    }

    /// Gives a row what the pipeline and the user would have given it: capture data, a camera,
    /// the size of the picture, the hash of `content`, a rating, a flag, two tags, all pipeline
    /// stages done.
    pub async fn decorate(&self, id: i64, content: &[u8]) -> Memory {
        let db = self.app.state.db.write();
        sqlx::query("INSERT OR IGNORE INTO cameras (name) VALUES ('Fuji X-T5')")
            .execute(db)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE files SET rating = 4, flag = 1, capture_ts = 1500000000, \
             capture_offset_min = 120, capture_src = 'exif', \
             camera_id = (SELECT id FROM cameras WHERE name = 'Fuji X-T5'), \
             width = 6000, height = 4000, duration_ms = 1200, mime = 'image/jpeg', hash = ?, \
             meta_state = 1, hash_state = 1, thumb_state = 1 WHERE id = ?",
        )
        .bind(sha1_bytes(content).to_vec())
        .bind(id)
        .execute(db)
        .await
        .unwrap();
        for tag in ["family", "holiday"] {
            sqlx::query("INSERT OR IGNORE INTO tags (name) VALUES (?)")
                .bind(tag)
                .execute(db)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO file_tags (file_id, tag_id) SELECT ?, id FROM tags WHERE name = ?",
            )
            .bind(id)
            .bind(tag)
            .execute(db)
            .await
            .unwrap();
        }
        let memory = self.memory_of(id).await;
        assert_ne!(memory, Memory::default(), "the row was not decorated");
        memory
    }

    /// Every trash item, oldest first.
    pub async fn trash_items(&self) -> Vec<TrashRow> {
        type Row = (
            i64,
            VolumeId,
            String,
            String,
            String,
            i64,
            Option<String>,
            bool,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, volume_id, batch, orig_path, trash_path, is_dir, reason, \
             meta_json IS NOT NULL FROM trash_items ORDER BY id",
        )
        .fetch_all(self.app.state.db.read())
        .await
        .unwrap();
        rows.into_iter()
            .map(
                |(id, volume_id, batch, orig_path, trash_path, is_dir, reason, has_meta)| {
                    TrashRow {
                        id,
                        volume_id,
                        batch,
                        orig_path,
                        trash_path,
                        is_dir: is_dir != 0,
                        reason,
                        has_meta,
                    }
                },
            )
            .collect()
    }

    /// The sidecar links as `(owner, sidecar, kind)`, each file written `volume:path`.
    pub async fn links(&self) -> BTreeSet<(String, String, String)> {
        type Row = (VolumeId, String, String, VolumeId, String, String, String);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT o.volume_id, o.dir_path, o.name, s.volume_id, s.dir_path, s.name, fs.kind \
             FROM file_sidecars fs \
             JOIN files o ON o.id = fs.file_id \
             JOIN files s ON s.id = fs.sidecar_id",
        )
        .fetch_all(self.app.state.db.read())
        .await
        .unwrap();
        let name = |volume: VolumeId, dir: &str, name: &str| {
            let path = if dir.is_empty() {
                name.to_owned()
            } else {
                format!("{dir}/{name}")
            };
            format!("{}:{path}", self.label(volume))
        };
        rows.into_iter()
            .map(|(ov, od, on, sv, sd, sn, kind)| (name(ov, &od, &on), name(sv, &sd, &sn), kind))
            .collect()
    }

    /// The file is on the disk with this content and indexed; its row id.
    pub async fn present(&self, volume: VolumeId, path: &str, content: &[u8], label: &str) -> i64 {
        let name = format!("{}:{path}", self.label(volume));
        let on_disk = std::fs::read(self.tree(volume).path(path))
            .unwrap_or_else(|err| panic!("{label}: {name} is not on the disk: {err}"));
        assert_eq!(on_disk, content, "{label}: the content of {name}");
        match self.row(volume, path).await {
            Some((id, ..)) => id,
            None => panic!("{label}: {name} is not in the index"),
        }
    }

    /// Neither on the disk nor in the index.
    pub async fn gone(&self, volume: VolumeId, path: &str, label: &str) {
        let name = format!("{}:{path}", self.label(volume));
        assert!(
            std::fs::symlink_metadata(self.tree(volume).path(path)).is_err(),
            "{label}: {name} is still on the disk"
        );
        assert!(
            self.row(volume, path).await.is_none(),
            "{label}: {name} is still in the index"
        );
    }

    /// The journal, `(op, state)` oldest first.
    pub async fn assert_journal(&self, expected: &[(&str, &str)], label: &str) {
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(op, state)| ((*op).to_owned(), (*state).to_owned()))
            .collect();
        assert_eq!(self.journal().await, expected, "{label}: the journal");
    }

    /// Whether the index has a `dirs` row for the folder.
    pub async fn dir_row(&self, volume: VolumeId, path: &str) -> bool {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM dirs WHERE volume_id = ? AND path = ?")
                .bind(volume)
                .bind(path)
                .fetch_one(self.app.state.db.read())
                .await
                .unwrap();
        count > 0
    }

    /// Changes one setting the way the settings service does.
    pub async fn keep_cross_volume_sources_in_trash(&self) {
        let mut settings = (*self.app.state.settings.get()).clone();
        settings.keep_cross_volume_source_in_trash = true;
        self.app.state.settings.update(settings).await.unwrap();
    }
}

/// What the index remembers about a file besides where it is: what the pipeline found and what
/// the user chose. The thumbnail state is left out: thumbnails are cached per row id, so a new
/// row redoes them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    pub rating: i64,
    pub flag: i64,
    pub capture_ts: Option<i64>,
    pub capture_offset_min: Option<i64>,
    pub capture_src: Option<String>,
    pub camera: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub mime: Option<String>,
    pub hash: Option<Vec<u8>>,
    pub meta_state: i64,
    pub hash_state: i64,
    pub tags: Vec<String>,
}

/// One row of `trash_items`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashRow {
    pub id: i64,
    pub volume_id: VolumeId,
    pub batch: String,
    pub orig_path: String,
    pub trash_path: String,
    pub is_dir: bool,
    pub reason: Option<String>,
    /// Whether the item remembers a snapshot of its index row.
    pub has_meta: bool,
}

/// How often each content appears on the volumes (see [`Fx::content_census`]).
pub type Census = BTreeMap<Vec<u8>, usize>;

/// A simulated crash the `nth` time a step is reached (1-based), once: the operation returns
/// `aborted` and leaves behind what a killed process would.
#[derive(Clone, Default)]
pub struct Trap(Arc<Mutex<Option<(Step, usize)>>>);

impl Trap {
    pub fn arm(&self, step: Step, nth: usize) {
        *self.0.lock().unwrap() = Some((step, nth));
    }

    /// Whether the crash has not happened (yet).
    pub fn armed(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }

    pub fn failpoints(&self) -> Failpoints {
        let armed = Arc::clone(&self.0);
        Failpoints::new(move |reached| {
            let mut armed = armed.lock().unwrap();
            let Some((step, left)) = armed.as_mut() else {
                return FailAction::Continue;
            };
            if *step != reached {
                return FailAction::Continue;
            }
            *left -= 1;
            if *left > 0 {
                return FailAction::Continue;
            }
            *armed = None;
            FailAction::Abort
        })
    }
}

/// A fixture whose `FsOps` crashes where `trap` says. `split` puts the two volumes on different
/// "devices", so moves between them copy.
pub async fn crash_fx(trap: &Trap, split: bool) -> Fx {
    let points = trap.failpoints();
    Fx::with(move |ops, trees| {
        let ops = ops.with_failpoints(points);
        if split {
            ops.with_mover(Arc::new(SplitMover::new(&trees)))
        } else {
            ops
        }
    })
    .await
}

/// The restart after a crash: reconciles with a fresh `FsOps` and checks what must hold whatever
/// the crash was. No row is left pending and no part file is left; no content was lost or
/// invented (`census`); recovery is repeatable and finds nothing the second time; and the index
/// equals the disk once the folders the report names are rescanned. `new_dirs` allows the empty
/// folders a crashed operation made on its way.
pub async fn recover(fx: &Fx, label: &str, census: &Census, new_dirs: bool) -> ReconcileReport {
    let report = fx.restarted().reconcile().await.unwrap();
    assert!(report.errors.is_empty(), "{label}: {:?}", report.errors);
    assert_eq!(report.still_pending, 0, "{label}: {report:?}");
    assert_eq!(
        report.examined,
        report.completed + report.abandoned,
        "{label}: every row examined was settled one way or the other: {report:?}"
    );
    assert_eq!(fx.pending_rows().await, 0, "{label}");
    assert_eq!(fx.leftover_parts(), Vec::<String>::new(), "{label}");
    assert_eq!(
        &fx.content_census(),
        census,
        "{label}: content was lost or invented"
    );

    let again = fx.restarted().reconcile().await.unwrap();
    assert_eq!(
        (
            again.examined,
            again.completed,
            again.abandoned,
            again.parts_removed,
            again.stray_parts_swept,
            again.links_dropped
        ),
        (0, 0, 0, 0, 0, 0),
        "{label}: a second recovery found work: {again:?}"
    );

    if !report.rescan.is_empty() {
        let request = ScanRequest {
            targets: report.rescan.clone(),
            ..ScanRequest::default()
        };
        fx.app
            .state
            .indexer
            .scan(&request, &RunContext::noop())
            .await
            .unwrap();
    }
    if new_dirs {
        let summary = fx.scan().await;
        let changes = (
            summary.added,
            summary.changed,
            summary.removed,
            summary.held,
            summary.dirs_removed,
        );
        assert_eq!(changes, (0, 0, 0, 0, 0), "{label}: {summary:?}");
    }
    fx.assert_in_sync_for(label).await;
    report
}

// --- crash scenarios ---------------------------------------------------------------------------

/// What a crash at one step must come to once the server restarts and recovers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// The crash came before anything changed: recovery abandons the row and everything is where
    /// it was.
    Undone,
    /// A preparatory step (trashing the file about to be replaced, or the junk of a folder about
    /// to be removed) finished and the operation itself did not: recovery finishes the former
    /// and abandons the latter.
    Partial,
    /// The disk work was complete: recovery finishes the index change.
    Done,
    /// The operation never reaches this step, so there is no crash: it just succeeds.
    Unreached,
}

/// One crash of a [`Scenario`]: the `nth` time `step` is reached.
#[derive(Debug, Clone, Copy)]
pub struct Case {
    pub step: Step,
    pub nth: usize,
    pub fate: Fate,
}

impl Case {
    pub fn label(&self) -> String {
        format!("{:?} #{} ({:?})", self.step, self.nth, self.fate)
    }
}

/// Every step there is: a scenario's matrix names each of them, so a step a later change adds to
/// or removes from an operation cannot go unnoticed.
pub const ALL_STEPS: [Step; 8] = [
    Step::AfterJournal,
    Step::AfterPartCreate,
    Step::AfterCopy,
    Step::AfterFsync,
    Step::AfterVerify,
    Step::AfterFinalRename,
    Step::BeforeUnlink,
    Step::BeforeIndexTx,
];

/// One operation, crashed at each of its steps in turn.
#[allow(async_fn_in_trait)]
pub trait Scenario {
    /// What `arrange` builds and `perform` and `verify` need to know about it.
    type State;
    /// Whether the volumes `a` and `b` are on different "devices".
    const SPLIT: bool = false;
    /// `(step, nth time it is reached, what recovery must make of the crash)`.
    const MATRIX: &'static [(Step, usize, Fate)];

    /// Builds the disk and the index; no crash can happen yet.
    async fn arrange(fx: &Fx) -> Self::State;

    /// The operation under test.
    async fn perform(fx: &Fx, state: &Self::State) -> Result<(), FsOpError>;

    /// How often each content must appear after a crash that ended as `fate` (a finished
    /// operation for `Done` and `Unreached`).
    fn census(before: &Census, _fate: Fate) -> Census {
        before.clone()
    }

    /// What recovery left, beyond the checks [`recover`] makes for every crash.
    async fn verify(fx: &Fx, state: &Self::State, case: &Case, report: &ReconcileReport);
}

/// Runs the scenario once per matrix entry, each on a fresh fixture.
pub async fn crash_at_every_step<S: Scenario>() {
    for step in ALL_STEPS {
        assert!(
            S::MATRIX.iter().any(|(listed, ..)| *listed == step),
            "the matrix does not say what {step:?} does"
        );
    }
    for &(step, nth, fate) in S::MATRIX {
        let case = Case { step, nth, fate };
        let label = case.label();
        println!("crash at {label}");
        let trap = Trap::default();
        let fx = crash_fx(&trap, S::SPLIT).await;
        let state = S::arrange(&fx).await;
        let before = fx.content_census();
        trap.arm(step, nth);
        let result = S::perform(&fx, &state).await;

        if fate == Fate::Unreached {
            assert_eq!(result, Ok(()), "{label}");
            assert!(trap.armed(), "{label}: the operation reached the step");
            assert_eq!(fx.pending_rows().await, 0, "{label}");
            assert_eq!(
                fx.content_census(),
                S::census(&before, Fate::Done),
                "{label}: content was lost or invented"
            );
            fx.assert_in_sync_for(&label).await;
            continue;
        }
        assert_eq!(result, Err(FsOpError::Aborted), "{label}");
        assert!(
            !trap.armed(),
            "{label}: the operation never reached the step"
        );
        let census = S::census(&before, fate);
        let report = recover(&fx, &label, &census, fate == Fate::Undone).await;
        S::verify(&fx, &state, &case, &report).await;
    }
}

pub fn is_part(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.starts_with(".apm-") && name.ends_with(".apm-part")
}

/// Every regular file below `root` (links are not followed or listed): path with `/` -> content.
pub fn disk_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                walk(&entry.path(), &path, out);
            } else if kind.is_file() {
                out.insert(path, std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, "", &mut out);
    out
}

/// The folders below `root` (the root not included), with `/`.
pub fn disk_dirs(root: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                out.insert(path.clone());
                walk(&entry.path(), &path, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, "", &mut out);
    out
}

/// Whether the file system folds the case of names (the default of macOS, never of Linux).
pub fn case_insensitive(scratch: &Path) -> bool {
    let probe = scratch.join(".apm-case-probe");
    std::fs::write(&probe, b"x").unwrap();
    let folded = scratch.join(".APM-CASE-PROBE").exists();
    std::fs::remove_file(&probe).unwrap();
    folded
}

/// Whether file permissions are enforced for this user (they are not for root), so a test that
/// makes a folder read-only can tell when it cannot work.
pub fn permissions_are_enforced(scratch: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let probe = scratch.join(".apm-permission-probe");
    std::fs::create_dir(&probe).unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o555)).unwrap();
    let enforced = std::fs::File::create(probe.join("x")).is_err();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_dir_all(&probe).unwrap();
    enforced
}

pub fn shared(ops: &FsOps) -> Arc<FsOps> {
    Arc::new(ops.clone())
}
