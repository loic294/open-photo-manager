//! Tests of the stage queue. The queue is checked against a model: the whole `files` table is read
//! back, and the scope rule, the state, the order and the cursor are applied in plain Rust, written
//! from the contract and not from the SQL (no `Subtree`, no `plan_targets`).

use std::fs;

use sqlx::Row as _;

use super::*;
use crate::config::Config;
use crate::db::TempDb;
use crate::db::models::file_kind_to_db;
use crate::db::testing::{insert_file, insert_volume};
use crate::indexer::VolumeRegistry;
use crate::settings::SettingsService;

const COLUMNS: [PipelineColumn; 3] = [
    PipelineColumn::Meta,
    PipelineColumn::Hash,
    PipelineColumn::Thumb,
];

const KINDS: [FileKind; 5] = [
    FileKind::Image,
    FileKind::Video,
    FileKind::Audio,
    FileKind::Sidecar,
    FileKind::Other,
];

/// Directories that are easy to get wrong: prefix siblings, neighbours of `/` and `0` in the byte
/// order, case and Unicode variants, astral code points.
const DIRS: &[&str] = &[
    "",
    "2019",
    "2019/Rome",
    "2019/Rome/day 1",
    "2019.old",
    "2019.old/x",
    "2019-x",
    "2019 b",
    "2019!",
    "2019~",
    "20190",
    "20191/a",
    "2019_x",
    "2018",
    "2020",
    "201",
    "20",
    "a",
    "a/b",
    "a/b/c",
    "a/bb",
    "a/b.c",
    "a b",
    "a-b",
    "a0",
    "a1",
    "A",
    "A/b",
    "\u{e9}t\u{e9}",
    "\u{e9}t\u{e9}/p",
    "e\u{301}te\u{301}",
    "e\u{301}te\u{301}/p",
    "\u{65e5}\u{672c}/\u{5199}\u{771f}",
    "\u{65e5}\u{672c}0",
    "emoji/\u{1f600}",
    "emoji\u{1f600}",
    "z",
];

/// Names that tie and nest in the byte order: one is the prefix of another, case and accents.
const NAMES: &[&str] = &["a.jpg", "a.jpg.xmp", "b.jpg", "B.jpg", "\u{e4}.jpg"];

/// A row of the `files` table as the model knows it.
#[derive(Debug, Clone)]
struct Model {
    file: PendingFile,
    /// `meta_state`, `hash_state`, `thumb_state`, in the order of [`PipelineColumn`].
    states: [i64; 3],
}

/// The state a seeded row gets, different for every column so that the three queues differ:
/// pending for three rows out of seven.
fn seeded_state(n: usize, salt: usize) -> i64 {
    match (n * 31 + salt * 17) % 7 {
        0..=2 => 0,
        3 | 4 => 1,
        5 => 2,
        _ => 3,
    }
}

/// Reads the whole table back, ordered like the queue.
async fn load_model(db: &Db) -> Vec<Model> {
    let rows = sqlx::query(
        "SELECT id, volume_id, dir_path, name, kind, size, mtime, btime, \
                meta_state, hash_state, thumb_state FROM files",
    )
    .fetch_all(db.write())
    .await
    .unwrap();
    let mut model: Vec<Model> = rows
        .iter()
        .map(|row| Model {
            file: pending_from_row(row).unwrap(),
            states: [
                row.get("meta_state"),
                row.get("hash_state"),
                row.get("thumb_state"),
            ],
        })
        .collect();
    model.sort_by_key(|row| PendingKey::from(&row.file));
    model
}

/// Is a file of directory `dir` on `volume_id` in the scope of `target`? Straight from the
/// definition: the directory itself, and for a recursive target every directory below it (the
/// base followed by a `/`, so `2019.old` is not below `2019`).
fn covered_by(target: &ScanTarget, volume_id: VolumeId, dir: &str) -> bool {
    if target.volume_id != volume_id {
        return false;
    }
    let base = target.dir.as_str();
    dir == base
        || (target.recursive
            && (base.is_empty()
                || dir
                    .strip_prefix(base)
                    .is_some_and(|rest| rest.starts_with('/'))))
}

/// Is `file` in the scope of `req`, given the volumes that are online?
fn in_scope(req: &ScanRequest, online: &[VolumeId], file: &PendingFile) -> bool {
    online.contains(&file.volume_id)
        && (req.targets.is_empty()
            || req
                .targets
                .iter()
                .any(|target| covered_by(target, file.volume_id, &file.dir_path)))
}

/// What a queue must return: the pending rows of `column` in scope that `keep` accepts, after the
/// cursor, in queue order.
fn expected(
    model: &[Model],
    column: PipelineColumn,
    req: &ScanRequest,
    online: &[VolumeId],
    keep: &dyn Fn(&Model) -> bool,
    after: Option<&PendingKey>,
) -> Vec<PendingFile> {
    model
        .iter()
        .filter(|row| row.states[column as usize] == 0)
        .filter(|row| in_scope(req, online, &row.file))
        .filter(|row| keep(row))
        .filter(|row| after.is_none_or(|key| PendingKey::from(&row.file) > *key))
        .map(|row| row.file.clone())
        .collect()
}

fn totals_of(files: &[PendingFile]) -> QueueTotals {
    QueueTotals {
        files: files.len() as u64,
        bytes: files.iter().map(|file| file.size as u64).sum(),
    }
}

/// Fails with the first difference instead of dumping two long lists.
fn assert_rows(label: &str, got: &[PendingFile], want: &[PendingFile]) {
    for (n, (got, want)) in got.iter().zip(want).enumerate() {
        assert_eq!(got, want, "{label}: row {n} differs");
    }
    assert_eq!(
        got.len(),
        want.len(),
        "{label}: {} rows, expected {}; the first extra one is {:?}",
        got.len(),
        want.len(),
        got.get(want.len()).or_else(|| want.get(got.len()))
    );
}

/// Three volumes of 37 directories with 5 files each, in states that differ per column and per row.
struct Library {
    db: TempDb,
    volumes: [VolumeId; 3],
    model: Vec<Model>,
}

impl Library {
    /// The volumes a run may read: the third one is "offline".
    fn online(&self) -> [VolumeId; 2] {
        [self.volumes[0], self.volumes[1]]
    }

    fn expected(
        &self,
        column: PipelineColumn,
        req: &ScanRequest,
        keep: &dyn Fn(&Model) -> bool,
        after: Option<&PendingKey>,
    ) -> Vec<PendingFile> {
        expected(&self.model, column, req, &self.online(), keep, after)
    }

    fn queue(&self, column: PipelineColumn, req: &ScanRequest) -> PendingQueue {
        PendingQueue::new(column, StageScope::new(req, &self.online()))
    }
}

async fn library() -> Library {
    library_of(DIRS, NAMES).await
}

/// Three volumes that each hold every name in every directory.
async fn library_of(dirs: &[&str], names: &[&str]) -> Library {
    let db = Db::open_temp().await.unwrap();
    let mut volumes = [0; 3];
    for (slot, name) in volumes.iter_mut().zip(["photos", "scans", "archive"]) {
        *slot = insert_volume(&db, name).await;
    }
    let mut tx = db.begin_write().await.unwrap();
    let mut n = 0;
    for &volume_id in &volumes {
        for dir in dirs {
            for name in names {
                sqlx::query(
                    "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, kind, \
                                        size, mtime, btime, meta_state, hash_state, thumb_state) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(volume_id)
                .bind(dir)
                .bind(name)
                .bind(name.to_lowercase())
                .bind(name.to_lowercase())
                .bind(file_kind_to_db(KINDS[n % KINDS.len()]))
                .bind((n as i64 * 13) % 9 * 100)
                .bind(1_700_000_000 + n as i64)
                .bind((n % 3 != 0).then_some(1_600_000_000 + n as i64))
                .bind(seeded_state(n, 0))
                .bind(seeded_state(n, 1))
                .bind(seeded_state(n, 2))
                .execute(&mut *tx)
                .await
                .unwrap();
                n += 1;
            }
        }
    }
    tx.commit().await.unwrap();
    let model = load_model(&db).await;
    assert_eq!(model.len(), 3 * dirs.len() * names.len());
    Library { db, volumes, model }
}

fn subtree(volume_id: VolumeId, dir: &str) -> ScanTarget {
    ScanTarget::subtree(volume_id, rel(dir))
}

fn directory(volume_id: VolumeId, dir: &str) -> ScanTarget {
    ScanTarget::dir(volume_id, rel(dir))
}

fn request(targets: Vec<ScanTarget>) -> ScanRequest {
    ScanRequest {
        targets,
        ..ScanRequest::default()
    }
}

/// The requests of the matrix; `c` is the volume that is not online.
fn requests(volumes: [VolumeId; 3]) -> Vec<(&'static str, ScanRequest)> {
    let [a, b, c] = volumes;
    vec![
        ("everything", ScanRequest::default()),
        ("a whole volume", request(vec![ScanTarget::volume(a)])),
        (
            "two whole volumes, listed backwards",
            request(vec![ScanTarget::volume(b), ScanTarget::volume(a)]),
        ),
        ("a subtree", request(vec![subtree(a, "2019")])),
        (
            "a subtree and its look-alike",
            request(vec![subtree(a, "2019"), subtree(a, "2019.old")]),
        ),
        (
            "a subtree and one inside it",
            request(vec![subtree(a, "2019"), subtree(a, "2019/Rome")]),
        ),
        ("a directory alone", request(vec![directory(a, "2019")])),
        (
            "a directory and a subtree below it",
            request(vec![directory(a, "2019"), subtree(a, "2019/Rome")]),
        ),
        ("the root alone", request(vec![directory(a, "")])),
        (
            "the root alone and a subtree",
            request(vec![directory(a, ""), subtree(a, "a")]),
        ),
        (
            "several volumes",
            request(vec![
                subtree(b, "a"),
                directory(a, "\u{e9}t\u{e9}"),
                subtree(a, "\u{65e5}\u{672c}"),
            ]),
        ),
        (
            "volumes out of order",
            request(vec![subtree(b, "z"), subtree(a, "z"), subtree(b, "2018")]),
        ),
        (
            "a volume that is not online",
            request(vec![subtree(c, "2019"), subtree(a, "2019.old")]),
        ),
        (
            "only a volume that is not online",
            request(vec![ScanTarget::volume(c)]),
        ),
        ("an unknown volume", request(vec![ScanTarget::volume(999)])),
        (
            "a directory that holds nothing",
            request(vec![directory(a, "no/such/dir")]),
        ),
        (
            "the decomposed spelling",
            request(vec![subtree(a, "e\u{301}te\u{301}")]),
        ),
        (
            "one scope per directory, as the watcher sends them",
            request(DIRS.iter().map(|dir| directory(a, dir)).collect()),
        ),
        (
            "a scope per directory in both volumes, recursive and not",
            request(
                DIRS.iter()
                    .enumerate()
                    .flat_map(|(n, dir)| {
                        [
                            ScanTarget {
                                volume_id: a,
                                dir: rel(dir),
                                recursive: n % 3 == 0,
                            },
                            directory(b, dir),
                        ]
                    })
                    .collect(),
            ),
        ),
    ]
}

/// Cursors to continue after: every row of the table (pending or not, in scope or not) and keys
/// that no row has.
fn cursors(lib: &Library) -> Vec<Option<PendingKey>> {
    let [a, b, _] = lib.volumes;
    let key = |volume_id, dir: &str, name: &str| {
        Some(PendingKey {
            volume_id,
            dir_path: dir.to_owned(),
            name: name.to_owned(),
        })
    };
    let mut cursors = vec![
        None,
        key(0, "", ""),
        key(a, "", ""),
        key(a, "", "a.jpg"),
        key(a, "2019", ""),
        key(a, "2019/", ""),
        key(a, "2019.", "x"),
        key(a, "2019/Rome/day 1", "zzz"),
        key(a, "no/such/dir", "x"),
        key(a, "\u{10ffff}", "zzz"),
        key(b, "", ""),
        key(b, "z", "zzz"),
        key(lib.volumes[2], "", ""),
        key(99, "", ""),
    ];
    cursors.extend(
        lib.model
            .iter()
            .map(|row| Some(PendingKey::from(&row.file))),
    );
    cursors
}

async fn page_after_each(
    queue: &PendingQueue,
    db: &Db,
    cursors: &[Option<PendingKey>],
) -> Vec<Vec<PendingFile>> {
    futures_util::future::join_all(
        cursors
            .iter()
            .map(|cursor| queue.page(db, cursor.as_ref(), MAX_PAGE)),
    )
    .await
    .into_iter()
    .map(Result::unwrap)
    .collect()
}

/// More pages than any pass of these tests needs: a pass that reaches it is stuck.
const PASS_BOUND: usize = 2_000;

/// Reads a queue to its end `limit` rows at a time, the way a stage does.
async fn walk(queue: &PendingQueue, db: &Db, limit: usize) -> Vec<Vec<PendingFile>> {
    let mut pages = Vec::new();
    let mut after: Option<PendingKey> = None;
    loop {
        let page = queue.page(db, after.as_ref(), limit).await.unwrap();
        let end = page.len() < limit;
        if let Some(last) = page.last() {
            after = Some(PendingKey::from(last));
        }
        pages.push(page);
        if end {
            return pages;
        }
        assert!(pages.len() < PASS_BOUND, "the pass does not end");
    }
}

fn flatten(pages: Vec<Vec<PendingFile>>) -> Vec<PendingFile> {
    pages.into_iter().flatten().collect()
}

fn rel(path: &str) -> RelPath {
    RelPath::new(path).unwrap()
}

fn scope(path: &str, recursive: bool) -> Scope {
    Scope {
        dir: rel(path),
        recursive,
    }
}

fn plan(targets: &[ScanTarget]) -> Vec<(VolumeId, Vec<(String, bool)>)> {
    plan_targets(targets, &[7, 8])
        .into_iter()
        .map(|plan| {
            let scopes = plan
                .scopes
                .iter()
                .map(|scope| (scope.dir.as_str().to_owned(), scope.recursive))
                .collect();
            (plan.volume_id, scopes)
        })
        .collect()
}

#[test]
fn no_targets_means_every_online_volume_completely() {
    let plans = plan_targets(&[], &[7, 8]);
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[0].volume_id, 7);
    assert_eq!(plans[1].scopes, [scope("", true)]);
    assert!(plans.iter().all(VolumePlan::whole_volume));
}

#[test]
fn targets_are_grouped_by_volume() {
    let plans = plan(&[
        ScanTarget::subtree(8, rel("x")),
        ScanTarget::subtree(2, rel("a")),
        ScanTarget::dir(8, rel("y")),
    ]);
    assert_eq!(
        plans,
        [
            (2, vec![("a".to_owned(), true)]),
            (8, vec![("x".to_owned(), true), ("y".to_owned(), false)]),
        ]
    );
}

#[test]
fn a_scope_inside_a_recursive_one_is_dropped() {
    let plans = plan(&[
        ScanTarget::subtree(1, rel("a/b/c")),
        ScanTarget::dir(1, rel("a/b")),
        ScanTarget::subtree(1, rel("a")),
        ScanTarget::subtree(1, rel("a/b")),
    ]);
    assert_eq!(plans, [(1, vec![("a".to_owned(), true)])]);
}

#[test]
fn only_a_path_prefix_that_is_a_parent_covers() {
    let plans = plan(&[
        ScanTarget::subtree(1, rel("2019")),
        ScanTarget::subtree(1, rel("2019.old")),
        ScanTarget::subtree(1, rel("2019/Rome")),
    ]);
    assert_eq!(
        plans,
        [(
            1,
            vec![("2019".to_owned(), true), ("2019.old".to_owned(), true)]
        )]
    );
}

#[test]
fn a_non_recursive_scope_does_not_cover_what_is_below_it() {
    let plans = plan(&[
        ScanTarget::dir(1, rel("a")),
        ScanTarget::subtree(1, rel("a/b")),
        ScanTarget::dir(1, rel("a")),
    ]);
    assert_eq!(
        plans,
        [(1, vec![("a".to_owned(), false), ("a/b".to_owned(), true)])]
    );
}

#[test]
fn a_recursive_scope_absorbs_the_non_recursive_one_of_the_same_directory() {
    let plans = plan(&[
        ScanTarget::dir(1, rel("a")),
        ScanTarget::subtree(1, rel("a")),
    ]);
    assert_eq!(plans, [(1, vec![("a".to_owned(), true)])]);
}

#[test]
fn the_whole_volume_covers_everything() {
    let plans = plan_targets(
        &[
            ScanTarget::dir(1, rel("a")),
            ScanTarget::volume(1),
            ScanTarget::subtree(1, rel("b")),
        ],
        &[],
    );
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].scopes, [scope("", true)]);
    assert!(plans[0].whole_volume());
    assert!(!plan_targets(&[ScanTarget::dir(1, RelPath::root())], &[])[0].whole_volume());
}

#[test]
fn a_stage_scope_keeps_the_online_volumes_in_id_order() {
    let scope = StageScope::new(&ScanRequest::default(), &[3, 1, 3, 2]);
    assert_eq!(scope.volume_ids(), [1, 2, 3]);
    assert!(!scope.is_empty());

    let scope = StageScope::new(
        &request(vec![
            subtree(5, "a"),
            directory(2, "b"),
            subtree(5, "c"),
            subtree(9, "d"),
        ]),
        &[2, 5],
    );
    assert_eq!(scope.volume_ids(), [2, 5], "volume 9 is not online");

    assert!(StageScope::new(&ScanRequest::default(), &[]).is_empty());
    assert!(StageScope::new(&request(vec![ScanTarget::volume(9)]), &[1, 2]).is_empty());
}

#[test]
fn a_key_is_the_position_of_a_row_in_the_queue() {
    let file = PendingFile {
        id: 17,
        volume_id: 2,
        dir_path: "a/b".to_owned(),
        name: "x.jpg".to_owned(),
        kind: FileKind::Image,
        size: 5,
        mtime: 6,
        btime: None,
    };
    let key = PendingKey::from(&file);
    assert_eq!(
        (key.volume_id, key.dir_path.as_str(), key.name.as_str()),
        (2, "a/b", "x.jpg")
    );

    let key = |volume_id, dir: &str, name: &str| PendingKey {
        volume_id,
        dir_path: dir.to_owned(),
        name: name.to_owned(),
    };
    let mut keys = vec![
        key(2, "a", "a"),
        key(1, "b", "a"),
        key(1, "a/b", "a"),
        key(1, "a", "b"),
        key(1, "a", "B"),
        key(1, "a.b", "a"),
        key(1, "\u{e9}", "a"),
        key(1, "z", "a"),
        key(1, "", "z"),
    ];
    keys.sort();
    let order: Vec<_> = keys
        .iter()
        .map(|key| (key.volume_id, key.dir_path.as_str(), key.name.as_str()))
        .collect();
    assert_eq!(
        order,
        [
            (1, "", "z"),
            (1, "a", "B"),
            (1, "a", "b"),
            (1, "a.b", "a"),
            (1, "a/b", "a"),
            (1, "b", "a"),
            (1, "z", "a"),
            (1, "\u{e9}", "a"),
            (2, "a", "a"),
        ],
        "volume, then directory, then name, compared bytewise"
    );
}

#[test]
fn the_column_comes_from_the_enum() {
    assert_eq!(PipelineColumn::Meta.column(), "meta_state");
    assert_eq!(PipelineColumn::Hash.column(), "hash_state");
    assert_eq!(PipelineColumn::Thumb.column(), "thumb_state");
    assert_eq!(
        COLUMNS.map(|column| column as usize),
        [0, 1, 2],
        "the model indexes the states by the discriminant"
    );
}

#[tokio::test]
async fn the_scope_rule_is_the_one_of_a_subtree() {
    let lib = library().await;
    let a = lib.volumes[0];
    let dirs_of = |req: ScanRequest| -> Vec<String> {
        let mut dirs: Vec<String> = lib
            .expected(PipelineColumn::Meta, &req, &|_| true, None)
            .into_iter()
            .map(|file| file.dir_path)
            .collect();
        dirs.dedup();
        dirs
    };
    // The same expectation through the queue, so that the model is not trusted blindly.
    let queue_dirs = |req: ScanRequest| {
        let queue = lib.queue(PipelineColumn::Meta, &req);
        let db = &lib.db;
        async move {
            let mut dirs: Vec<String> = queue
                .page(db, None, MAX_PAGE)
                .await
                .unwrap()
                .into_iter()
                .map(|file| file.dir_path)
                .collect();
            dirs.dedup();
            dirs
        }
    };

    let subtree_2019 = request(vec![subtree(a, "2019")]);
    let dirs = queue_dirs(subtree_2019.clone()).await;
    assert_eq!(dirs, dirs_of(subtree_2019));
    assert_eq!(dirs, ["2019", "2019/Rome", "2019/Rome/day 1"]);

    let directory_2019 = request(vec![directory(a, "2019")]);
    assert_eq!(queue_dirs(directory_2019).await, ["2019"]);

    let root_only = request(vec![directory(a, "")]);
    assert_eq!(queue_dirs(root_only).await, [""]);

    let look_alikes = request(vec![subtree(a, "2019.old"), subtree(a, "a/b")]);
    assert_eq!(
        queue_dirs(look_alikes).await,
        ["2019.old", "2019.old/x", "a/b", "a/b/c"],
        "a/b.c lies between a/b and a/b/c in the order, and is not below a/b"
    );
}

#[tokio::test]
async fn every_request_reads_what_the_model_says() {
    let lib = library().await;
    let cursors = cursors(&lib);
    for (label, req) in requests(lib.volumes) {
        for (n, column) in COLUMNS.into_iter().enumerate() {
            let label = format!("{label}, {column:?}");
            let queue = lib.queue(column, &req);

            let want = lib.expected(column, &req, &|_| true, None);
            let got = queue.page(&lib.db, None, MAX_PAGE).await.unwrap();
            assert_rows(&label, &got, &want);
            assert_eq!(
                queue.totals(&lib.db).await.unwrap(),
                totals_of(&want),
                "{label}: totals"
            );

            // All the cursors for the first column, a few for the others.
            let step = if n == 0 { 1 } else { 9 };
            let some: Vec<_> = cursors.iter().step_by(step).cloned().collect();
            let pages = page_after_each(&queue, &lib.db, &some).await;
            for (cursor, got) in some.iter().zip(&pages) {
                let want = lib.expected(column, &req, &|_| true, cursor.as_ref());
                assert_rows(&format!("{label}, after {cursor:?}"), got, &want);
            }
        }
    }
}

#[tokio::test]
async fn a_request_for_a_volume_that_is_not_online_reads_nothing() {
    let lib = library().await;
    let [_, _, offline] = lib.volumes;
    for req in [
        request(vec![ScanTarget::volume(offline)]),
        request(vec![subtree(offline, "2019"), directory(offline, "")]),
    ] {
        let queue = lib.queue(PipelineColumn::Meta, &req);
        assert!(queue.page(&lib.db, None, 100).await.unwrap().is_empty());
        assert_eq!(queue.totals(&lib.db).await.unwrap(), QueueTotals::default());
    }
}

#[tokio::test]
async fn a_queue_reads_the_column_it_was_made_for() {
    let lib = library().await;
    let everything = ScanRequest::default();
    let sets: Vec<Vec<FileId>> = COLUMNS
        .into_iter()
        .map(|column| {
            lib.expected(column, &everything, &|_| true, None)
                .into_iter()
                .map(|file| file.id)
                .collect()
        })
        .collect();
    assert!(
        sets[0] != sets[1] && sets[1] != sets[2] && sets[0] != sets[2],
        "the seed must give the three queues different rows"
    );
    for (column, want) in COLUMNS.into_iter().zip(&sets) {
        let got: Vec<FileId> = lib
            .queue(column, &everything)
            .page(&lib.db, None, MAX_PAGE)
            .await
            .unwrap()
            .into_iter()
            .map(|file| file.id)
            .collect();
        assert_eq!(&got, want, "{column:?}");
    }
}

#[tokio::test]
async fn a_pass_visits_every_row_once_in_order_and_only_the_last_page_is_short() {
    let lib = library().await;
    let [a, b, _] = lib.volumes;
    for req in [
        ScanRequest::default(),
        request(vec![subtree(b, "a"), subtree(a, "2019"), directory(a, "")]),
    ] {
        let want = lib.expected(PipelineColumn::Meta, &req, &|_| true, None);
        assert!(want.len() > 14, "the pass must span several pages of 7");
        let queue = lib.queue(PipelineColumn::Meta, &req);
        for limit in [1, 2, 7, 64, want.len(), want.len() + 1, 500] {
            let pages = walk(&queue, &lib.db, limit).await;
            let (last, full) = pages.split_last().unwrap();
            assert!(
                full.iter().all(|page| page.len() == limit),
                "limit {limit}: a page that is not the last one is full"
            );
            assert!(last.len() < limit, "limit {limit}: the last page is short");
            assert_rows(&format!("limit {limit}"), &flatten(pages), &want);
        }
    }
}

#[tokio::test]
async fn a_page_runs_over_the_end_of_a_volume() {
    let lib = library().await;
    let [a, b, _] = lib.volumes;
    let req = ScanRequest::default();
    let in_a = lib.expected(
        PipelineColumn::Meta,
        &request(vec![ScanTarget::volume(a)]),
        &|_| true,
        None,
    );
    let last_in_a = PendingKey::from(in_a.last().unwrap());
    let page = lib
        .queue(PipelineColumn::Meta, &req)
        .page(&lib.db, Some(&last_in_a), 5)
        .await
        .unwrap();
    assert_eq!(page.len(), 5);
    assert!(page.iter().all(|file| file.volume_id == b));

    let before_end = PendingKey::from(&in_a[in_a.len() - 3]);
    let page = lib
        .queue(PipelineColumn::Meta, &req)
        .page(&lib.db, Some(&before_end), 5)
        .await
        .unwrap();
    let volumes: Vec<_> = page.iter().map(|file| file.volume_id).collect();
    assert_eq!(
        volumes,
        [a, a, b, b, b],
        "one page holds the end of a and the start of b"
    );
}

async fn set_state(db: &Db, column: PipelineColumn, id: FileId, state: i64) {
    let sql = match column {
        PipelineColumn::Meta => "UPDATE files SET meta_state = ? WHERE id = ?",
        PipelineColumn::Hash => "UPDATE files SET hash_state = ? WHERE id = ?",
        PipelineColumn::Thumb => "UPDATE files SET thumb_state = ? WHERE id = ?",
    };
    sqlx::query(sql)
        .bind(state)
        .bind(id)
        .execute(db.write())
        .await
        .unwrap();
}

async fn delete_file(db: &Db, id: FileId) {
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(id)
        .execute(db.write())
        .await
        .unwrap();
}

/// Reloads the model after the test changed the table, and checks the queue against it from the
/// start and after `cursor`.
async fn assert_follows_table(
    lib: &mut Library,
    label: &str,
    queue: &PendingQueue,
    req: &ScanRequest,
    cursor: &PendingKey,
) {
    lib.model = load_model(&lib.db).await;
    for after in [None, Some(cursor)] {
        let want = lib.expected(PipelineColumn::Meta, req, &|_| true, after);
        let got = queue.page(&lib.db, after, MAX_PAGE).await.unwrap();
        assert_rows(&format!("{label}, after {after:?}"), &got, &want);
    }
}

#[tokio::test]
async fn a_cursor_survives_what_happens_to_the_rows_around_it() {
    let mut lib = library().await;
    let a = lib.volumes[0];
    let req = request(vec![ScanTarget::volume(a)]);
    let queue = lib.queue(PipelineColumn::Meta, &req);

    let first = queue.page(&lib.db, None, 10).await.unwrap();
    let cursor = PendingKey::from(first.last().unwrap());
    let ahead = queue.page(&lib.db, Some(&cursor), MAX_PAGE).await.unwrap();
    assert!(ahead.len() > 20);

    // The stage finishes what it read: those rows leave the queue and the rest does not move.
    for file in &first {
        set_state(&lib.db, PipelineColumn::Meta, file.id, 1).await;
    }
    assert_follows_table(&mut lib, "rows finished", &queue, &req, &cursor).await;
    let restarted = queue.page(&lib.db, None, MAX_PAGE).await.unwrap();
    assert_rows("a pass restarted from the start", &restarted, &ahead);

    // Rows get deleted, the one at the cursor among them.
    delete_file(&lib.db, first.last().unwrap().id).await;
    for file in &ahead[..2] {
        delete_file(&lib.db, file.id).await;
    }
    assert_follows_table(&mut lib, "rows deleted", &queue, &req, &cursor).await;
    let page = queue.page(&lib.db, Some(&cursor), MAX_PAGE).await.unwrap();
    assert_rows("without the deleted rows", &page, &ahead[2..]);

    // A row that arrives before the cursor is not seen by this pass, one after it is.
    let early = insert_file(&lib.db, a, &cursor.dir_path, "!early.jpg").await;
    let late = insert_file(&lib.db, a, &cursor.dir_path, &format!("{}~", cursor.name)).await;
    assert_follows_table(&mut lib, "rows inserted", &queue, &req, &cursor).await;
    let page = queue.page(&lib.db, Some(&cursor), MAX_PAGE).await.unwrap();
    assert!(page.iter().all(|file| file.id != early));
    assert!(page.iter().any(|file| file.id == late));
    let start = queue.page(&lib.db, None, MAX_PAGE).await.unwrap();
    assert!(start.iter().any(|file| file.id == early));

    // A finished row after the cursor that a rescan sets pending again comes back, and a pending
    // one that a stage finishes meanwhile goes.
    let done = lib
        .model
        .iter()
        .find(|row| {
            row.states[0] != 0 && row.file.volume_id == a && PendingKey::from(&row.file) > cursor
        })
        .map(|row| row.file.id)
        .unwrap();
    set_state(&lib.db, PipelineColumn::Meta, done, 0).await;
    set_state(&lib.db, PipelineColumn::Meta, ahead[5].id, 2).await;
    assert_follows_table(&mut lib, "states changed", &queue, &req, &cursor).await;
    let page = queue.page(&lib.db, Some(&cursor), MAX_PAGE).await.unwrap();
    assert!(page.iter().any(|file| file.id == done));
    assert!(page.iter().all(|file| file.id != ahead[5].id));
}

#[tokio::test]
async fn a_pass_that_finishes_every_row_it_reads_sees_each_one_once() {
    let lib = library().await;
    let [a, b, _] = lib.volumes;
    let req = request(vec![subtree(b, ""), subtree(a, "2019")]);
    for column in COLUMNS {
        let queue = lib.queue(column, &req);
        let want = lib.expected(column, &req, &|_| true, None);
        let mut left = totals_of(&want);
        let mut seen = Vec::new();
        let mut after = None;
        let mut pages = 0;
        loop {
            pages += 1;
            assert!(pages < PASS_BOUND, "the pass does not end");
            let page = queue.page(&lib.db, after.as_ref(), 13).await.unwrap();
            let restarted = queue.page(&lib.db, None, 13).await.unwrap();
            assert_rows("a pass restarted from the start", &restarted, &page);
            let Some(last) = page.last() else { break };
            after = Some(PendingKey::from(last));
            for file in &page {
                set_state(&lib.db, column, file.id, 1).await;
            }
            left.files -= page.len() as u64;
            left.bytes -= page.iter().map(|file| file.size as u64).sum::<u64>();
            assert_eq!(queue.totals(&lib.db).await.unwrap(), left, "{column:?}");
            seen.extend(page);
        }
        assert_rows(&format!("{column:?}"), &seen, &want);
        assert_eq!(left, QueueTotals::default());
    }
}

const HAS_SIDECAR: &str = "EXISTS (SELECT 1 FROM files o WHERE o.volume_id = f.volume_id \
     AND o.dir_path = f.dir_path AND o.name = f.name || '.xmp')";
const HAS_NO_SIDECAR: &str = "NOT EXISTS (SELECT 1 FROM files o WHERE o.volume_id = f.volume_id \
     AND o.dir_path = f.dir_path AND o.name = f.name || '.xmp')";

/// Reads the queue that `with_condition` builds from `conditions` for some requests of the matrix
/// and checks it against the model. Returns how many rows of the first column the conditions keep
/// in the whole library, and how many it has without them.
async fn check_condition(
    lib: &Library,
    label: &str,
    conditions: &[&'static str],
    keep: &dyn Fn(&Model) -> bool,
) -> (usize, usize) {
    let cursors = cursors(lib);
    for (name, req) in requests(lib.volumes).into_iter().step_by(4) {
        for column in COLUMNS {
            let label = format!("{label}, {name}, {column:?}");
            let queue = conditions
                .iter()
                .fold(lib.queue(column, &req), |queue, sql| {
                    queue.with_condition(sql)
                });

            let want = lib.expected(column, &req, keep, None);
            let got = queue.page(&lib.db, None, MAX_PAGE).await.unwrap();
            assert_rows(&label, &got, &want);
            assert_eq!(
                queue.totals(&lib.db).await.unwrap(),
                totals_of(&want),
                "{label}: totals"
            );
            let pages = flatten(walk(&queue, &lib.db, 7).await);
            assert_rows(&format!("{label}, 7 at a time"), &pages, &want);

            let some: Vec<_> = cursors.iter().step_by(37).cloned().collect();
            let pages = page_after_each(&queue, &lib.db, &some).await;
            for (cursor, got) in some.iter().zip(&pages) {
                let want = lib.expected(column, &req, keep, cursor.as_ref());
                assert_rows(&format!("{label}, after {cursor:?}"), got, &want);
            }
        }
    }
    let everything = ScanRequest::default();
    (
        lib.expected(PipelineColumn::Meta, &everything, keep, None)
            .len(),
        lib.expected(PipelineColumn::Meta, &everything, &|_| true, None)
            .len(),
    )
}

#[tokio::test]
async fn a_condition_narrows_pages_and_totals_alike() {
    let lib = library().await;
    let sidecars: HashSet<(VolumeId, String, String)> = lib
        .model
        .iter()
        .filter(|row| row.file.name.ends_with(".xmp"))
        .map(|row| {
            let name = row.file.name.trim_end_matches(".xmp");
            (
                row.file.volume_id,
                row.file.dir_path.clone(),
                name.to_owned(),
            )
        })
        .collect();
    let has_sidecar = |row: &Model| {
        sidecars.contains(&(
            row.file.volume_id,
            row.file.dir_path.clone(),
            row.file.name.clone(),
        ))
    };

    let (kept, all) = check_condition(&lib, "a column", &["f.size >= 400"], &|row| {
        row.file.size >= 400
    })
    .await;
    assert!(0 < kept && kept < all);

    let (kept, all) = check_condition(
        &lib,
        "two conditions",
        &["f.size >= 200", "f.kind = 'image'"],
        &|row| row.file.size >= 200 && row.file.kind == FileKind::Image,
    )
    .await;
    assert!(0 < kept && kept < all);

    // Without the parentheses around the first one, the AND would bind tighter than its OR.
    let (kept, all) = check_condition(
        &lib,
        "an OR inside a condition",
        &["f.kind = 'image' OR f.size = 0", "f.id % 2 = 0"],
        &|row| (row.file.kind == FileKind::Image || row.file.size == 0) && row.file.id % 2 == 0,
    )
    .await;
    assert!(0 < kept && kept < all);

    let (with, all) = check_condition(&lib, "EXISTS", &[HAS_SIDECAR], &has_sidecar).await;
    assert!(0 < with && with < all);
    let (without, all_again) = check_condition(&lib, "NOT EXISTS", &[HAS_NO_SIDECAR], &|row| {
        !has_sidecar(row)
    })
    .await;
    assert_eq!(all, all_again);
    assert_eq!(with + without, all, "the two halves partition the queue");

    let (kept, _) = check_condition(&lib, "nothing", &["1 = 0"], &|_| false).await;
    assert_eq!(kept, 0);
    let (kept, all) = check_condition(&lib, "everything", &["1 = 1"], &|_| true).await;
    assert_eq!(kept, all);
    let (kept, all) = check_condition(&lib, "a quoted ?", &["f.name <> '?'"], &|_| true).await;
    assert_eq!(kept, all);
}

#[tokio::test]
async fn a_condition_does_not_change_the_queue_it_was_added_to() {
    let lib = library().await;
    let plain = lib.queue(PipelineColumn::Meta, &ScanRequest::default());
    let narrowed = plain.clone().with_condition("1 = 0");
    assert!(narrowed.page(&lib.db, None, 10).await.unwrap().is_empty());
    assert_eq!(plain.page(&lib.db, None, 10).await.unwrap().len(), 10);
}

/// `count` files `f<i>.jpg` of one volume, in directories chosen by the statement: the first `?` is
/// the last index, the second the volume.
async fn seed(db: &Db, statement: &'static str, volume_id: VolumeId, count: usize) {
    sqlx::query(statement)
        .bind(count as i64 - 1)
        .bind(volume_id)
        .execute(db.write())
        .await
        .unwrap();
}

/// Files 50 to a directory, named so that they sort like their index.
const SEQUENCE: &str = "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?) \
     INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, kind, size, mtime) \
     SELECT ?, d, f, f, f, 'image', i, 1 FROM \
     (SELECT i, printf('d%03d', i / 50) AS d, printf('f%05d.jpg', i) AS f FROM n)";

#[tokio::test]
async fn the_page_length_is_clamped_to_what_a_stage_can_hold() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    let rows = MAX_PAGE + 600;
    seed(&db, SEQUENCE, volume, rows).await;
    let scope = StageScope::new(&ScanRequest::default(), &[volume]);
    let queue = PendingQueue::new(PipelineColumn::Meta, scope);

    for (asked, got) in [
        (0, 1),
        (1, 1),
        (2, 2),
        (MAX_PAGE - 1, MAX_PAGE - 1),
        (MAX_PAGE, MAX_PAGE),
        (MAX_PAGE + 1, MAX_PAGE),
        (usize::MAX, MAX_PAGE),
    ] {
        let page = queue.page(&db, None, asked).await.unwrap();
        assert_eq!(page.len(), got, "asked for {asked}");
    }

    // A pass with the longest page still visits every row once.
    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        pages += 1;
        assert!(pages < PASS_BOUND, "the pass does not end");
        let page = queue.page(&db, after.as_ref(), usize::MAX).await.unwrap();
        let Some(last) = page.last() else { break };
        after = Some(PendingKey::from(last));
        seen.extend(page);
    }
    assert_eq!(seen.len(), rows);
    assert!(
        seen.windows(2)
            .all(|pair| { PendingKey::from(&pair[0]) < PendingKey::from(&pair[1]) })
    );
    let names: Vec<_> = seen.iter().map(|file| file.name.clone()).collect();
    let expected: Vec<_> = (0..rows).map(|i| format!("f{i:05}.jpg")).collect();
    assert_eq!(names, expected);

    let totals = queue.totals(&db).await.unwrap();
    assert_eq!(totals.files, rows as u64);
    assert_eq!(totals.bytes, (rows as u64 - 1) * rows as u64 / 2);
}

#[tokio::test]
async fn a_row_comes_back_as_it_was_stored() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    let big = i64::MAX / 8;
    let stored = [
        ("", "root.jpg", FileKind::Image, 0, 0, None),
        (
            "a",
            "clip.mov",
            FileKind::Video,
            big,
            4_102_444_800,
            Some(-86_400),
        ),
        ("a/b", "song.flac", FileKind::Audio, 123, i64::MAX, Some(0)),
        ("a/b", "song.xmp", FileKind::Sidecar, 1, 1, Some(i64::MIN)),
        ("a/b/c", "other", FileKind::Other, big, 1_700_000_000, None),
    ];
    let mut tx = db.begin_write().await.unwrap();
    for (dir, name, kind, size, mtime, btime) in stored {
        sqlx::query(
            "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, kind, size, \
                                mtime, btime) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(volume)
        .bind(dir)
        .bind(name)
        .bind(name)
        .bind(name)
        .bind(file_kind_to_db(kind))
        .bind(size)
        .bind(mtime)
        .bind(btime)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();

    let scope = StageScope::new(&ScanRequest::default(), &[volume]);
    for column in COLUMNS {
        let queue = PendingQueue::new(column, scope.clone());
        let got = queue.page(&db, None, 10).await.unwrap();
        let got: Vec<_> = got
            .iter()
            .map(|file| {
                (
                    file.dir_path.as_str(),
                    file.name.as_str(),
                    file.kind,
                    file.size,
                    file.mtime,
                    file.btime,
                )
            })
            .collect();
        assert_eq!(got, stored, "{column:?}");
        assert_eq!(
            queue.totals(&db).await.unwrap(),
            QueueTotals {
                files: 5,
                bytes: 2 * big as u64 + 124
            }
        );
    }
}

#[tokio::test]
async fn a_kind_that_no_stage_knows_is_an_error_not_a_default() {
    let db = Db::open_temp().await.unwrap();
    let row = sqlx::query(
        "SELECT 1 AS id, 1 AS volume_id, '' AS dir_path, 'x' AS name, 'hologram' AS kind, \
                1 AS size, 1 AS mtime, NULL AS btime",
    )
    .fetch_one(db.read())
    .await
    .unwrap();
    let err = pending_from_row(&row).unwrap_err();
    assert!(
        matches!(&err, sqlx::Error::ColumnDecode { index, .. } if index == "kind"),
        "{err:?}"
    );
}

/// Directories that a careless query would misread: quotes, SQL, `LIKE` wildcards and neighbours
/// of them, the placeholder syntaxes of SQLite, control characters, comment markers.
const AWKWARD_DIRS: &[&str] = &[
    "it's",
    "say \"hi\"",
    "x'; DROP TABLE files; --",
    "50%",
    "50%/_",
    "500",
    "5_0",
    "5x0",
    "100%",
    "1000",
    "back\\slash",
    "what?",
    ":name",
    "$1",
    "@p",
    "?1",
    "tab\there",
    "new\nline",
    " lead",
    "trail ",
    "star*",
    "[br]",
    "{cur}",
    "a;b,c",
    "--",
    "x/*c*/y",
    "\u{202e}rtl",
    "...",
    "~",
    "%",
    "_",
];

const AWKWARD_NAMES: &[&str] = &[
    "it's.jpg",
    "\"q\".jpg",
    "%.jpg",
    "_.jpg",
    "a\\b.jpg",
    "?.jpg",
    ":name.jpg",
    "$1.jpg",
    "'; DROP TABLE files; --.jpg",
    "tab\t.jpg",
    "new\nline.jpg",
    " .jpg",
    "'",
];

#[tokio::test]
async fn awkward_paths_are_data_everywhere() {
    let lib = library_of(AWKWARD_DIRS, AWKWARD_NAMES).await;
    let [a, b, _] = lib.volumes;
    let tables = |db: &Db| {
        let pool = db.write().clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM files")
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    let mut requests: Vec<(String, ScanRequest)> = Vec::new();
    for dir in AWKWARD_DIRS {
        requests.push((format!("subtree {dir:?}"), request(vec![subtree(a, dir)])));
        requests.push((
            format!("directory {dir:?}"),
            request(vec![directory(a, dir)]),
        ));
    }
    requests.push((
        "every one as a subtree".to_owned(),
        request(AWKWARD_DIRS.iter().map(|dir| subtree(a, dir)).collect()),
    ));
    requests.push((
        "every one as a directory, on two volumes".to_owned(),
        request(
            AWKWARD_DIRS
                .iter()
                .flat_map(|dir| [directory(b, dir), directory(a, dir)])
                .collect(),
        ),
    ));

    let cursors: Vec<_> = cursors(&lib).into_iter().step_by(11).collect();
    for (label, req) in &requests {
        for column in [PipelineColumn::Meta, PipelineColumn::Thumb] {
            let label = format!("{label}, {column:?}");
            let queue = lib.queue(column, req);
            let want = lib.expected(column, req, &|_| true, None);
            let got = queue.page(&lib.db, None, MAX_PAGE).await.unwrap();
            assert_rows(&label, &got, &want);
            assert_eq!(queue.totals(&lib.db).await.unwrap(), totals_of(&want));
            let pages = flatten(walk(&queue, &lib.db, 5).await);
            assert_rows(&format!("{label}, 5 at a time"), &pages, &want);
        }
    }
    let (label, req) = requests.last().unwrap();
    let queue = lib.queue(PipelineColumn::Meta, req);
    let pages = page_after_each(&queue, &lib.db, &cursors).await;
    for (cursor, got) in cursors.iter().zip(&pages) {
        let want = lib.expected(PipelineColumn::Meta, req, &|_| true, cursor.as_ref());
        assert_rows(&format!("{label}, after {cursor:?}"), got, &want);
    }

    assert_eq!(
        tables(&lib.db).await,
        (3 * AWKWARD_DIRS.len() * AWKWARD_NAMES.len()) as i64,
        "nothing was executed"
    );
}

#[tokio::test]
async fn a_wildcard_in_a_directory_name_is_not_a_pattern() {
    let lib = library_of(AWKWARD_DIRS, AWKWARD_NAMES).await;
    let a = lib.volumes[0];
    let dirs_of = |req: ScanRequest| {
        let queue = lib.queue(PipelineColumn::Thumb, &req);
        let db = &lib.db;
        async move {
            let mut dirs: Vec<String> = queue
                .page(db, None, MAX_PAGE)
                .await
                .unwrap()
                .into_iter()
                .map(|file| file.dir_path)
                .collect();
            dirs.dedup();
            dirs
        }
    };
    assert_eq!(dirs_of(request(vec![subtree(a, "100%")])).await, ["100%"]);
    assert_eq!(
        dirs_of(request(vec![subtree(a, "50%")])).await,
        ["50%", "50%/_"]
    );
    assert_eq!(dirs_of(request(vec![subtree(a, "5_0")])).await, ["5_0"]);
    assert_eq!(dirs_of(request(vec![subtree(a, "%")])).await, ["%"]);
    assert_eq!(dirs_of(request(vec![subtree(a, "_")])).await, ["_"]);
}

/// 4200 files in 450 directories of volume 1: `d000` to `d149` hold 20 files each, and next to every
/// one of them lie `d###/sub` and the look-alike `d###.x` with 4 each. A fifth of them are done.
const WIDE: &str = "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?), \
     src(d, f, i) AS ( \
         SELECT printf('d%03d', i / 20), printf('f%02d.jpg', i % 20), i FROM n \
         UNION ALL \
         SELECT printf('d%03d/sub', i / 4), printf('s%d.jpg', i % 4), i FROM n WHERE i < 600 \
         UNION ALL \
         SELECT printf('d%03d.x', i / 4), printf('x%d.jpg', i % 4), i FROM n WHERE i < 600) \
     INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, kind, size, mtime, \
                        meta_state) \
     SELECT ?, d, f, f, f, 'image', i, 1, CASE WHEN i % 5 = 0 THEN 1 ELSE 0 END FROM src";

async fn wide_library() -> Library {
    let db = Db::open_temp().await.unwrap();
    let mut volumes = [0; 3];
    for (slot, name) in volumes.iter_mut().zip(["photos", "scans", "archive"]) {
        *slot = insert_volume(&db, name).await;
    }
    seed(&db, WIDE, volumes[0], 3000).await;
    let model = load_model(&db).await;
    assert_eq!(model.len(), 4200);
    Library { db, volumes, model }
}

#[tokio::test]
async fn any_number_of_scopes_reads_what_the_scopes_hold() {
    let lib = wide_library().await;
    let a = lib.volumes[0];
    let cursors: Vec<_> = lib
        .model
        .iter()
        .step_by(301)
        .map(|row| Some(PendingKey::from(&row.file)))
        .collect();
    // Up to MAX_SQL_SCOPES the scopes are SQL terms, above it they are filtered in memory.
    for count in [1, 2, 3, 63, 64, 65, 66, 70, 75, 150] {
        for reversed in [false, true] {
            let mut targets: Vec<ScanTarget> = (0..count)
                .map(|k| ScanTarget {
                    volume_id: a,
                    dir: rel(&format!("d{:03}", k * 150 / count)),
                    recursive: k % 2 == 0,
                })
                .collect();
            if reversed {
                targets.reverse();
            }
            let req = request(targets);
            let scope = StageScope::new(&req, &lib.online());
            assert_eq!(scope.plans.len(), 1);
            assert_eq!(scope.plans[0].scopes.len(), count);

            for column in [PipelineColumn::Meta, PipelineColumn::Thumb] {
                let label = format!("{count} scopes, reversed: {reversed}, {column:?}");
                let queue = PendingQueue::new(column, scope.clone());
                let want = lib.expected(column, &req, &|_| true, None);
                assert!(want.len() >= 16 * count, "{label}");
                assert!(
                    want.iter().all(|file| !file.dir_path.ends_with(".x")),
                    "{label}: a look-alike is not in a subtree"
                );

                let got = queue.page(&lib.db, None, MAX_PAGE).await.unwrap();
                assert_rows(&label, &got, &want[..want.len().min(MAX_PAGE)]);
                assert_eq!(queue.totals(&lib.db).await.unwrap(), totals_of(&want));
                // The limits cut the pass at different rows of the hull, so the chunks of the
                // filtered read end at different rows too.
                for limit in [97, 1999, MAX_PAGE] {
                    let pages = flatten(walk(&queue, &lib.db, limit).await);
                    assert_rows(&format!("{label}, {limit} at a time"), &pages, &want);
                }
                let pages = page_after_each(&queue, &lib.db, &cursors).await;
                for (cursor, got) in cursors.iter().zip(&pages) {
                    let want = lib.expected(column, &req, &|_| true, cursor.as_ref());
                    let want = &want[..want.len().min(MAX_PAGE)];
                    assert_rows(&format!("{label}, after {cursor:?}"), got, want);
                }
            }
        }
    }
}

#[tokio::test]
async fn scopes_on_several_volumes_share_one_pass() {
    let lib = wide_library().await;
    let [a, b, _] = lib.volumes;
    seed(&lib.db, SEQUENCE, b, 300).await;
    let mut lib = lib;
    lib.model = load_model(&lib.db).await;

    // More scopes than the SQL takes on one volume, a few on the other.
    let mut targets: Vec<ScanTarget> = (0..80)
        .map(|k| directory(a, &format!("d{:03}", k * 2)))
        .collect();
    targets.extend([subtree(b, "d001"), directory(b, "d003"), subtree(b, "d005")]);
    let req = request(targets);
    let queue = lib.queue(PipelineColumn::Meta, &req);
    let want = lib.expected(PipelineColumn::Meta, &req, &|_| true, None);
    assert!(want.iter().any(|file| file.volume_id == a));
    assert!(want.iter().any(|file| file.volume_id == b));
    for limit in [1, 50, 333] {
        let pages = flatten(walk(&queue, &lib.db, limit.min(60)).await);
        assert_rows(&format!("{limit}"), &pages, &want);
    }
    assert_eq!(queue.totals(&lib.db).await.unwrap(), totals_of(&want));
}

fn hull(scopes: &[(&str, bool)]) -> Hull {
    Hull::of(&VolumePlan {
        volume_id: 1,
        scopes: scopes
            .iter()
            .map(|&(dir, recursive)| scope(dir, recursive))
            .collect(),
    })
}

fn bounds(hull: &Hull, cursor: Option<(&str, &str)>) -> String {
    let mut qb = QueryBuilder::<Sqlite>::new("");
    let key = cursor.map(|(dir, name)| PendingKey {
        volume_id: 1,
        dir_path: dir.to_owned(),
        name: name.to_owned(),
    });
    hull.push_bounds(&mut qb, key.as_ref());
    text(&qb)
}

fn text(qb: &QueryBuilder<Sqlite>) -> String {
    qb.sql().as_str().to_owned()
}

/// Is `dir` inside the hull, by its definition?
fn in_hull(hull: &Hull, dir: &str) -> bool {
    hull.lower.as_deref().is_none_or(|lower| dir >= lower)
        && hull
            .upper
            .as_ref()
            .is_none_or(|(upper, inclusive)| dir < upper.as_str() || (*inclusive && dir == upper))
}

#[test]
fn the_hull_ends_where_the_last_scope_ends() {
    let owned = |dir: &str, inclusive| Some((dir.to_owned(), inclusive));
    assert_eq!(hull(&[("", true)]), Hull::default(), "a whole volume");
    assert_eq!(
        hull(&[("", true), ("a", false)]),
        Hull::default(),
        "a scope that a whole volume covers"
    );
    assert_eq!(
        hull(&[("a", true)]),
        Hull {
            lower: Some("a".to_owned()),
            upper: owned("a0", false)
        },
        "'0' follows '/' in the byte order, so a0 is the first path after a's subtree"
    );
    assert_eq!(
        hull(&[("a", false)]),
        Hull {
            lower: Some("a".to_owned()),
            upper: owned("a", true)
        },
        "a directory alone ends at itself"
    );
    assert_eq!(
        hull(&[("", false)]),
        Hull {
            lower: None,
            upper: owned("", true)
        },
        "the root alone"
    );
    assert_eq!(
        hull(&[("", false), ("b", true)]),
        Hull {
            lower: None,
            upper: owned("b0", false)
        }
    );
    assert_eq!(
        hull(&[("m", false), ("c", true), ("x/y", false)]),
        Hull {
            lower: Some("c".to_owned()),
            upper: owned("x/y", true)
        },
        "the smallest start and the largest end, whatever the order"
    );
    assert_eq!(
        hull(&[("a", false), ("a0", true)]).upper,
        owned("a00", false),
        "a larger bound wins"
    );
    assert_eq!(
        hull(&[("b0", false), ("b", true)]).upper,
        owned("b0", true),
        "at the same bound the inclusive one reaches further"
    );
}

#[test]
fn the_hull_holds_every_directory_of_the_scopes_and_not_the_ones_far_from_them() {
    let extra = [
        "no/such/dir",
        "2019/Rome/day 1/x/y",
        "a/b/c/d",
        "zzz",
        "\u{10ffff}",
        "\u{10ffff}/a",
    ];
    for (label, req) in requests([1, 2, 3]) {
        for plan in plan_targets(&req.targets, &[1, 2, 3]) {
            let hull = Hull::of(&plan);
            let matcher = ScopeMatcher::new(&plan.scopes);
            for dir in DIRS.iter().chain(&extra) {
                if matcher.contains(dir) {
                    assert!(
                        in_hull(&hull, dir),
                        "{label}: {dir:?} is in scope but outside {hull:?}"
                    );
                }
            }
        }
    }

    let one = hull(&[("2019", true)]);
    for dir in ["2019", "2019/Rome", "2019.old", "2019 b", "2019!"] {
        assert!(in_hull(&one, dir), "{dir}");
    }
    for dir in [
        "", "201", "2018", "20190", "2019_x", "2019~", "2020", "a", "z",
    ] {
        assert!(!in_hull(&one, dir), "{dir}");
    }
}

#[test]
fn the_cursor_is_the_lower_bound_when_it_lies_inside_the_hull() {
    let h = hull(&[("b", true), ("d", false)]);
    assert_eq!(h.upper, Some(("d".to_owned(), true)));
    assert_eq!(bounds(&h, None), " AND f.dir_path >= ? AND f.dir_path <= ?");
    assert_eq!(
        bounds(&h, Some(("c", "x.jpg"))),
        " AND (f.dir_path, f.name) > (?, ?) AND f.dir_path <= ?",
        "one lower bound, the cursor, so that the planner has no other to choose"
    );
    assert_eq!(
        bounds(&h, Some(("b", ""))),
        " AND (f.dir_path, f.name) > (?, ?) AND f.dir_path <= ?",
        "a cursor at the start of the hull"
    );
    assert_eq!(
        bounds(&h, Some(("a", "z.jpg"))),
        " AND f.dir_path >= ? AND f.dir_path <= ?",
        "a cursor before the hull constrains nothing"
    );

    let subtree_only = hull(&[("b", true)]);
    assert_eq!(
        bounds(&subtree_only, Some(("b/x", "a"))),
        " AND (f.dir_path, f.name) > (?, ?) AND f.dir_path < ?"
    );

    let whole = Hull::default();
    assert_eq!(bounds(&whole, None), "");
    assert_eq!(
        bounds(&whole, Some(("a", "b"))),
        " AND (f.dir_path, f.name) > (?, ?)"
    );
}

#[test]
fn the_matcher_is_the_in_memory_twin_of_the_scope_rule() {
    let extra = ["no/such/dir", "2019/Rome/day 1/x/y", "a/b/c/d", "zzz", "a/"];
    for (label, req) in requests([1, 2, 3]) {
        for plan in plan_targets(&req.targets, &[1, 2, 3]) {
            let matcher = ScopeMatcher::new(&plan.scopes);
            for dir in DIRS.iter().chain(&extra) {
                let want = req.targets.is_empty()
                    || req
                        .targets
                        .iter()
                        .any(|target| covered_by(target, plan.volume_id, dir));
                assert_eq!(
                    matcher.contains(dir),
                    want,
                    "{label}: {dir:?} on volume {}",
                    plan.volume_id
                );
            }
        }
    }
}

#[test]
fn the_matcher_takes_a_directory_alone_or_a_subtree() {
    let matcher =
        ScopeMatcher::new(&[scope("a", false), scope("b/c", true), scope("\u{e9}", true)]);
    for dir in ["a", "b/c", "b/c/d", "b/c/d/e", "\u{e9}", "\u{e9}/x"] {
        assert!(matcher.contains(dir), "{dir}");
    }
    for dir in [
        "",
        "a/b",
        "b",
        "b/cc",
        "b/c.d",
        "b/d",
        "c",
        "\u{e9}t\u{e9}",
        "e\u{301}",
    ] {
        assert!(!matcher.contains(dir), "{dir}");
    }
    assert!(ScopeMatcher::new(&[scope("", true)]).contains("any/where"));
    assert!(ScopeMatcher::new(&[scope("", false)]).contains(""));
    assert!(!ScopeMatcher::new(&[scope("", false)]).contains("a"));
    assert!(!ScopeMatcher::new(&[]).contains(""));
}

#[test]
fn the_pending_code_is_a_literal_and_the_scope_is_written_for_the_planner() {
    let plan = VolumePlan {
        volume_id: 1,
        scopes: vec![scope("a", true), scope("c", false)],
    };
    for column in COLUMNS {
        let name = column.column();
        let queue = PendingQueue::new(
            column,
            StageScope {
                plans: vec![plan.clone()],
            },
        )
        .with_condition("f.size > 0")
        .with_condition("f.id % 2 = 0 OR f.size = 7");
        let others: Vec<_> = COLUMNS
            .iter()
            .map(|other| other.column())
            .filter(|other| *other != name)
            .collect();

        let page = text(&queue.page_statement(&plan, None, 10, ScopeFilter::Sql));
        let page_hull = text(&queue.page_statement(&plan, None, 10, ScopeFilter::HullOnly));
        let totals = text(&queue.totals_statement(&plan));
        for sql in [&page, &page_hull, &totals] {
            assert!(sql.contains(&format!(" AND f.{name} = 0 ")), "{sql}");
            assert!(others.iter().all(|other| !sql.contains(other)), "{sql}");
            assert!(
                sql.contains(" AND (f.size > 0) AND (f.id % 2 = 0 OR f.size = 7)"),
                "conditions are parenthesised: {sql}"
            );
            assert!(sql.contains("WHERE f.volume_id = ?"), "{sql}");
        }

        // The scope terms of a page are residual filters: what remains of the filter once the
        // hull's bounds and the order are taken out must not name `f.dir_path` without the `+`.
        let plain = |sql: &str| {
            let filter = sql.split_once(" WHERE ").map_or(sql, |(_, filter)| filter);
            filter
                .replace("+f.dir_path", "")
                .replace(" AND f.dir_path >= ?", "")
                .replace(" AND f.dir_path <= ?", "")
                .replace(" AND f.dir_path < ?", "")
                .replace("ORDER BY f.dir_path", "")
        };
        assert!(page.contains("OR +f.dir_path = ?)"), "{page}");
        assert!(page.contains("+f.dir_path >= ?"), "{page}");
        assert!(!plain(&page).contains("f.dir_path"), "{page}");
        assert!(
            page.contains(" AND f.dir_path >= ? AND f.dir_path <= ? ORDER BY"),
            "the range of the hull follows the scope: {page}"
        );
        assert!(
            !page_hull.contains("+f.dir_path") && !plain(&page_hull).contains("f.dir_path"),
            "the hull statement does not filter by scope itself: {page_hull}"
        );
        // A count has no order to keep, so its terms are plain and the planner may search them.
        assert!(!totals.contains("+f.dir_path"), "{totals}");
        assert!(totals.contains("f.dir_path = ?"), "{totals}");
        assert!(!totals.contains("ORDER BY"), "{totals}");
        for sql in [&page, &page_hull] {
            assert!(
                sql.contains(" ORDER BY f.dir_path, f.name LIMIT ?"),
                "{sql}"
            );
        }
    }

    let whole = VolumePlan {
        volume_id: 1,
        scopes: vec![scope("", true)],
    };
    let queue = PendingQueue::new(
        PipelineColumn::Meta,
        StageScope {
            plans: vec![whole.clone()],
        },
    );
    let sql = text(&queue.page_statement(&whole, None, 10, ScopeFilter::Sql));
    assert!(
        !sql.contains("dir_path >=") && !sql.contains(" OR "),
        "a whole volume has no scope term: {sql}"
    );
}

#[tokio::test]
async fn the_indexer_scopes_a_run_to_the_volumes_that_are_online() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let config = Config::for_tests(temp.dir());
    fs::create_dir_all(&config.volumes_dir).unwrap();
    for name in ["photos", "scans", "archive"] {
        fs::create_dir(config.volumes_dir.join(name)).unwrap();
    }
    let settings = SettingsService::load(db.clone(), &config).await.unwrap();
    let volumes = VolumeRegistry::load(db.clone(), config.volumes_dir.clone())
        .await
        .unwrap();
    let indexer = Indexer::new(db.clone(), settings, volumes);
    let known = indexer.volumes().refresh().await.unwrap();
    let id = |name: &str| known.iter().find(|volume| volume.name == name).unwrap().id;
    let (photos, scans, archive) = (id("photos"), id("scans"), id("archive"));
    for volume in [photos, scans, archive] {
        insert_file(&db, volume, "a", "x.jpg").await;
    }

    let mut all = vec![photos, scans, archive];
    all.sort_unstable();
    let scope = indexer.stage_scope(&ScanRequest::default());
    assert_eq!(scope.volume_ids(), all);
    let queue = PendingQueue::new(PipelineColumn::Meta, scope);
    assert_eq!(queue.totals(&db).await.unwrap().files, 3);

    // A volume that goes offline leaves every scope, and its files stay in the table.
    fs::remove_dir(config.volumes_dir.join("scans")).unwrap();
    indexer.volumes().refresh().await.unwrap();
    let scope = indexer.stage_scope(&ScanRequest::default());
    let mut online = vec![photos, archive];
    online.sort_unstable();
    assert_eq!(scope.volume_ids(), online);
    let queue = PendingQueue::new(PipelineColumn::Meta, scope);
    let page = queue.page(&db, None, 10).await.unwrap();
    assert_eq!(page.len(), 2);
    assert!(page.iter().all(|file| file.volume_id != scans));

    let only_scans = ScanRequest {
        targets: vec![ScanTarget::volume(scans)],
        ..ScanRequest::default()
    };
    assert!(indexer.stage_scope(&only_scans).is_empty());
    let mixed = ScanRequest {
        targets: vec![ScanTarget::volume(scans), subtree(photos, "a")],
        ..ScanRequest::default()
    };
    assert_eq!(indexer.stage_scope(&mixed).volume_ids(), [photos]);

    let in_table: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
        .fetch_one(db.read())
        .await
        .unwrap();
    assert_eq!(in_table, 3);
}
