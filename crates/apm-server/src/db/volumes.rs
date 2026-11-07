//! Volumes: every sub-directory of `APM_VOLUMES_DIR` is one volume, registered in the `volumes` table.
//!
//! [`sync_volumes`] runs at startup (`Db::from_config`) and whenever the scanner wants to re-check
//! what is mounted. It never deletes a row: a volume that disappeared (an unmounted share) stays,
//! marked offline, so its indexed files, tags and history survive until it comes back. Volume ids are
//! stable: a directory that reappears under the same name gets its old row.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use apm_core::paths::VolumeId;

use super::models::Volume;
use super::{Db, DbError, now_secs};

/// Prefix of the temporary file [`sync_volumes`] creates (and removes) in a volume root to find out
/// whether the volume is writable. Watchers and scanners should ignore names with this prefix.
pub const PROBE_PREFIX: &str = ".apm-probe-";

/// What the file system says about one sub-directory of the volumes directory.
#[derive(Debug)]
struct Found {
    name: String,
    root: String,
    online: bool,
    writable: bool,
}

/// Registers the volumes found in `volumes_dir` and returns every known volume (found or not),
/// ordered by name.
///
/// * A sub-directory (or a symlink to one) whose name does not start with `.` is a volume. Plain
///   files, hidden directories, dangling links and names that are not UTF-8 are ignored.
/// * `root` is the canonical absolute path. `online` is whether the root can be listed; `writable`
///   is proven by creating and removing a [`PROBE_PREFIX`] file, and is always false when offline.
/// * Volumes that are not found keep their row, with `online = 0` and `writable = 0`. A missing or
///   unreadable `volumes_dir` therefore puts every volume offline instead of failing.
///
/// The file system is inspected first; the table is then updated in one short transaction.
pub async fn sync_volumes(db: &Db, volumes_dir: &Path) -> Result<Vec<Volume>, DbError> {
    let dir = volumes_dir.to_path_buf();
    let found = tokio::task::spawn_blocking(move || discover(&dir))
        .await
        .map_err(|err| DbError::Setup(format!("volume discovery failed: {err}")))?;

    let now = now_secs();
    let mut tx = db.begin_write().await?;
    sqlx::query("UPDATE volumes SET online = 0, writable = 0")
        .execute(&mut *tx)
        .await?;
    for volume in &found {
        sqlx::query(
            "INSERT INTO volumes (name, root, writable, online, created_at) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (name) DO UPDATE SET \
             root = excluded.root, writable = excluded.writable, online = excluded.online",
        )
        .bind(&volume.name)
        .bind(&volume.root)
        .bind(volume.writable)
        .bind(volume.online)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let volumes = list(db).await?;
    for volume in volumes.iter().filter(|volume| !volume.online) {
        tracing::warn!(volume = %volume.name, root = %volume.root, "volume is offline");
    }
    Ok(volumes)
}

/// Every known volume, ordered by name (case-insensitively), then id.
pub async fn list(db: &Db) -> Result<Vec<Volume>, DbError> {
    Ok(sqlx::query_as::<_, Volume>(
        "SELECT id, name, root, writable, online, scan_gen, last_scan_at, created_at \
         FROM volumes ORDER BY name COLLATE NOCASE, id",
    )
    .fetch_all(db.read())
    .await?)
}

/// One volume by id.
pub async fn get(db: &Db, id: VolumeId) -> Result<Option<Volume>, DbError> {
    Ok(sqlx::query_as::<_, Volume>(
        "SELECT id, name, root, writable, online, scan_gen, last_scan_at, created_at \
         FROM volumes WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db.read())
    .await?)
}

fn discover(volumes_dir: &Path) -> Vec<Found> {
    let entries = match fs::read_dir(volumes_dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                dir = %volumes_dir.display(),
                error = %err,
                "cannot list the volumes directory, every volume is reported offline"
            );
            return Vec::new();
        }
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(error = %err, "cannot read an entry of the volumes directory");
                continue;
            }
        };
        let Ok(name) = entry.file_name().into_string() else {
            tracing::warn!(entry = ?entry.file_name(), "ignoring a volume whose name is not UTF-8");
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // `metadata` follows symlinks: a link to a directory is a volume, a dangling one is not.
        if !fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        let root = match fs::canonicalize(&path) {
            Ok(root) => root,
            Err(err) => {
                tracing::warn!(volume = %name, error = %err, "cannot resolve the volume path");
                continue;
            }
        };
        let Some(root) = root.to_str().map(str::to_owned) else {
            tracing::warn!(volume = %name, "ignoring a volume whose path is not UTF-8");
            continue;
        };
        let online = fs::read_dir(&root).is_ok();
        let writable = online && probe_writable(Path::new(&root));
        found.push(Found {
            name,
            root,
            online,
            writable,
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Creates and removes a uniquely named file in `root`. A volume where the probe file can be created
/// but not removed counts as read-only: nothing could be cleaned up on it either.
fn probe_writable(root: &Path) -> bool {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    for _ in 0..8 {
        let name = format!(
            "{PROBE_PREFIX}{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = root.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                drop(file);
                return match fs::remove_file(&path) {
                    Ok(()) => true,
                    Err(err) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %err,
                            "created a probe file but could not remove it, treating the volume as read-only"
                        );
                        false
                    }
                };
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::db::TempDb;

    struct Fixture {
        db: TempDb,
        volumes_dir: PathBuf,
    }

    impl Fixture {
        async fn new() -> Self {
            let db = Db::open_temp().await.unwrap();
            let volumes_dir = db.dir().join("volumes");
            fs::create_dir_all(&volumes_dir).unwrap();
            Self { db, volumes_dir }
        }

        fn mkdir(&self, name: &str) -> PathBuf {
            let path = self.volumes_dir.join(name);
            fs::create_dir_all(&path).unwrap();
            path
        }

        async fn sync(&self) -> Vec<Volume> {
            sync_volumes(&self.db, &self.volumes_dir).await.unwrap()
        }
    }

    fn names(volumes: &[Volume]) -> Vec<&str> {
        volumes.iter().map(|volume| volume.name.as_str()).collect()
    }

    /// Permission bits do not restrict root, which is how some CI containers run the tests.
    #[cfg(unix)]
    fn permissions_are_enforced(dir: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let probe = dir.join("permission-probe");
        fs::create_dir(&probe).unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o555)).unwrap();
        let enforced = fs::File::create(probe.join("x")).is_err();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(&probe).unwrap();
        enforced
    }

    #[tokio::test]
    async fn registers_each_subdirectory_as_an_online_writable_volume() {
        let fx = Fixture::new().await;
        let photos = fx.mkdir("photos");
        fx.mkdir("Archive");
        let volumes = fx.sync().await;

        assert_eq!(names(&volumes), ["Archive", "photos"]);
        let photos_row = &volumes[1];
        assert_eq!(
            photos_row.root,
            fs::canonicalize(photos).unwrap().to_str().unwrap()
        );
        assert!(photos_row.online && photos_row.writable);
        assert_eq!((photos_row.scan_gen, photos_row.last_scan_at), (0, None));
        assert!((now_secs() - photos_row.created_at).abs() < 60);
        assert_eq!(list(&fx.db).await.unwrap(), volumes);
        assert_eq!(
            get(&fx.db, photos_row.id).await.unwrap().as_ref(),
            Some(photos_row)
        );
        assert_eq!(get(&fx.db, 9_999).await.unwrap(), None);
    }

    #[tokio::test]
    async fn ids_and_creation_time_are_stable_across_syncs() {
        let fx = Fixture::new().await;
        fx.mkdir("a");
        fx.mkdir("b");
        let first = fx.sync().await;
        sqlx::query("UPDATE volumes SET scan_gen = 7, last_scan_at = 42")
            .execute(fx.db.write())
            .await
            .unwrap();
        let second = fx.sync().await;

        assert_eq!(first.len(), 2);
        for (before, after) in first.iter().zip(&second) {
            assert_eq!(before.id, after.id);
            assert_eq!(before.created_at, after.created_at);
            assert_eq!(
                (after.scan_gen, after.last_scan_at),
                (7, Some(42)),
                "scan state survives"
            );
        }
    }

    #[tokio::test]
    async fn a_removed_directory_goes_offline_and_comes_back_with_its_id() {
        let fx = Fixture::new().await;
        fx.mkdir("a");
        fx.mkdir("b");
        let first = fx.sync().await;
        let b_id = first[1].id;
        let file_id = crate::db::testing::insert_file(&fx.db, b_id, "", "keep.jpg").await;

        fs::remove_dir_all(fx.volumes_dir.join("b")).unwrap();
        let second = fx.sync().await;
        assert_eq!(names(&second), ["a", "b"], "the row is kept");
        assert!(second[0].online);
        assert!(!second[1].online && !second[1].writable);
        assert_eq!(second[1].id, b_id);
        assert_eq!(second[1].root, first[1].root, "the last known root is kept");
        let files: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE id = ?")
            .bind(file_id)
            .fetch_one(fx.db.read())
            .await
            .unwrap();
        assert_eq!(files, 1, "the indexed files of an offline volume are kept");

        fx.mkdir("b");
        let third = fx.sync().await;
        assert_eq!(third[1].id, b_id);
        assert!(third[1].online && third[1].writable);
    }

    #[tokio::test]
    async fn a_renamed_directory_is_a_new_volume_and_the_old_one_goes_offline() {
        let fx = Fixture::new().await;
        fx.mkdir("old");
        let first = fx.sync().await;
        fs::rename(fx.volumes_dir.join("old"), fx.volumes_dir.join("new")).unwrap();
        let second = fx.sync().await;

        assert_eq!(names(&second), ["new", "old"]);
        assert!(second[0].online);
        assert_ne!(second[0].id, first[0].id);
        assert!(!second[1].online);
        assert_eq!(second[1].id, first[0].id);
    }

    #[tokio::test]
    async fn hidden_directories_plain_files_and_dangling_links_are_ignored() {
        let fx = Fixture::new().await;
        fx.mkdir("photos");
        fx.mkdir(".hidden");
        fx.mkdir(".apm-trash");
        fs::write(fx.volumes_dir.join("notes.txt"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(fx.volumes_dir.join("gone"), fx.volumes_dir.join("dangling"))
            .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            fx.volumes_dir.join("notes.txt"),
            fx.volumes_dir.join("file-link"),
        )
        .unwrap();

        assert_eq!(names(&fx.sync().await), ["photos"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_to_a_directory_is_a_volume_with_the_resolved_root() {
        let fx = Fixture::new().await;
        let real = fx.db.dir().join("elsewhere");
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, fx.volumes_dir.join("linked")).unwrap();

        let volumes = fx.sync().await;
        assert_eq!(names(&volumes), ["linked"]);
        assert_eq!(
            volumes[0].root,
            fs::canonicalize(&real).unwrap().to_str().unwrap()
        );
        assert!(volumes[0].online && volumes[0].writable);
    }

    #[tokio::test]
    async fn the_probe_leaves_nothing_behind() {
        let fx = Fixture::new().await;
        let root = fx.mkdir("photos");
        fx.sync().await;
        fx.sync().await;
        assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_only_directory_is_online_but_not_writable() {
        use std::os::unix::fs::PermissionsExt;

        let fx = Fixture::new().await;
        if !permissions_are_enforced(fx.db.dir()) {
            return;
        }
        let root = fx.mkdir("frozen");
        fx.mkdir("open");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
        let volumes = fx.sync().await;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(names(&volumes), ["frozen", "open"]);
        assert!(volumes[0].online && !volumes[0].writable);
        assert!(volumes[1].online && volumes[1].writable);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_directory_is_offline() {
        use std::os::unix::fs::PermissionsExt;

        let fx = Fixture::new().await;
        if !permissions_are_enforced(fx.db.dir()) {
            return;
        }
        let root = fx.mkdir("locked");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
        let volumes = fx.sync().await;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(names(&volumes), ["locked"]);
        assert!(!volumes[0].online && !volumes[0].writable);
    }

    #[tokio::test]
    async fn a_missing_volumes_directory_means_no_volumes_and_keeps_known_rows_offline() {
        let fx = Fixture::new().await;
        fx.mkdir("photos");
        let before = fx.sync().await;

        fs::remove_dir_all(&fx.volumes_dir).unwrap();
        let after = fx.sync().await;
        assert_eq!(names(&after), ["photos"]);
        assert_eq!(after[0].id, before[0].id);
        assert!(!after[0].online && !after[0].writable);

        let empty = Fixture::new().await;
        fs::remove_dir_all(&empty.volumes_dir).unwrap();
        assert!(empty.sync().await.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn names_that_are_not_utf8_are_ignored() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let fx = Fixture::new().await;
        fx.mkdir("fine");
        fs::create_dir(fx.volumes_dir.join(OsStr::from_bytes(b"caf\xe9"))).unwrap();
        assert_eq!(names(&fx.sync().await), ["fine"]);
    }

    #[tokio::test]
    async fn list_orders_names_ignoring_ascii_case() {
        let fx = Fixture::new().await;
        for name in ["beta", "Alpha", "charlie", "Delta"] {
            fx.mkdir(name);
        }
        assert_eq!(
            names(&fx.sync().await),
            ["Alpha", "beta", "charlie", "Delta"]
        );
    }

    #[tokio::test]
    async fn concurrent_syncs_do_not_fail_or_duplicate_rows() {
        let fx = Fixture::new().await;
        fx.mkdir("a");
        fx.mkdir("b");
        let syncs = (0..6).map(|_| sync_volumes(&fx.db, &fx.volumes_dir));
        for result in futures_util::future::join_all(syncs).await {
            assert_eq!(result.unwrap().len(), 2);
        }
        assert_eq!(list(&fx.db).await.unwrap().len(), 2);
    }
}
