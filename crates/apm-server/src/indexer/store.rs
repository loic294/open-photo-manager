//! The SQL of the scanner. Everything a scan reads from or writes to `files`, `dirs` and `volumes`
//! lives here; `scanner.rs` decides WHAT to write, this module writes it.
//!
//! Reads go through the reader pool ([`Db::read`]) before the write transaction of a group starts.
//! The `write_*` and `delete_*` functions take the connection of a transaction that the caller
//! opened with [`Db::begin_write`]; nothing here asks for the writer while a transaction is open.
//!
//! # Rows written by a scan
//! * A new or changed file gets `kind`, `size`, `mtime`, `btime`, `hash = NULL`, the three pipeline
//!   states pending (all `not applicable` for a sidecar) and `seen_gen` = the scan's generation. The
//!   id of a changed file never changes. Columns the later stages fill (`capture_*`, `camera_id`,
//!   `width`, ...) stay as they are until the file is extracted again.
//! * Unchanged files are not written at all.
//! * A directory row is written when it is new or its modification time changed.
//! * A row is only deleted when its `seen_gen` is older than the scan's generation: a row written
//!   during the scan (by this scan, or by a file operation that ran meanwhile and stamped the
//!   volume's current generation) is never removed by it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use apm_core::dto::FileKind;
use apm_core::names::{extension_lower, normalize_name, stem_norm};
use apm_core::paths::{FileId, RelPath, VolumeId};
use apm_core::sidecar::SidecarMatcher;
use sqlx::{QueryBuilder, Row as _, Sqlite, SqliteConnection};

use super::diff::StoredFile;
use super::links;
use super::walk::DirListing;
use crate::db::models::PipelineState;
use crate::db::subtree::Subtree;
use crate::db::{Db, now_secs};

/// Rows per multi-row `INSERT` (at most 14 binds each).
const INSERT_CHUNK: usize = 200;
/// Values per `IN (...)` list.
const IN_CHUNK: usize = 500;
/// Rows removed per statement and per transaction.
pub(crate) const DELETE_CHUNK: usize = 1000;

/// The generation of one scan of a volume.
pub(crate) struct Generation {
    /// The new `volumes.scan_gen`.
    pub value: i64,
    /// No scan of the whole volume has completed before.
    pub first_scan: bool,
}

/// Starts a scan: bumps the volume's `scan_gen`. `None` when the volume does not exist.
pub(crate) async fn begin_generation(
    db: &Db,
    volume_id: VolumeId,
) -> Result<Option<Generation>, sqlx::Error> {
    let row = sqlx::query(
        "UPDATE volumes SET scan_gen = scan_gen + 1 WHERE id = ? RETURNING scan_gen, last_scan_at",
    )
    .bind(volume_id)
    .fetch_optional(db.write())
    .await?;
    row.map(|row| {
        let last_scan_at: Option<i64> = row.try_get("last_scan_at")?;
        Ok(Generation {
            value: row.try_get("scan_gen")?,
            first_scan: last_scan_at.is_none(),
        })
    })
    .transpose()
}

/// How a scan changes `volumes.held_removals`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldUpdate {
    Keep,
    /// The scan saw the whole volume: this is the count, 0 when nothing is held.
    Set(u64),
    /// The scan saw a part: the count can only grow.
    Raise(u64),
}

/// Ends a scan: stamps `last_scan_at` (only for a scan of the whole volume) and records the held
/// removals.
pub(crate) async fn finish_volume(
    db: &Db,
    volume_id: VolumeId,
    whole_volume: bool,
    held: HeldUpdate,
) -> Result<(), sqlx::Error> {
    let mut qb = QueryBuilder::<Sqlite>::new("UPDATE volumes SET ");
    let mut set = qb.separated(", ");
    let mut any = false;
    if whole_volume {
        set.push("last_scan_at = ")
            .push_bind_unseparated(now_secs());
        any = true;
    }
    match held {
        HeldUpdate::Keep => {}
        HeldUpdate::Set(count) => {
            set.push("held_removals = ")
                .push_bind_unseparated(to_i64(count));
            any = true;
        }
        HeldUpdate::Raise(count) => {
            set.push("held_removals = MAX(held_removals, ")
                .push_bind_unseparated(to_i64(count))
                .push_unseparated(")");
            any = true;
        }
    }
    if !any {
        return Ok(());
    }
    qb.push(" WHERE id = ").push_bind(volume_id);
    qb.build().execute(db.write()).await?;
    Ok(())
}

/// Volumes with removals waiting for the user's confirmation: `(id, name, files)`.
pub(crate) async fn held_removals(db: &Db) -> Result<Vec<(VolumeId, String, u64)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, name, held_removals FROM volumes WHERE held_removals > 0 ORDER BY name",
    )
    .fetch_all(db.read())
    .await?;
    rows.iter()
        .map(|row| {
            let files: i64 = row.try_get("held_removals")?;
            Ok((
                row.try_get("id")?,
                row.try_get("name")?,
                u64::try_from(files).unwrap_or(0),
            ))
        })
        .collect()
}

/// Number of file rows of the volume.
pub(crate) async fn count_files(db: &Db, volume_id: VolumeId) -> Result<u64, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE volume_id = ?")
        .bind(volume_id)
        .fetch_one(db.read())
        .await?;
    Ok(u64::try_from(count).unwrap_or(0))
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn push_list(qb: &mut QueryBuilder<Sqlite>, values: &[&str]) {
    let mut list = qb.separated(", ");
    for value in values {
        list.push_bind((*value).to_owned());
    }
}

/// What the database holds for the directories of one batch.
#[derive(Default)]
pub(crate) struct Stored {
    /// File rows by `dir_path`.
    pub files: HashMap<String, Vec<StoredFile>>,
    /// Names of the sub-directories that have a row, by parent path.
    pub children: HashMap<String, Vec<String>>,
    /// The directory rows of the batch: path to modification time.
    pub dirs: HashMap<String, Option<i64>>,
}

/// Loads the stored rows a batch is compared with: the files and the sub-directories of the
/// `healthy` directories and the rows of every directory in `listed`.
pub(crate) async fn load_stored(
    db: &Db,
    volume_id: VolumeId,
    listed: &[&str],
    healthy: &[&str],
) -> Result<Stored, sqlx::Error> {
    let mut stored = Stored::default();
    for chunk in healthy.chunks(IN_CHUNK) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT id, dir_path, name, kind, size, mtime FROM files WHERE volume_id = ",
        );
        qb.push_bind(volume_id).push(" AND dir_path IN (");
        push_list(&mut qb, chunk);
        qb.push(")");
        for row in qb.build().fetch_all(db.read()).await? {
            let dir: String = row.try_get("dir_path")?;
            let kind: &str = row.try_get("kind")?;
            let file = StoredFile {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                kind: kind
                    .parse::<FileKind>()
                    .map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
                size: row.try_get("size")?,
                mtime: row.try_get("mtime")?,
            };
            stored.files.entry(dir).or_default().push(file);
        }

        let mut qb =
            QueryBuilder::<Sqlite>::new("SELECT parent_path, name FROM dirs WHERE volume_id = ");
        qb.push_bind(volume_id).push(" AND parent_path IN (");
        push_list(&mut qb, chunk);
        qb.push(")");
        for row in qb.build().fetch_all(db.read()).await? {
            let parent: String = row.try_get("parent_path")?;
            stored
                .children
                .entry(parent)
                .or_default()
                .push(row.try_get("name")?);
        }
    }
    stored.dirs = load_dir_rows(db, volume_id, listed).await?;
    Ok(stored)
}

/// The rows of the directories `paths`: path to modification time. Directories without a row are
/// missing from the map.
pub(crate) async fn load_dir_rows(
    db: &Db,
    volume_id: VolumeId,
    paths: &[&str],
) -> Result<HashMap<String, Option<i64>>, sqlx::Error> {
    let mut dirs = HashMap::new();
    for chunk in paths.chunks(IN_CHUNK) {
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT path, mtime FROM dirs WHERE volume_id = ");
        qb.push_bind(volume_id).push(" AND path IN (");
        push_list(&mut qb, chunk);
        qb.push(")");
        for row in qb.build().fetch_all(db.read()).await? {
            dirs.insert(row.try_get("path")?, row.try_get("mtime")?);
        }
    }
    Ok(dirs)
}

/// Everything one directory contributes to a write transaction.
pub(crate) struct DirUnit {
    pub listing: DirListing,
    /// One entry per listed file; the new files get their id when the group is written.
    pub ids: Vec<Option<FileId>>,
    /// Indexes into `listing.files` of the files to insert or update.
    pub writes: Vec<usize>,
    /// Recompute the sidecar links of the directory.
    pub relink: bool,
    /// Stored sidecars that changed: their owners are extracted again.
    pub changed_sidecars: Vec<FileId>,
    /// The directory's own row: `Some(mtime)` inserts it or updates its modification time.
    pub set_dir: Option<Option<i64>>,
    /// Directories that get a row without a listing of their own (sub-directories of a
    /// non-recursive scan, an unreadable directory).
    pub ensure_dirs: Vec<String>,
    /// Directories whose visible listing this unit changes, for the change report.
    pub touched: Vec<RelPath>,
}

impl DirUnit {
    /// A unit that writes nothing yet.
    pub fn new(listing: DirListing) -> Self {
        Self {
            listing,
            ids: Vec::new(),
            writes: Vec::new(),
            relink: false,
            changed_sidecars: Vec::new(),
            set_dir: None,
            ensure_dirs: Vec::new(),
            touched: Vec::new(),
        }
    }

    /// Rows this unit writes, to cut the transactions at about the same size.
    pub fn weight(&self) -> usize {
        self.writes.len() + usize::from(self.set_dir.is_some()) + self.ensure_dirs.len()
    }

    /// Nothing to write, nothing to report.
    pub fn is_idle(&self) -> bool {
        self.weight() == 0 && !self.relink && self.changed_sidecars.is_empty()
    }
}

/// Writes the units of one transaction: directory rows, file rows, then the sidecar links (which
/// need the ids of the new files) and the extraction reset of the owners of changed sidecars.
pub(crate) async fn write_group(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    generation: i64,
    matcher: &SidecarMatcher,
    units: &mut [DirUnit],
) -> Result<(), sqlx::Error> {
    let had_stored: Vec<bool> = units
        .iter()
        .map(|unit| unit.ids.iter().any(Option::is_some))
        .collect();
    write_dirs(conn, volume_id, generation, units).await?;
    write_files(conn, volume_id, generation, units).await?;
    write_links(conn, volume_id, matcher, units, &had_stored).await
}

/// `(parent_path, name)` of a directory path as stored in `dirs` (the root has no parent).
fn dir_parts(path: &str) -> (Option<&str>, &str) {
    match path.rsplit_once('/') {
        Some((parent, name)) => (Some(parent), name),
        None if path.is_empty() => (None, ""),
        None => (Some(""), path),
    }
}

async fn write_dirs(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    generation: i64,
    units: &[DirUnit],
) -> Result<(), sqlx::Error> {
    let mut set: BTreeMap<&str, Option<i64>> = BTreeMap::new();
    let mut ensure: BTreeSet<&str> = BTreeSet::new();
    for unit in units {
        if let Some(mtime) = unit.set_dir {
            set.insert(unit.listing.dir.as_str(), mtime);
        }
        ensure.extend(unit.ensure_dirs.iter().map(String::as_str));
    }
    ensure.retain(|path| !set.contains_key(path));

    let set: Vec<(&str, Option<i64>)> = set.into_iter().collect();
    let ensure: Vec<(&str, Option<i64>)> = ensure.into_iter().map(|path| (path, None)).collect();
    insert_dirs(conn, volume_id, generation, &set, true).await?;
    insert_dirs(conn, volume_id, generation, &ensure, false).await
}

async fn insert_dirs(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    generation: i64,
    rows: &[(&str, Option<i64>)],
    overwrite: bool,
) -> Result<(), sqlx::Error> {
    for chunk in rows.chunks(INSERT_CHUNK) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "INSERT INTO dirs (volume_id, path, parent_path, name, mtime, seen_gen) ",
        );
        qb.push_values(chunk, |mut row, (path, mtime)| {
            let (parent, name) = dir_parts(path);
            row.push_bind(volume_id)
                .push_bind(*path)
                .push_bind(parent)
                .push_bind(name)
                .push_bind(*mtime)
                .push_bind(generation);
        });
        qb.push(if overwrite {
            " ON CONFLICT (volume_id, path) DO UPDATE SET mtime = excluded.mtime, \
             seen_gen = excluded.seen_gen"
        } else {
            " ON CONFLICT (volume_id, path) DO NOTHING"
        });
        qb.build().execute(&mut *conn).await?;
    }
    Ok(())
}

async fn write_files(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    generation: i64,
    units: &mut [DirUnit],
) -> Result<(), sqlx::Error> {
    let work: Vec<(usize, usize)> = units
        .iter()
        .enumerate()
        .flat_map(|(unit, u)| u.writes.iter().map(move |&file| (unit, file)))
        .collect();
    for chunk in work.chunks(INSERT_CHUNK) {
        let assigned = upsert_files(conn, volume_id, generation, units, chunk).await?;
        for (unit, file, id) in assigned {
            units[unit].ids[file] = Some(id);
        }
    }
    Ok(())
}

/// Inserts or updates the files `chunk` (`(unit, file)` indexes) and returns their ids.
async fn upsert_files(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    generation: i64,
    units: &[DirUnit],
    chunk: &[(usize, usize)],
) -> Result<Vec<(usize, usize, FileId)>, sqlx::Error> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, \
         mtime, btime, meta_state, hash_state, thumb_state, seen_gen) ",
    );
    qb.push_values(chunk, |mut row, &(unit, file)| {
        let dir = &units[unit].listing.dir;
        let entry = &units[unit].listing.files[file];
        let state = if entry.kind == FileKind::Sidecar {
            PipelineState::NotApplicable
        } else {
            PipelineState::Pending
        }
        .code();
        row.push_bind(volume_id)
            .push_bind(dir.as_str())
            .push_bind(entry.name.as_str())
            .push_bind(normalize_name(&entry.name))
            .push_bind(stem_norm(&entry.name))
            .push_bind(extension_lower(&entry.name))
            .push_bind(entry.kind.as_str())
            .push_bind(entry.size)
            .push_bind(entry.mtime)
            .push_bind(entry.btime)
            .push_bind(state)
            .push_bind(state)
            .push_bind(state)
            .push_bind(generation);
    });
    qb.push(
        " ON CONFLICT (volume_id, dir_path, name) DO UPDATE SET kind = excluded.kind, \
         size = excluded.size, mtime = excluded.mtime, btime = excluded.btime, hash = NULL, \
         meta_state = excluded.meta_state, hash_state = excluded.hash_state, \
         thumb_state = excluded.thumb_state, seen_gen = excluded.seen_gen \
         RETURNING id, dir_path, name",
    );
    let rows = qb.build().fetch_all(&mut *conn).await?;

    let by_path: HashMap<(&str, &str), (usize, usize)> = chunk
        .iter()
        .map(|&(unit, file)| {
            let key = (
                units[unit].listing.dir.as_str(),
                units[unit].listing.files[file].name.as_str(),
            );
            (key, (unit, file))
        })
        .collect();
    let mut assigned = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: FileId = row.try_get("id")?;
        let dir: &str = row.try_get("dir_path")?;
        let name: &str = row.try_get("name")?;
        let Some(&(unit, file)) = by_path.get(&(dir, name)) else {
            return Err(sqlx::Error::Protocol(format!(
                "the upsert returned a row that was not written: {dir}/{name}"
            )));
        };
        assigned.push((unit, file, id));
    }
    Ok(assigned)
}

async fn write_links(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    matcher: &SidecarMatcher,
    units: &[DirUnit],
    had_stored: &[bool],
) -> Result<(), sqlx::Error> {
    let mut wanted = Vec::new();
    let mut present = HashSet::new();
    let mut stored_dirs: Vec<&str> = Vec::new();
    for (unit, had_stored) in units.iter().zip(had_stored) {
        if !unit.relink {
            continue;
        }
        let ids: Option<Vec<FileId>> = unit.ids.iter().copied().collect();
        let Some(ids) = ids else {
            return Err(sqlx::Error::Protocol(format!(
                "a file of {} has no id after the write",
                unit.listing.dir.as_str()
            )));
        };
        let names: Vec<&str> = unit
            .listing
            .files
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        wanted.extend(links::wanted_links(matcher, &names, &ids));
        present.extend(ids);
        if *had_stored {
            stored_dirs.push(unit.listing.dir.as_str());
        }
    }
    if !stored_dirs.is_empty() || !wanted.is_empty() {
        let stored = links::load_stored(conn, volume_id, &stored_dirs).await?;
        let delta = links::link_delta(&wanted, &stored, &present);
        links::apply_delta(conn, &delta).await?;
    }
    let changed: Vec<FileId> = units
        .iter()
        .flat_map(|unit| unit.changed_sidecars.iter().copied())
        .collect();
    links::reextract_owners_of(conn, &changed).await
}

/// Inserts the directory rows that are missing among `paths` (ancestors of a scanned directory).
pub(crate) async fn ensure_dirs(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    generation: i64,
    paths: &[&str],
) -> Result<(), sqlx::Error> {
    let rows: Vec<(&str, Option<i64>)> = paths.iter().map(|path| (*path, None)).collect();
    insert_dirs(conn, volume_id, generation, &rows, false).await
}

/// What a delete statement removed.
#[derive(Debug, Default)]
pub(crate) struct Removed {
    pub rows: u64,
    /// The directories of the removed file rows, or the paths of the removed directory rows.
    pub dirs: BTreeSet<String>,
}

/// Deletes the file rows `ids` that were not written during this scan, after putting the owners of
/// the sidecars among them back in the extraction queue.
pub(crate) async fn delete_files(
    conn: &mut SqliteConnection,
    generation: i64,
    ids: &[FileId],
) -> Result<Removed, sqlx::Error> {
    links::reextract_owners_of(conn, ids).await?;
    let mut removed = Removed::default();
    for chunk in ids.chunks(IN_CHUNK * 10) {
        let mut qb = QueryBuilder::<Sqlite>::new("DELETE FROM files WHERE seen_gen < ");
        qb.push_bind(generation).push(" AND id IN (");
        let mut list = qb.separated(", ");
        for id in chunk {
            list.push_bind(*id);
        }
        qb.push(") RETURNING dir_path");
        for row in qb.build().fetch_all(&mut *conn).await? {
            removed.rows += 1;
            removed.dirs.insert(row.try_get("dir_path")?);
        }
    }
    Ok(removed)
}

/// How many file rows lie in the subtree and were not written during this scan.
pub(crate) async fn count_subtree_files(
    db: &Db,
    volume_id: VolumeId,
    generation: i64,
    subtree: &Subtree,
) -> Result<u64, sqlx::Error> {
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM files WHERE volume_id = ");
    qb.push_bind(volume_id)
        .push(" AND seen_gen < ")
        .push_bind(generation)
        .push(" AND ");
    subtree.push_predicate(&mut qb, "dir_path");
    let count: i64 = qb.build_query_scalar().fetch_one(db.read()).await?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Deletes up to [`DELETE_CHUNK`] file rows of the subtree that were not written during this scan
/// (nothing is left to delete when `rows` is 0).
pub(crate) async fn delete_subtree_files(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    generation: i64,
    subtree: &Subtree,
) -> Result<Removed, sqlx::Error> {
    delete_subtree_rows(conn, "files", "dir_path", volume_id, generation, subtree).await
}

/// Deletes up to [`DELETE_CHUNK`] directory rows of the subtree that were not written during this
/// scan (nothing is left to delete when `rows` is 0).
pub(crate) async fn delete_subtree_dirs(
    conn: &mut SqliteConnection,
    volume_id: VolumeId,
    generation: i64,
    subtree: &Subtree,
) -> Result<Removed, sqlx::Error> {
    delete_subtree_rows(conn, "dirs", "path", volume_id, generation, subtree).await
}

/// `table` and `column` are literals of this module, never input. `column` is the path column,
/// which is also what the rows report back.
async fn delete_subtree_rows(
    conn: &mut SqliteConnection,
    table: &str,
    column: &str,
    volume_id: VolumeId,
    generation: i64,
    subtree: &Subtree,
) -> Result<Removed, sqlx::Error> {
    let mut qb = QueryBuilder::<Sqlite>::new("DELETE FROM ");
    qb.push(table)
        .push(" WHERE id IN (SELECT id FROM ")
        .push(table)
        .push(" WHERE volume_id = ")
        .push_bind(volume_id)
        .push(" AND seen_gen < ")
        .push_bind(generation)
        .push(" AND ");
    subtree.push_predicate(&mut qb, column);
    qb.push(" LIMIT ")
        .push_bind(DELETE_CHUNK as i64)
        .push(") RETURNING ")
        .push(column);
    let mut removed = Removed::default();
    for row in qb.build().fetch_all(&mut *conn).await? {
        removed.rows += 1;
        removed.dirs.insert(row.try_get(column)?);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use apm_core::paths::RelPath;

    use super::*;
    use crate::db::testing::{insert_dir, insert_file, insert_volume};
    use crate::indexer::walk::FileEntry;

    fn entry(name: &str, kind: FileKind, size: i64) -> FileEntry {
        FileEntry {
            name: name.to_owned(),
            kind,
            size,
            mtime: 100,
            btime: Some(90),
        }
    }

    fn unit(dir: &str, files: Vec<FileEntry>) -> DirUnit {
        let writes = (0..files.len()).collect();
        DirUnit {
            ids: vec![None; files.len()],
            writes,
            relink: true,
            changed_sidecars: Vec::new(),
            set_dir: Some(Some(5)),
            ensure_dirs: Vec::new(),
            touched: Vec::new(),
            listing: DirListing {
                dir: RelPath::new(dir).unwrap(),
                mtime: Some(5),
                files,
                subdirs: Vec::new(),
                skipped_invalid: 0,
                error: None,
            },
        }
    }

    async fn write(db: &Db, volume: VolumeId, generation: i64, units: &mut [DirUnit]) {
        let mut tx = db.begin_write().await.unwrap();
        write_group(
            &mut tx,
            volume,
            generation,
            &SidecarMatcher::default(),
            units,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    #[test]
    fn directory_rows_split_into_parent_and_name() {
        assert_eq!(dir_parts(""), (None, ""));
        assert_eq!(dir_parts("2024"), (Some(""), "2024"));
        assert_eq!(dir_parts("2024/Trip"), (Some("2024"), "Trip"));
        assert_eq!(dir_parts("a/b/c"), (Some("a/b"), "c"));
    }

    #[tokio::test]
    async fn the_generation_counts_up_and_knows_the_first_scan() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let first = begin_generation(&db, volume).await.unwrap().unwrap();
        assert_eq!((first.value, first.first_scan), (1, true));

        finish_volume(&db, volume, true, HeldUpdate::Set(0))
            .await
            .unwrap();
        let second = begin_generation(&db, volume).await.unwrap().unwrap();
        assert_eq!((second.value, second.first_scan), (2, false));
        assert!(begin_generation(&db, 999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn held_removals_are_set_raised_and_listed() {
        let db = Db::open_temp().await.unwrap();
        let a = insert_volume(&db, "alpha").await;
        let b = insert_volume(&db, "beta").await;
        assert!(held_removals(&db).await.unwrap().is_empty());

        finish_volume(&db, b, false, HeldUpdate::Set(70))
            .await
            .unwrap();
        finish_volume(&db, a, false, HeldUpdate::Raise(10))
            .await
            .unwrap();
        finish_volume(&db, a, false, HeldUpdate::Raise(4))
            .await
            .unwrap();
        finish_volume(&db, a, false, HeldUpdate::Keep)
            .await
            .unwrap();
        assert_eq!(
            held_removals(&db).await.unwrap(),
            [(a, "alpha".to_owned(), 10), (b, "beta".to_owned(), 70)]
        );
        let last_scan: Option<i64> =
            sqlx::query_scalar("SELECT last_scan_at FROM volumes WHERE id = ?")
                .bind(a)
                .fetch_one(db.read())
                .await
                .unwrap();
        assert_eq!(last_scan, None, "a partial scan does not stamp the volume");

        finish_volume(&db, b, true, HeldUpdate::Set(0))
            .await
            .unwrap();
        assert_eq!(held_removals(&db).await.unwrap().len(), 1);
        let last_scan: Option<i64> =
            sqlx::query_scalar("SELECT last_scan_at FROM volumes WHERE id = ?")
                .bind(b)
                .fetch_one(db.read())
                .await
                .unwrap();
        assert!(last_scan.is_some());
    }

    #[tokio::test]
    async fn new_files_get_ids_states_and_derived_columns() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let mut units = [unit(
            "2024/Trip",
            vec![
                entry("IMG_0001.CR3", FileKind::Image, 10),
                entry("IMG_0001.xmp", FileKind::Sidecar, 2),
                entry("Cafe\u{301}.JPG", FileKind::Image, 3),
            ],
        )];
        write(&db, volume, 7, &mut units).await;

        assert!(units[0].ids.iter().all(Option::is_some));
        let rows = sqlx::query(
            "SELECT name, name_norm, stem_norm, ext, kind, size, mtime, btime, meta_state, \
             hash_state, thumb_state, seen_gen, hash FROM files WHERE volume_id = ? ORDER BY name",
        )
        .bind(volume)
        .fetch_all(db.read())
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        let by_name = |name: &str| {
            rows.iter()
                .find(|row| row.get::<String, _>("name") == name)
                .unwrap()
        };

        let raw = by_name("IMG_0001.CR3");
        assert_eq!(raw.get::<String, _>("name_norm"), "img_0001.cr3");
        assert_eq!(raw.get::<String, _>("stem_norm"), "img_0001");
        assert_eq!(raw.get::<String, _>("ext"), "cr3");
        assert_eq!(raw.get::<String, _>("kind"), "image");
        assert_eq!(raw.get::<i64, _>("size"), 10);
        assert_eq!(raw.get::<Option<i64>, _>("btime"), Some(90));
        assert_eq!(raw.get::<i64, _>("seen_gen"), 7);
        assert_eq!(
            (
                raw.get::<i64, _>("meta_state"),
                raw.get::<i64, _>("hash_state"),
                raw.get::<i64, _>("thumb_state")
            ),
            (0, 0, 0)
        );
        assert!(raw.get::<Option<Vec<u8>>, _>("hash").is_none());

        let sidecar = by_name("IMG_0001.xmp");
        assert_eq!(
            (
                sidecar.get::<i64, _>("meta_state"),
                sidecar.get::<i64, _>("hash_state"),
                sidecar.get::<i64, _>("thumb_state")
            ),
            (3, 3, 3),
            "a sidecar has nothing to extract, hash or render"
        );

        let nfd = by_name("Cafe\u{301}.JPG");
        assert_eq!(
            nfd.get::<String, _>("name_norm"),
            "caf\u{e9}.jpg",
            "NFC and lowercase"
        );

        let dirs: Vec<(String, Option<String>, String, Option<i64>)> =
            sqlx::query_as("SELECT path, parent_path, name, mtime FROM dirs WHERE volume_id = ?")
                .bind(volume)
                .fetch_all(db.read())
                .await
                .unwrap();
        assert_eq!(
            dirs,
            [(
                "2024/Trip".to_owned(),
                Some("2024".to_owned()),
                "Trip".to_owned(),
                Some(5)
            )]
        );
    }

    #[tokio::test]
    async fn the_links_of_a_written_directory_follow_its_names() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let mut units = [unit(
            "",
            vec![
                entry("IMG_1.CR3", FileKind::Image, 1),
                entry("IMG_1.xmp", FileKind::Sidecar, 1),
                entry("lonely.xmp", FileKind::Sidecar, 1),
            ],
        )];
        write(&db, volume, 1, &mut units).await;

        let links: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT o.name, s.name, l.kind FROM file_sidecars l \
             JOIN files o ON o.id = l.file_id JOIN files s ON s.id = l.sidecar_id",
        )
        .fetch_all(db.read())
        .await
        .unwrap();
        assert_eq!(
            links,
            [(
                "IMG_1.CR3".to_owned(),
                "IMG_1.xmp".to_owned(),
                "stem".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn an_update_keeps_the_id_and_resets_the_pipeline() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let mut first = [unit("", vec![entry("a.jpg", FileKind::Image, 1)])];
        write(&db, volume, 1, &mut first).await;
        let id = first[0].ids[0].unwrap();
        sqlx::query(
            "UPDATE files SET meta_state = 1, hash_state = 1, thumb_state = 1, hash = zeroblob(20), \
             capture_ts = 123, width = 640 WHERE id = ?",
        )
        .bind(id)
        .execute(db.write())
        .await
        .unwrap();

        let mut second = [unit("", vec![entry("a.jpg", FileKind::Image, 99)])];
        second[0].ids = vec![Some(id)];
        write(&db, volume, 2, &mut second).await;

        assert_eq!(second[0].ids, [Some(id)]);
        let row = sqlx::query(
            "SELECT size, meta_state, hash_state, thumb_state, hash, capture_ts, width, seen_gen \
             FROM files WHERE id = ?",
        )
        .bind(id)
        .fetch_one(db.read())
        .await
        .unwrap();
        assert_eq!(row.get::<i64, _>("size"), 99);
        assert_eq!(
            (
                row.get::<i64, _>("meta_state"),
                row.get::<i64, _>("hash_state"),
                row.get::<i64, _>("thumb_state")
            ),
            (0, 0, 0)
        );
        assert!(row.get::<Option<Vec<u8>>, _>("hash").is_none());
        assert_eq!(
            row.get::<Option<i64>, _>("capture_ts"),
            Some(123),
            "extracted data stays until re-extracted"
        );
        assert_eq!(row.get::<Option<i64>, _>("width"), Some(640));
        assert_eq!(row.get::<i64, _>("seen_gen"), 2);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn ensured_directories_do_not_overwrite_known_ones() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        insert_dir(&db, volume, "a").await;
        sqlx::query("UPDATE dirs SET mtime = 42, seen_gen = 3 WHERE path = 'a'")
            .execute(db.write())
            .await
            .unwrap();

        let mut tx = db.begin_write().await.unwrap();
        ensure_dirs(&mut tx, volume, 9, &["", "a", "a/b"])
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let rows: Vec<(String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT path, mtime, seen_gen FROM dirs WHERE volume_id = ? ORDER BY path",
        )
        .bind(volume)
        .fetch_all(db.read())
        .await
        .unwrap();
        assert_eq!(
            rows,
            [
                (String::new(), None, 9),
                ("a".to_owned(), Some(42), 3),
                ("a/b".to_owned(), None, 9)
            ]
        );
    }

    #[tokio::test]
    async fn the_stored_rows_of_a_batch_are_grouped_by_directory() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let other = insert_volume(&db, "other").await;
        for dir in ["", "a", "a/b", "a/c", "z"] {
            insert_dir(&db, volume, dir).await;
        }
        insert_dir(&db, other, "a/other").await;
        insert_file(&db, volume, "a", "one.jpg").await;
        insert_file(&db, volume, "a", "two.jpg").await;
        insert_file(&db, volume, "a/b", "three.jpg").await;
        insert_file(&db, other, "a", "elsewhere.jpg").await;
        sqlx::query("UPDATE dirs SET mtime = 7 WHERE volume_id = ? AND path = 'a'")
            .bind(volume)
            .execute(db.write())
            .await
            .unwrap();

        let stored = load_stored(&db, volume, &["a", "missing"], &["a", "missing"])
            .await
            .unwrap();
        let mut names: Vec<_> = stored.files["a"].iter().map(|f| f.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["one.jpg", "two.jpg"]);
        assert!(
            !stored.files.contains_key("a/b"),
            "only the directories asked for"
        );
        let mut children = stored.children["a"].clone();
        children.sort_unstable();
        assert_eq!(children, ["b", "c"], "other volumes never leak in");
        assert_eq!(stored.dirs.get("a"), Some(&Some(7)));
        assert!(!stored.dirs.contains_key("missing"));
        assert_eq!(stored.files["a"][0].kind, FileKind::Other);
        assert_eq!(count_files(&db, volume).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn deleting_spares_rows_written_in_this_generation() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        let old = insert_file(&db, volume, "", "old.jpg").await;
        let fresh = insert_file(&db, volume, "", "fresh.jpg").await;
        sqlx::query("UPDATE files SET seen_gen = 5 WHERE id = ?")
            .bind(fresh)
            .execute(db.write())
            .await
            .unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let removed = delete_files(&mut tx, 5, &[old, fresh]).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(removed.rows, 1);
        assert_eq!(removed.dirs.into_iter().collect::<Vec<_>>(), [""]);
        let left: Vec<String> = sqlx::query_scalar("SELECT name FROM files")
            .fetch_all(db.read())
            .await
            .unwrap();
        assert_eq!(left, ["fresh.jpg"]);
    }

    #[tokio::test]
    async fn a_subtree_is_deleted_in_chunks_without_its_prefix_siblings() {
        let db = Db::open_temp().await.unwrap();
        let volume = insert_volume(&db, "photos").await;
        for dir in ["2019", "2019/Rome", "2019.old", "2019-x/y", "2018"] {
            insert_dir(&db, volume, dir).await;
            insert_file(&db, volume, dir, "a.jpg").await;
            insert_file(&db, volume, dir, "b.jpg").await;
        }
        let subtree = Subtree::new(&RelPath::new("2019").unwrap());
        assert_eq!(
            count_subtree_files(&db, volume, 3, &subtree).await.unwrap(),
            4
        );

        let mut tx = db.begin_write().await.unwrap();
        let removed = delete_subtree_files(&mut tx, volume, 3, &subtree)
            .await
            .unwrap();
        assert_eq!(removed.rows, 4);
        assert_eq!(
            removed.dirs.into_iter().collect::<Vec<_>>(),
            ["2019", "2019/Rome"]
        );
        assert_eq!(
            delete_subtree_files(&mut tx, volume, 3, &subtree)
                .await
                .unwrap()
                .rows,
            0
        );
        let removed = delete_subtree_dirs(&mut tx, volume, 3, &subtree)
            .await
            .unwrap();
        assert_eq!(removed.rows, 2);
        assert_eq!(
            removed.dirs.into_iter().collect::<Vec<_>>(),
            ["2019", "2019/Rome"]
        );
        tx.commit().await.unwrap();

        let left: Vec<String> = sqlx::query_scalar("SELECT path FROM dirs ORDER BY path")
            .fetch_all(db.read())
            .await
            .unwrap();
        assert_eq!(left, ["2018", "2019-x/y", "2019.old"]);
        assert_eq!(count_files(&db, volume).await.unwrap(), 6);
    }
}
