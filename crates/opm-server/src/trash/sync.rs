//! Sync with the disk: makes the `trash_items` rows agree with what `.opm-trash` holds.
//!
//! The trash layout `<volume root>/.opm-trash/<batch>/<original relative path>` says where every
//! file came from, so the trash stays usable when the database was lost or replaced and when
//! someone cleaned the trash by hand. [`TrashService::sync`] repairs both, for every online
//! volume:
//!
//! * **Removes** the rows whose file is definitely gone (`FsError::NotFound`; any other error
//!   keeps the row). The scanner's mass-disappearance guard applies: when at least 50 rows and
//!   more than a fifth of the volume's rows would go, none is removed and the count is reported,
//!   because that looks like an unmounted share, not like a cleaned trash. The rows that stay
//!   are harmless: a restore of one fails per item, and emptying the trash or the retention
//!   sweeper deletes a row whose content is already gone.
//! * **Adopts** every regular file below `.opm-trash/<batch>/` that no row covers, as a file
//!   item: `orig_path` is the path below the batch, `size` comes from the disk, `reason` and
//!   `meta_json` are `NULL` (the file restores like a new one), `task_id` is read from a `-t<id>`
//!   batch suffix and `trashed_at` from the batch name's `YYYYMMDD-HHMMSS` stamp (UTC). A batch
//!   that is not named like that gets the time of the sync, never the file's modification time:
//!   an old photo someone dropped there must not be older than the retention period on arrival.
//!   A directory that a folder item covers is not entered, files directly below `.opm-trash`
//!   belong to no batch and are left alone, and junk is adopted like anything else so that every
//!   byte of the trash is visible and "Empty trash" can reclaim it.
//!
//! It is idempotent and only reads the volume: the rows are the only thing it changes. At the end
//! it checks the running totals the summary reads (`trash_volume_totals`) against the rows and
//! rewrites them when they differ, which the triggers that keep them make a rare event. Symbolic
//! links are never followed (nor adopted), names that are not UTF-8 are skipped and counted, and
//! the task stops between directories when it is cancelled.
//!
//! The task has kind `fs_trash`, so it runs on the `fs` lane like every task that trashes,
//! restores or deletes: a file that `FsOps::trash_file` has just moved into the trash has no row
//! for a moment, and a sync that looked at that moment would adopt it without its reason and its
//! index snapshot. It is deliberately not an `index` task: `IndexJobs` counts every queued or
//! running `index` task as a scan (its status, and the startup and periodic scans that join a
//! scan that is already open).
//!
//! The lister is deliberately its own small one: the scanner's walker skips `.opm-trash` and junk
//! and classifies sidecars, none of which applies here.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDateTime;
use opm_core::consts::TRASH_DIR;
use opm_core::dto::TaskKind;
use opm_core::paths::{RelPath, TaskId, TrashItemId, VolumeId};
use serde_json::{Value, json};
use sqlx::{QueryBuilder, Sqlite};

use super::{TrashService, to_i64};
use crate::error::AppError;
use crate::indexer::guard_trips;
use crate::security::{FsError, VolumeRoot};
use crate::tasks::{TaskCtx, TaskError, TaskSpec};

const TITLE: &str = "Checking the trash";
/// Characters of the `YYYYMMDD-HHMMSS` stamp a batch name starts with.
const STAMP_LEN: usize = 15;
const STAMP_FORMAT: &str = "%Y%m%d-%H%M%S";
/// Rows whose file is looked at per blocking call.
const CHECK_PAGE: usize = 1_000;
/// Paths asked about per statement (SQLite allows 32766 binds).
const LOOKUP_CHUNK: usize = 500;
/// Files adopted per write transaction.
const ADOPT_CHUNK: usize = 1_000;
/// Rows deleted per statement.
const DELETE_CHUNK: usize = 5_000;

/// Whether the running totals (`trash_volume_totals`, kept by triggers) differ from what the rows
/// add up to, in either direction.
const TOTALS_DIFFER_SQL: &str = "SELECT EXISTS (\
     SELECT 1 FROM (\
       SELECT volume_id, COUNT(*) AS items, COALESCE(SUM(size), 0) AS bytes \
       FROM trash_items GROUP BY volume_id \
       EXCEPT SELECT volume_id, items, bytes FROM trash_volume_totals)) \
     OR EXISTS (\
     SELECT 1 FROM (\
       SELECT volume_id, items, bytes FROM trash_volume_totals \
       EXCEPT SELECT volume_id, COUNT(*), COALESCE(SUM(size), 0) \
       FROM trash_items GROUP BY volume_id))";

/// What a sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Tally {
    volumes: u64,
    /// Volumes that were offline and so were not looked at.
    offline: u64,
    adopted: u64,
    removed: u64,
    /// Rows whose file is missing that the mass-disappearance guard kept.
    held: u64,
    /// Entries that could not be read (permissions, odd names).
    unreadable: u64,
}

impl Tally {
    /// What the finished task says: "Checked the trash: added 3 items found on the disk, ...".
    fn title(&self) -> String {
        let mut parts = Vec::new();
        if self.adopted > 0 {
            parts.push(format!(
                "added {} found on the disk",
                counted(self.adopted, "item", "items")
            ));
        }
        if self.removed > 0 {
            parts.push(format!(
                "removed {} whose file is gone",
                counted(self.removed, "item", "items")
            ));
        }
        if self.held > 0 {
            parts.push(format!(
                "kept {} whose file is missing (is a share unmounted?)",
                counted(self.held, "item", "items")
            ));
        }
        if self.unreadable > 0 {
            parts.push(format!(
                "skipped {} that could not be read",
                counted(self.unreadable, "entry", "entries")
            ));
        }
        if self.offline > 0 {
            parts.push(format!(
                "{} offline",
                counted(self.offline, "volume", "volumes")
            ));
        }
        if parts.is_empty() {
            "The trash matches the disk".to_owned()
        } else {
            format!("Checked the trash: {}", parts.join(", "))
        }
    }

    fn result(&self) -> Value {
        json!({
            "adopted": self.adopted,
            "removed": self.removed,
            "held": self.held,
            "unreadable": self.unreadable,
            "volumes": self.volumes,
            "offline": self.offline,
        })
    }
}

fn counted(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// When the batch was made, from the stamp its name starts with (UTC). `None` for another name.
fn batch_time(batch: &str) -> Option<i64> {
    let stamp = batch.get(..STAMP_LEN)?;
    let time = NaiveDateTime::parse_from_str(stamp, STAMP_FORMAT).ok()?;
    Some(time.and_utc().timestamp())
}

/// The task that made the batch, from a `-t<id>` suffix after the stamp.
fn batch_task(batch: &str) -> Option<TaskId> {
    let digits = batch.get(STAMP_LEN..)?.strip_prefix("-t")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Splits `.opm-trash/<batch>/<below...>` into the batch name and the part below it. `None` for
/// `.opm-trash` itself and for anything outside it.
fn below_batch(dir: &RelPath) -> Option<(String, RelPath)> {
    let mut parts = dir.components();
    if parts.next()? != TRASH_DIR {
        return None;
    }
    let batch = parts.next()?.to_owned();
    let below = RelPath::new(&parts.collect::<Vec<_>>().join("/")).ok()?;
    Some((batch, below))
}

/// One directory of the trash as the walk sees it.
#[derive(Debug, Default, PartialEq, Eq)]
struct Listing {
    /// Regular files: name and size.
    files: Vec<(String, u64)>,
    dirs: Vec<String>,
    /// Entries that could not be read, and a directory that could not be opened.
    unreadable: u64,
}

/// Lists one directory on the current (blocking) thread. A directory that is gone, is not a
/// folder or is behind a link yields an empty listing; links and special files are left out.
fn list_dir(root: &VolumeRoot, dir: &RelPath) -> Listing {
    let mut listing = Listing::default();
    let path = match root.resolve_dir(dir) {
        Ok(path) => path,
        Err(FsError::NotFound(_) | FsError::NotADirectory(_) | FsError::SymlinkRejected(_)) => {
            return listing;
        }
        Err(_) => {
            listing.unreadable += 1;
            return listing;
        }
    };
    let Ok(entries) = std::fs::read_dir(&path) else {
        listing.unreadable += 1;
        return listing;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            listing.unreadable += 1;
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            listing.unreadable += 1;
            continue;
        };
        // Neither call follows a link.
        let Ok(kind) = entry.file_type() else {
            listing.unreadable += 1;
            continue;
        };
        if kind.is_dir() {
            listing.dirs.push(name);
        } else if kind.is_file() {
            match entry.metadata() {
                Ok(meta) => listing.files.push((name, meta.len())),
                Err(_) => listing.unreadable += 1,
            }
        }
    }
    listing.files.sort_unstable();
    listing.dirs.sort_unstable();
    listing
}

/// Whether the file of a row is definitely gone: only a missing path says so, any other error
/// (a permission, a link in the way) keeps the row.
fn is_gone(root: &VolumeRoot, trash_path: &str) -> bool {
    RelPath::new(trash_path).is_ok_and(|rel| matches!(root.stat(&rel), Err(FsError::NotFound(_))))
}

/// A row to look at: its id, volume and path.
type Checked = (TrashItemId, VolumeId, String);

/// The rows of `page` whose file is gone, grouped by volume. Rows of a volume without a root
/// (offline) are not looked at.
fn missing_in(
    roots: &HashMap<VolumeId, Arc<VolumeRoot>>,
    page: Vec<Checked>,
) -> Vec<(VolumeId, TrashItemId)> {
    page.into_iter()
        .filter(|(_, volume_id, path)| roots.get(volume_id).is_some_and(|root| is_gone(root, path)))
        .map(|(id, volume_id, _)| (volume_id, id))
        .collect()
}

impl TrashService {
    /// Queues the check of the trash against the disk and returns the task id. See the module
    /// docs for what it does; it never fails for what it finds, and reports its counts in the
    /// final title and result.
    pub async fn sync(&self) -> Result<TaskId, AppError> {
        let service = self.clone();
        let spec = TaskSpec::new(TaskKind::FsTrash, TITLE)
            .resumable()
            .with_params(json!({ "op": "trash_sync" }));
        let id = self
            .tasks
            .submit(spec, move |ctx| async move { service.run_sync(ctx).await })
            .await?;
        tracing::info!(task = id, "check of the trash queued");
        Ok(id)
    }

    pub(super) async fn run_sync(&self, ctx: TaskCtx) -> Result<Value, TaskError> {
        let volumes = self.volumes.refresh().await?;
        let mut tally = Tally::default();
        let mut roots = HashMap::new();
        for volume in &volumes {
            match volume.online.then(|| self.volumes.get(volume.id)) {
                Some(Ok(root)) => {
                    roots.insert(volume.id, root);
                }
                _ => tally.offline += 1,
            }
        }
        tally.volumes = roots.len() as u64;

        let checkpoint = crate::tasks::Checkpoint::new(&self.db, ctx.id).await?;
        if let Some(saved) = checkpoint.load("sync-removed").await? {
            tally = saved;
        } else {
            self.remove_gone(&ctx, &roots, &mut tally).await?;
            checkpoint.save("sync-removed", &tally).await?;
        }
        for volume in &volumes {
            if let Some(root) = roots.get(&volume.id) {
                self.adopt_untracked(&ctx, volume.id, root, &mut tally)
                    .await?;
            }
        }
        if self.repair_totals().await? {
            tracing::warn!(
                task = ctx.id,
                "the running totals of the trash disagreed with its rows and were recounted"
            );
        }

        tracing::info!(
            task = ctx.id,
            volumes = tally.volumes,
            adopted = tally.adopted,
            removed = tally.removed,
            held = tally.held,
            unreadable = tally.unreadable,
            "check of the trash finished"
        );
        ctx.progress.set_title(tally.title());
        Ok(tally.result())
    }

    /// Recounts the running totals the summary reads and rewrites them when they disagree with
    /// the rows. The triggers of migration 0048 keep them exact, so this is a safety net for a
    /// writer that got around them (`INSERT OR REPLACE`, an edited database). It looks without
    /// the write lock and only takes it when something is off; the answer is whether it repaired.
    async fn repair_totals(&self) -> Result<bool, TaskError> {
        let differ: bool = sqlx::query_scalar(TOTALS_DIFFER_SQL)
            .fetch_one(self.db.read())
            .await?;
        if !differ {
            return Ok(false);
        }
        let mut tx = self.db.begin_write().await?;
        // Under the write lock no row changes between the check and the rewrite.
        let still: bool = sqlx::query_scalar(TOTALS_DIFFER_SQL)
            .fetch_one(&mut *tx)
            .await?;
        if still {
            sqlx::query("DELETE FROM trash_volume_totals")
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO trash_volume_totals (volume_id, items, bytes) \
                 SELECT volume_id, COUNT(*), COALESCE(SUM(size), 0) FROM trash_items \
                 GROUP BY volume_id",
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(still)
    }

    /// Removes the rows whose file is gone from the volumes in `roots`, unless the guard holds a
    /// volume's removals back.
    async fn remove_gone(
        &self,
        ctx: &TaskCtx,
        roots: &HashMap<VolumeId, Arc<VolumeRoot>>,
        tally: &mut Tally,
    ) -> Result<(), TaskError> {
        let checkpoint = crate::tasks::Checkpoint::new(&self.db, ctx.id).await?;
        let totals: HashMap<VolumeId, i64> = match checkpoint.load("sync-totals").await? {
            Some(saved) => saved,
            None => {
                let totals: Vec<(VolumeId, i64)> = sqlx::query_as(
                    "SELECT volume_id, COUNT(*) FROM trash_items GROUP BY volume_id",
                )
                .fetch_all(self.db.read())
                .await?;
                let totals: HashMap<VolumeId, i64> = totals.into_iter().collect();
                checkpoint.save("sync-totals", &totals).await?;
                totals
            }
        };

        // One pass over the table by id: every page is a rowid range, whatever the volumes.
        let shared = Arc::new(roots.clone());
        let mut gone: HashMap<VolumeId, Vec<TrashItemId>> = HashMap::new();
        let mut after: TrashItemId = 0;
        let mut pages = checkpoint
            .entries::<(TrashItemId, Vec<(VolumeId, TrashItemId)>)>("sync-page:")
            .await?;
        pages.sort_by(|a, b| a.0.cmp(&b.0));
        let mut page_index = pages.len();
        for (_, (last, missing)) in pages {
            after = last;
            if missing.is_empty() {
                continue;
            }
            // A previously missing file may have returned while the server was stopped.
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT id, volume_id, trash_path FROM trash_items WHERE id IN (",
            );
            let mut ids = query.separated(", ");
            for (_, id) in missing {
                ids.push_bind(id);
            }
            ids.push_unseparated(")");
            let page: Vec<Checked> = query.build_query_as().fetch_all(self.db.read()).await?;
            let roots = Arc::clone(&shared);
            let missing = tokio::task::spawn_blocking(move || missing_in(&roots, page))
                .await
                .map_err(|err| AppError::internal(&err))?;
            for (volume_id, id) in missing {
                gone.entry(volume_id).or_default().push(id);
            }
        }
        loop {
            if ctx.cancel.is_cancelled() {
                return Err(TaskError::Cancelled);
            }
            let page: Vec<Checked> = sqlx::query_as(
                "SELECT id, volume_id, trash_path FROM trash_items WHERE id > ? ORDER BY id LIMIT ?",
            )
            .bind(after)
            .bind(CHECK_PAGE as i64)
            .fetch_all(self.db.read())
            .await?;
            let Some((last, _, _)) = page.last() else {
                break;
            };
            after = *last;
            let looked_at = page.len() as i64;
            let roots = Arc::clone(&shared);
            let missing = tokio::task::spawn_blocking(move || missing_in(&roots, page))
                .await
                .map_err(|err| AppError::internal(&err))?;
            checkpoint
                .save(&format!("sync-page:{page_index:020}"), &(after, &missing))
                .await?;
            page_index += 1;
            for (volume_id, id) in missing {
                gone.entry(volume_id).or_default().push(id);
            }
            ctx.progress.add(looked_at, 0);
        }

        for (volume_id, ids) in gone {
            let key = format!("sync-delete:{volume_id}");
            if let Some((removed, held)) = checkpoint.load::<(u64, u64)>(&key).await? {
                tally.removed += removed;
                tally.held += held;
                continue;
            }
            let total = totals.get(&volume_id).copied().unwrap_or(0).max(0) as u64;
            if guard_trips(ids.len() as u64, total) {
                tally.held += ids.len() as u64;
                checkpoint.save(&key, &(0_u64, ids.len() as u64)).await?;
                tracing::warn!(
                    volume = volume_id,
                    missing = ids.len(),
                    rows = total,
                    "rows of the trash kept: most of the volume's trash is missing, is the share mounted?"
                );
                continue;
            }
            let removed = self.delete_rows(&ids).await?;
            tally.removed += removed;
            checkpoint.save(&key, &(removed, 0_u64)).await?;
        }
        Ok(())
    }

    async fn delete_rows(&self, ids: &[TrashItemId]) -> Result<u64, TaskError> {
        let mut removed = 0;
        for chunk in ids.chunks(DELETE_CHUNK) {
            let mut builder = QueryBuilder::<Sqlite>::new("DELETE FROM trash_items WHERE id IN (");
            let mut list = builder.separated(", ");
            for id in chunk {
                list.push_bind(*id);
            }
            list.push_unseparated(")");
            let mut tx = self.db.begin_write().await?;
            removed += builder.build().execute(&mut *tx).await?.rows_affected();
            tx.commit().await?;
        }
        Ok(removed)
    }

    /// Walks the trash of one volume and adopts the files that no row covers.
    async fn adopt_untracked(
        &self,
        ctx: &TaskCtx,
        volume_id: VolumeId,
        root: &Arc<VolumeRoot>,
        tally: &mut Tally,
    ) -> Result<(), TaskError> {
        let Ok(trash) = RelPath::root().join(TRASH_DIR) else {
            return Ok(());
        };
        let checkpoint = crate::tasks::Checkpoint::new(&self.db, ctx.id).await?;
        let key = format!("sync-adopt:{volume_id}");
        let mut stack = match checkpoint.load::<(Vec<RelPath>, Tally)>(&key).await? {
            Some((stack, saved)) => {
                *tally = saved;
                stack
            }
            None => vec![trash],
        };
        while let Some(dir) = stack.pop() {
            if ctx.cancel.is_cancelled() {
                return Err(TaskError::Cancelled);
            }
            let listing = {
                let (root, dir) = (Arc::clone(root), dir.clone());
                tokio::task::spawn_blocking(move || list_dir(&root, &dir))
                    .await
                    .map_err(|err| AppError::internal(&err))?
            };
            tally.unreadable += listing.unreadable;
            ctx.progress
                .add((listing.files.len() + listing.dirs.len()) as i64, 0);

            let Some((batch, below)) = below_batch(&dir) else {
                // `.opm-trash` itself: its batches are what matters, a loose file is not one.
                push_children(&mut stack, &dir, &listing.dirs);
                checkpoint.save(&key, &(&stack, &*tally)).await?;
                continue;
            };
            let tracked = self.tracked_in(volume_id, &dir, &listing).await?;
            let untracked_dirs: Vec<String> = listing
                .dirs
                .iter()
                .filter(|name| !is_folder_item(&tracked, &dir, name))
                .cloned()
                .collect();
            push_children(&mut stack, &dir, &untracked_dirs);

            let fresh: Vec<&(String, u64)> = listing
                .files
                .iter()
                .filter(|(name, _)| !is_tracked(&tracked, &dir, name))
                .collect();
            if !fresh.is_empty() {
                tally.adopted += self
                    .adopt(ctx, volume_id, &batch, &dir, &below, &fresh)
                    .await?;
            }
            checkpoint.save(&key, &(&stack, &*tally)).await?;
        }
        Ok(())
    }

    /// Which entries of `listing` already have a row: trash path to `is_dir`.
    async fn tracked_in(
        &self,
        volume_id: VolumeId,
        dir: &RelPath,
        listing: &Listing,
    ) -> Result<HashMap<String, bool>, TaskError> {
        let paths: Vec<String> = listing
            .files
            .iter()
            .map(|(name, _)| name)
            .chain(&listing.dirs)
            .filter_map(|name| dir.join(name).ok())
            .map(|path| path.as_str().to_owned())
            .collect();
        let mut tracked = HashMap::new();
        for chunk in paths.chunks(LOOKUP_CHUNK) {
            let mut builder = QueryBuilder::<Sqlite>::new(
                "SELECT trash_path, is_dir FROM trash_items WHERE volume_id = ",
            );
            builder.push_bind(volume_id).push(" AND trash_path IN (");
            let mut list = builder.separated(", ");
            for path in chunk {
                list.push_bind(path.as_str());
            }
            list.push_unseparated(")");
            let rows: Vec<(String, bool)> =
                builder.build_query_as().fetch_all(self.db.read()).await?;
            tracked.extend(rows);
        }
        Ok(tracked)
    }

    /// Inserts `files` of one directory as file items and returns how many rows were new.
    async fn adopt(
        &self,
        ctx: &TaskCtx,
        volume_id: VolumeId,
        batch: &str,
        dir: &RelPath,
        below: &RelPath,
        files: &[&(String, u64)],
    ) -> Result<u64, TaskError> {
        let trashed_at = batch_time(batch).unwrap_or_else(|| self.now());
        let task_id = batch_task(batch);
        let mut adopted = 0;
        for chunk in files.chunks(ADOPT_CHUNK) {
            if ctx.cancel.is_cancelled() {
                return Err(TaskError::Cancelled);
            }
            let mut tx = self.db.begin_write().await?;
            for (name, size) in chunk.iter().copied() {
                let (Ok(trash_path), Ok(orig_path)) = (dir.join(name), below.join(name)) else {
                    continue;
                };
                let inserted = sqlx::query(
                    "INSERT INTO trash_items (volume_id, batch, orig_path, trash_path, is_dir, size, \
                     reason, task_id, meta_json, trashed_at) VALUES (?, ?, ?, ?, 0, ?, NULL, ?, NULL, ?) \
                     ON CONFLICT (volume_id, trash_path) DO NOTHING",
                )
                .bind(volume_id)
                .bind(batch)
                .bind(orig_path.as_str())
                .bind(trash_path.as_str())
                .bind(to_i64(*size))
                .bind(task_id)
                .bind(trashed_at)
                .execute(&mut *tx)
                .await?;
                adopted += inserted.rows_affected();
            }
            tx.commit().await?;
        }
        tracing::debug!(
            volume = volume_id,
            batch,
            adopted,
            "untracked trash files adopted"
        );
        Ok(adopted)
    }
}

fn push_children(stack: &mut Vec<RelPath>, dir: &RelPath, names: &[String]) {
    // Reversed, so the walk pops them in name order.
    stack.extend(names.iter().rev().filter_map(|name| dir.join(name).ok()));
}

/// Whether the entry has a row of its own (a file or a folder item).
fn is_tracked(tracked: &HashMap<String, bool>, dir: &RelPath, name: &str) -> bool {
    dir.join(name)
        .is_ok_and(|path| tracked.contains_key(path.as_str()))
}

/// Whether the directory is a folder item, which covers everything below it.
fn is_folder_item(tracked: &HashMap<String, bool>, dir: &RelPath, name: &str) -> bool {
    dir.join(name)
        .is_ok_and(|path| tracked.get(path.as_str()).copied() == Some(true))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    fn scratch() -> TempDir {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
        std::fs::create_dir_all(&base).unwrap();
        TempDir::new_in(base.canonicalize().unwrap()).unwrap()
    }

    #[test]
    fn the_stamp_of_a_batch_is_utc() {
        assert_eq!(batch_time("20240315-101500-t7"), Some(1_710_497_700));
        assert_eq!(batch_time("19991231-235959-r1a2b3c4d"), Some(946_684_799));
        assert_eq!(batch_time("20240315-101500"), Some(1_710_497_700));
    }

    #[test]
    fn a_name_without_a_valid_stamp_has_no_time() {
        for name in [
            "",
            "mystuff",
            "2024",
            "20241345-250000-t1",
            "20240315_101500-t1",
            "é0240315-101500",
        ] {
            assert_eq!(batch_time(name), None, "{name:?}");
        }
    }

    #[test]
    fn only_a_t_suffix_names_the_task() {
        assert_eq!(batch_task("20240315-101500-t7"), Some(7));
        assert_eq!(batch_task("20240315-101500-t1234567"), Some(1_234_567));
        for name in [
            "20240315-101500",
            "20240315-101500-r1a2b3c4d",
            "20240315-101500-x9",
            "20240315-101500-t",
            "20240315-101500-t7x",
            "20240315-101500-t-5",
            "20240315-101500-t+5",
            "20240315-101500-t99999999999999999999",
            "mystuff-t7",
        ] {
            assert_eq!(batch_task(name), None, "{name:?}");
        }
    }

    #[test]
    fn a_trash_directory_splits_into_the_batch_and_what_is_below() {
        let dir = RelPath::new(".opm-trash/20240315-101500-t7/Trips/2019").unwrap();
        let (batch, below) = below_batch(&dir).unwrap();
        assert_eq!(batch, "20240315-101500-t7");
        assert_eq!(below.as_str(), "Trips/2019");

        let batch_dir = RelPath::new(".opm-trash/b").unwrap();
        let (batch, below) = below_batch(&batch_dir).unwrap();
        assert_eq!(batch, "b");
        assert!(below.is_root());

        assert!(below_batch(&RelPath::new(".opm-trash").unwrap()).is_none());
        assert!(below_batch(&RelPath::new("Trips/2019").unwrap()).is_none());
        assert!(below_batch(&RelPath::root()).is_none());
    }

    #[test]
    fn the_title_says_what_changed() {
        assert_eq!(Tally::default().title(), "The trash matches the disk");
        let tally = Tally {
            adopted: 1,
            removed: 2,
            ..Tally::default()
        };
        assert_eq!(
            tally.title(),
            "Checked the trash: added 1 item found on the disk, removed 2 items whose file is gone"
        );
        let tally = Tally {
            held: 120,
            unreadable: 1,
            offline: 2,
            ..Tally::default()
        };
        assert_eq!(
            tally.title(),
            "Checked the trash: kept 120 items whose file is missing (is a share unmounted?), \
             skipped 1 entry that could not be read, 2 volumes offline"
        );
    }

    #[test]
    fn the_result_carries_every_counter() {
        let tally = Tally {
            volumes: 2,
            offline: 1,
            adopted: 3,
            removed: 4,
            held: 5,
            unreadable: 6,
        };
        assert_eq!(
            tally.result(),
            json!({"adopted": 3, "removed": 4, "held": 5, "unreadable": 6, "volumes": 2, "offline": 1})
        );
    }

    #[test]
    fn a_listing_has_regular_files_and_folders_only() {
        let dir = scratch();
        let root = VolumeRoot::new(1, dir.path()).unwrap();
        let trash = dir.path().join(".opm-trash/b");
        std::fs::create_dir_all(trash.join("sub")).unwrap();
        std::fs::write(trash.join("b.jpg"), b"12345").unwrap();
        std::fs::write(trash.join("a.jpg"), b"1").unwrap();
        std::os::unix::fs::symlink(trash.join("a.jpg"), trash.join("link.jpg")).unwrap();
        std::os::unix::fs::symlink(trash.join("sub"), trash.join("dirlink")).unwrap();

        let listing = list_dir(&root, &RelPath::new(".opm-trash/b").unwrap());
        assert_eq!(
            listing,
            Listing {
                files: vec![("a.jpg".to_owned(), 1), ("b.jpg".to_owned(), 5)],
                dirs: vec!["sub".to_owned()],
                unreadable: 0,
            }
        );
    }

    #[test]
    fn a_missing_or_linked_directory_lists_as_empty() {
        let dir = scratch();
        let root = VolumeRoot::new(1, dir.path()).unwrap();
        std::fs::create_dir_all(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/x.jpg"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join(".opm-trash")).unwrap();

        assert_eq!(
            list_dir(&root, &RelPath::new(".opm-trash").unwrap()),
            Listing::default()
        );
        assert_eq!(
            list_dir(&root, &RelPath::new("nothing/here").unwrap()),
            Listing::default()
        );
        assert_eq!(
            list_dir(&root, &RelPath::new("real/x.jpg").unwrap()),
            Listing::default()
        );
    }

    #[test]
    fn only_a_missing_path_counts_as_gone() {
        let dir = scratch();
        let root = VolumeRoot::new(1, dir.path()).unwrap();
        std::fs::create_dir_all(dir.path().join(".opm-trash/b")).unwrap();
        std::fs::write(dir.path().join(".opm-trash/b/here.jpg"), b"x").unwrap();
        std::os::unix::fs::symlink(
            dir.path().join(".opm-trash/b/here.jpg"),
            dir.path().join(".opm-trash/b/link.jpg"),
        )
        .unwrap();

        assert!(!is_gone(&root, ".opm-trash/b/here.jpg"));
        assert!(is_gone(&root, ".opm-trash/b/gone.jpg"));
        assert!(is_gone(&root, ".opm-trash/missing-batch/x.jpg"));
        assert!(
            !is_gone(&root, ".opm-trash/b/link.jpg"),
            "a link in the way is not 'gone'"
        );
        assert!(
            !is_gone(&root, "../outside"),
            "a corrupt path keeps its row"
        );
    }
}
