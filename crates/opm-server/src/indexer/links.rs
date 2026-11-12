//! Sidecar links of the directories a scan touched.
//!
//! The rules are `opm_core::sidecar`: [`SidecarMatcher::link`] maps the file NAMES of one
//! directory to links, so the links of a directory are a pure function of its names and of the
//! sidecar-extension setting. This module turns that into the delta against the stored
//! `file_sidecars` rows and applies it, in the same transaction that wrote the file rows.
//!
//! * A directory's links are recomputed when a file appeared, vanished or changed kind (a sidecar
//!   setting change reclassifies files), and on every directory of a forced scan.
//! * The links come from the directory LISTING, mapped to file ids. Rows of vanished files still
//!   exist until the end of the scan, so they must not be read back by name; their links are left
//!   alone and go with the row (foreign keys cascade).
//! * An owner whose sidecars changed (a sidecar was added, removed, rewritten or reclassified)
//!   gets `meta_state = pending` again: its metadata merges the sidecars' data, so it must be
//!   extracted again.
//! * An orphan sidecar is an ordinary `sidecar` row without links.

use std::collections::{HashMap, HashSet};

use opm_core::paths::FileId;
use opm_core::sidecar::{LinkKind, SidecarMatcher};
use sqlx::{QueryBuilder, Row as _, Sqlite, SqliteConnection};

use crate::db::models::PipelineState;

/// Rows per statement; two binds per row.
const LINK_CHUNK: usize = 1000;
/// Ids per `IN (...)` list.
const ID_CHUNK: usize = 5000;

/// One `file_sidecars` row: `owner` owns `sidecar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct LinkRow {
    pub owner: FileId,
    pub sidecar: FileId,
    pub kind: LinkKind,
}

/// What to change so that the stored links match the wanted ones.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct LinkDelta {
    /// Links to add (or whose kind to correct).
    pub insert: Vec<LinkRow>,
    /// `(owner, sidecar)` pairs to remove.
    pub delete: Vec<(FileId, FileId)>,
    /// Owners whose set of sidecars changed, sorted, without duplicates.
    pub reextract: Vec<FileId>,
}

impl LinkDelta {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.insert.is_empty() && self.delete.is_empty()
    }
}

/// The links a directory with the files `names` implies. `ids[i]` is the file id of `names[i]`.
pub(crate) fn wanted_links(
    matcher: &SidecarMatcher,
    names: &[&str],
    ids: &[FileId],
) -> Vec<LinkRow> {
    debug_assert_eq!(names.len(), ids.len());
    matcher
        .link(names)
        .into_iter()
        .map(|link| LinkRow {
            owner: ids[link.owner],
            sidecar: ids[link.sidecar],
            kind: link.kind,
        })
        .collect()
}

/// Compares the wanted links of a directory with the stored ones. `present` holds the ids of the
/// files in the directory listing: a stored link is only removed when both of its ends are in it
/// (a vanished file takes its links with it when its row is deleted).
pub(crate) fn link_delta(
    wanted: &[LinkRow],
    stored: &[LinkRow],
    present: &HashSet<FileId>,
) -> LinkDelta {
    let want: HashSet<(FileId, FileId)> = wanted.iter().map(|l| (l.owner, l.sidecar)).collect();
    let have: HashMap<(FileId, FileId), LinkKind> = stored
        .iter()
        .map(|l| ((l.owner, l.sidecar), l.kind))
        .collect();

    let mut delta = LinkDelta::default();
    for link in wanted {
        if have.get(&(link.owner, link.sidecar)) != Some(&link.kind) {
            delta.insert.push(*link);
            delta.reextract.push(link.owner);
        }
    }
    for link in stored {
        let key = (link.owner, link.sidecar);
        if !want.contains(&key) && present.contains(&link.owner) && present.contains(&link.sidecar)
        {
            delta.delete.push(key);
            delta.reextract.push(link.owner);
        }
    }
    delta.reextract.sort_unstable();
    delta.reextract.dedup();
    delta
}

/// The stored links whose sidecar lies in one of `dirs` of the volume (a link never leaves its
/// directory).
pub(crate) async fn load_stored(
    conn: &mut SqliteConnection,
    volume_id: i64,
    dirs: &[&str],
) -> Result<Vec<LinkRow>, sqlx::Error> {
    let mut stored = Vec::new();
    for chunk in dirs.chunks(ID_CHUNK) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT file_id, sidecar_id, kind FROM file_sidecars WHERE sidecar_id IN \
             (SELECT id FROM files WHERE volume_id = ",
        );
        qb.push_bind(volume_id).push(" AND dir_path IN (");
        let mut list = qb.separated(", ");
        for dir in chunk {
            list.push_bind(*dir);
        }
        qb.push("))");
        for row in qb.build().fetch_all(&mut *conn).await? {
            let kind: String = row.try_get("kind")?;
            stored.push(LinkRow {
                owner: row.try_get("file_id")?,
                sidecar: row.try_get("sidecar_id")?,
                kind: kind
                    .parse()
                    .map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
            });
        }
    }
    Ok(stored)
}

/// Applies `delta` and resets the extraction of the owners it touched. Returns the number of
/// link rows written.
pub(crate) async fn apply_delta(
    conn: &mut SqliteConnection,
    delta: &LinkDelta,
) -> Result<u64, sqlx::Error> {
    for chunk in delta.delete.chunks(LINK_CHUNK) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "DELETE FROM file_sidecars WHERE (file_id, sidecar_id) IN (",
        );
        qb.push_values(chunk, |mut row, (owner, sidecar)| {
            row.push_bind(*owner).push_bind(*sidecar);
        });
        qb.push(")");
        qb.build().execute(&mut *conn).await?;
    }
    for chunk in delta.insert.chunks(LINK_CHUNK) {
        let mut qb =
            QueryBuilder::<Sqlite>::new("INSERT INTO file_sidecars (file_id, sidecar_id, kind) ");
        qb.push_values(chunk, |mut row, link| {
            row.push_bind(link.owner)
                .push_bind(link.sidecar)
                .push_bind(link.kind.as_str());
        });
        qb.push(" ON CONFLICT (file_id, sidecar_id) DO UPDATE SET kind = excluded.kind");
        qb.build().execute(&mut *conn).await?;
    }
    reextract(conn, &delta.reextract).await?;
    Ok((delta.insert.len() + delta.delete.len()) as u64)
}

/// Starts the `UPDATE` that puts files back in the queue of the metadata extractor; the caller
/// adds the condition that picks the files. Files that are pending already are left alone, and so
/// are those the stage does not apply to (a file that just became a sidecar stays as it is).
fn requeue_statement() -> QueryBuilder<Sqlite> {
    let mut qb = QueryBuilder::<Sqlite>::new("UPDATE files SET meta_state = ");
    qb.push_bind(PipelineState::Pending.code())
        .push(" WHERE meta_state NOT IN (")
        .push_bind(PipelineState::Pending.code())
        .push(", ")
        .push_bind(PipelineState::NotApplicable.code())
        .push(") AND ");
    qb
}

/// Puts the files `ids` back in the queue of the metadata extractor.
pub(crate) async fn reextract(
    conn: &mut SqliteConnection,
    ids: &[FileId],
) -> Result<(), sqlx::Error> {
    for chunk in ids.chunks(ID_CHUNK) {
        let mut qb = requeue_statement();
        qb.push("id IN (");
        let mut list = qb.separated(", ");
        for id in chunk {
            list.push_bind(*id);
        }
        qb.push(")");
        qb.build().execute(&mut *conn).await?;
    }
    Ok(())
}

/// Puts the owners of the sidecars `sidecar_ids` back in the queue of the metadata extractor
/// (their sidecar changed or is about to be deleted).
pub(crate) async fn reextract_owners_of(
    conn: &mut SqliteConnection,
    sidecar_ids: &[FileId],
) -> Result<(), sqlx::Error> {
    for chunk in sidecar_ids.chunks(ID_CHUNK) {
        let mut qb = requeue_statement();
        qb.push("id IN (SELECT file_id FROM file_sidecars WHERE sidecar_id IN (");
        let mut list = qb.separated(", ");
        for id in chunk {
            list.push_bind(*id);
        }
        qb.push("))");
        qb.build().execute(&mut *conn).await?;
    }
    Ok(())
}

/// Recomputes the links of one directory from the file rows stored in it and applies the
/// difference. The seam for the file operations: after they added, renamed or removed files they
/// call this in the same transaction (the scanner derives the links from its listing instead).
/// Returns the number of link rows written.
pub async fn relink_dir(
    conn: &mut SqliteConnection,
    volume_id: i64,
    dir_path: &str,
    matcher: &SidecarMatcher,
) -> Result<u64, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, name FROM files WHERE volume_id = ? AND dir_path = ? ORDER BY name",
    )
    .bind(volume_id)
    .bind(dir_path)
    .fetch_all(&mut *conn)
    .await?;
    let mut ids = Vec::with_capacity(rows.len());
    let mut names = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.try_get::<FileId, _>("id")?);
        names.push(row.try_get::<&str, _>("name")?);
    }
    let wanted = wanted_links(matcher, &names, &ids);
    let stored = load_stored(&mut *conn, volume_id, &[dir_path]).await?;
    let present: HashSet<FileId> = ids.iter().copied().collect();
    apply_delta(conn, &link_delta(&wanted, &stored, &present)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::db::testing::{insert_dir, insert_file, insert_volume};

    fn row(owner: FileId, sidecar: FileId, kind: LinkKind) -> LinkRow {
        LinkRow {
            owner,
            sidecar,
            kind,
        }
    }

    fn set(ids: &[FileId]) -> HashSet<FileId> {
        ids.iter().copied().collect()
    }

    #[test]
    fn wanted_links_map_listing_indexes_to_ids() {
        let names = ["IMG_1.CR3", "IMG_1.xmp", "IMG_1.CR3.xmp", "lonely.xmp"];
        let ids = [10, 11, 12, 13];
        let mut wanted = wanted_links(&SidecarMatcher::default(), &names, &ids);
        wanted.sort_by_key(|l| (l.sidecar, l.owner));
        assert_eq!(
            wanted,
            [row(10, 11, LinkKind::Stem), row(10, 12, LinkKind::Exact)]
        );
    }

    #[test]
    fn an_empty_extension_list_links_nothing() {
        let matcher = SidecarMatcher::new::<&str>(&[]);
        assert!(wanted_links(&matcher, &["a.jpg", "a.xmp"], &[1, 2]).is_empty());
    }

    #[test]
    fn identical_links_need_no_change() {
        let links = [row(1, 2, LinkKind::Exact)];
        let delta = link_delta(&links, &links, &set(&[1, 2]));
        assert!(delta.is_empty());
        assert!(delta.reextract.is_empty());
    }

    #[test]
    fn new_and_stale_links_are_found_and_their_owners_marked() {
        let wanted = [row(1, 2, LinkKind::Stem), row(3, 4, LinkKind::Exact)];
        let stored = [row(1, 2, LinkKind::Stem), row(5, 2, LinkKind::Stem)];
        let delta = link_delta(&wanted, &stored, &set(&[1, 2, 3, 4, 5]));
        assert_eq!(delta.insert, [row(3, 4, LinkKind::Exact)]);
        assert_eq!(delta.delete, [(5, 2)]);
        assert_eq!(delta.reextract, [3, 5]);
    }

    #[test]
    fn a_link_of_a_vanished_file_is_left_to_the_row_deletion() {
        let stored = [row(1, 2, LinkKind::Stem), row(9, 2, LinkKind::Stem)];
        let wanted = [row(1, 2, LinkKind::Stem)];
        let delta = link_delta(&wanted, &stored, &set(&[1, 2]));
        assert!(delta.is_empty(), "owner 9 is not in the listing: {delta:?}");
    }

    #[test]
    fn a_changed_kind_is_rewritten() {
        let delta = link_delta(
            &[row(1, 2, LinkKind::Exact)],
            &[row(1, 2, LinkKind::Stem)],
            &set(&[1, 2]),
        );
        assert_eq!(delta.insert, [row(1, 2, LinkKind::Exact)]);
        assert!(delta.delete.is_empty());
        assert_eq!(delta.reextract, [1]);
    }

    struct Rows {
        db: crate::db::TempDb,
        volume: i64,
        owner: FileId,
        xmp: FileId,
    }

    async fn rows() -> Rows {
        let db = Db::open_temp().await.unwrap();
        let handle = db.handle();
        let volume = insert_volume(&handle, "photos").await;
        insert_dir(&handle, volume, "").await;
        let owner = insert_file(&handle, volume, "", "a.jpg").await;
        let xmp = insert_file(&handle, volume, "", "a.xmp").await;
        sqlx::query("UPDATE files SET meta_state = 1 WHERE id = ?")
            .bind(owner)
            .execute(handle.write())
            .await
            .unwrap();
        Rows {
            db,
            volume,
            owner,
            xmp,
        }
    }

    async fn meta_state(db: &Db, id: FileId) -> i64 {
        sqlx::query_scalar("SELECT meta_state FROM files WHERE id = ?")
            .bind(id)
            .fetch_one(db.read())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn links_are_stored_loaded_and_removed() {
        let t = rows().await;
        let db = t.db.handle();
        let link = row(t.owner, t.xmp, LinkKind::Stem);

        let mut tx = db.begin_write().await.unwrap();
        let delta = link_delta(&[link], &[], &set(&[t.owner, t.xmp]));
        assert_eq!(apply_delta(&mut tx, &delta).await.unwrap(), 1);
        let stored = load_stored(&mut tx, t.volume, &[""]).await.unwrap();
        assert_eq!(stored, [link]);
        assert!(
            load_stored(&mut tx, t.volume, &["other"])
                .await
                .unwrap()
                .is_empty()
        );
        tx.commit().await.unwrap();
        assert_eq!(
            meta_state(&db, t.owner).await,
            0,
            "the owner is queued again"
        );

        sqlx::query("UPDATE files SET meta_state = 1 WHERE id = ?")
            .bind(t.owner)
            .execute(db.write())
            .await
            .unwrap();
        let mut tx = db.begin_write().await.unwrap();
        let delta = link_delta(&[], &[link], &set(&[t.owner, t.xmp]));
        apply_delta(&mut tx, &delta).await.unwrap();
        assert!(
            load_stored(&mut tx, t.volume, &[""])
                .await
                .unwrap()
                .is_empty()
        );
        tx.commit().await.unwrap();
        assert_eq!(meta_state(&db, t.owner).await, 0);
    }

    #[tokio::test]
    async fn rewriting_a_link_corrects_its_kind() {
        let t = rows().await;
        let db = t.db.handle();
        let mut tx = db.begin_write().await.unwrap();
        let stem = row(t.owner, t.xmp, LinkKind::Stem);
        let exact = row(t.owner, t.xmp, LinkKind::Exact);
        apply_delta(&mut tx, &link_delta(&[stem], &[], &set(&[t.owner, t.xmp])))
            .await
            .unwrap();
        apply_delta(
            &mut tx,
            &link_delta(&[exact], &[stem], &set(&[t.owner, t.xmp])),
        )
        .await
        .unwrap();
        assert_eq!(
            load_stored(&mut tx, t.volume, &[""]).await.unwrap(),
            [exact]
        );
    }

    #[tokio::test]
    async fn a_directory_is_relinked_from_its_stored_names() {
        let t = rows().await;
        let db = t.db.handle();
        let matcher = SidecarMatcher::default();
        let mut tx = db.begin_write().await.unwrap();

        assert_eq!(
            relink_dir(&mut tx, t.volume, "", &matcher).await.unwrap(),
            1
        );
        let stem = row(t.owner, t.xmp, LinkKind::Stem);
        assert_eq!(load_stored(&mut tx, t.volume, &[""]).await.unwrap(), [stem]);
        assert_eq!(
            relink_dir(&mut tx, t.volume, "", &matcher).await.unwrap(),
            0,
            "nothing changed"
        );

        sqlx::query(
            "UPDATE files SET name = 'b.xmp', name_norm = 'b.xmp', stem_norm = 'b' WHERE id = ?",
        )
        .bind(t.xmp)
        .execute(&mut *tx)
        .await
        .unwrap();
        assert_eq!(
            relink_dir(&mut tx, t.volume, "", &matcher).await.unwrap(),
            1
        );
        assert!(
            load_stored(&mut tx, t.volume, &[""])
                .await
                .unwrap()
                .is_empty()
        );
        tx.commit().await.unwrap();
        assert_eq!(meta_state(&db, t.owner).await, 0);
    }

    #[tokio::test]
    async fn files_the_stage_does_not_apply_to_are_not_queued() {
        let t = rows().await;
        let db = t.db.handle();
        let mut tx = db.begin_write().await.unwrap();
        sqlx::query("UPDATE files SET meta_state = ? WHERE id = ?")
            .bind(PipelineState::NotApplicable.code())
            .bind(t.owner)
            .execute(&mut *tx)
            .await
            .unwrap();
        let link = row(t.owner, t.xmp, LinkKind::Stem);
        apply_delta(&mut tx, &link_delta(&[link], &[], &set(&[t.owner, t.xmp])))
            .await
            .unwrap();
        reextract_owners_of(&mut tx, &[t.xmp]).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            meta_state(&db, t.owner).await,
            PipelineState::NotApplicable.code()
        );
    }

    #[tokio::test]
    async fn owners_of_a_sidecar_are_queued_again() {
        let t = rows().await;
        let db = t.db.handle();
        let mut tx = db.begin_write().await.unwrap();
        let link = row(t.owner, t.xmp, LinkKind::Stem);
        apply_delta(&mut tx, &link_delta(&[link], &[], &set(&[t.owner, t.xmp])))
            .await
            .unwrap();
        sqlx::query("UPDATE files SET meta_state = 1")
            .execute(&mut *tx)
            .await
            .unwrap();
        reextract_owners_of(&mut tx, &[t.xmp]).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(meta_state(&db, t.owner).await, 0);
        assert_eq!(
            meta_state(&db, t.xmp).await,
            1,
            "only owners are queued again"
        );
    }
}
