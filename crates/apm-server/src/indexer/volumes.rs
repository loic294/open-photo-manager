//! The volume registry: the one place where the indexer, the extractor, the hasher, the thumbnailer
//! and the file operations obtain the [`VolumeRoot`] of a volume.
//!
//! Every sub-directory of `APM_VOLUMES_DIR` is a volume (see [`crate::db::volumes`]). The registry
//! keeps an in-memory snapshot of the known volumes and, for every ONLINE one, a shared
//! [`VolumeRoot`] (the canonical root, built once). Lookups are synchronous and take a short read
//! lock, so any task can call them; nothing here holds a lock across an `await`.
//!
//! * [`VolumeRegistry::load`] builds the snapshot at startup from the `volumes` table
//!   (`Db::from_config` has already synchronised it with the directory).
//! * [`VolumeRegistry::refresh`] re-runs [`db::volumes::sync_volumes`] (picks up newly mounted shares,
//!   marks missing ones offline and re-probes writability). Filesystem capabilities are reused for
//!   an unchanged online root and re-probed when a root changes or comes back online.
//! * [`VolumeRegistry::get`] is the lookup: an unknown id is a 404 `not_found`, an offline volume a
//!   409 `volume_offline`.
//!
//! A volume is never forgotten: one whose share is unmounted stays in [`VolumeRegistry::list`] with
//! `online = false`, and its rows stay in the database, until it comes back.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::hostfs::VolumeCapabilities;
use apm_core::paths::VolumeId;

use crate::config::WatchPolicy;
use crate::db::models::Volume;
use crate::db::{self, Db, DbError};
use crate::error::AppError;
use crate::security::VolumeRoot;

/// What the registry knows about a volume id.
pub(crate) enum Lookup {
    Online(Arc<VolumeRoot>),
    /// A known volume whose root is missing or unreadable.
    Offline,
    Unknown,
}

/// Known volumes and the roots of the online ones. Cheap to clone (one `Arc`).
#[derive(Clone)]
pub struct VolumeRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    volumes_dir: PathBuf,
    watch_policy: WatchPolicy,
    snapshot: RwLock<Snapshot>,
    /// Serialises [`VolumeRegistry::refresh`]: two overlapping refreshes would probe the same
    /// directories twice and could publish their snapshots out of order.
    refreshing: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Snapshot {
    volumes: Vec<Volume>,
    roots: HashMap<VolumeId, Arc<VolumeRoot>>,
    capabilities: HashMap<VolumeId, VolumeCapabilities>,
}

impl fmt::Debug for VolumeRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = self.read();
        f.debug_struct("VolumeRegistry")
            .field("volumes_dir", &self.inner.volumes_dir)
            .field("volumes", &snapshot.volumes.len())
            .field("online", &snapshot.roots.len())
            .finish()
    }
}

impl VolumeRegistry {
    /// Builds the registry from the volumes already recorded in the database. `volumes_dir` is
    /// `APM_VOLUMES_DIR`, the directory [`refresh`](Self::refresh) looks at.
    pub async fn load(db: Db, volumes_dir: PathBuf) -> Result<Self, DbError> {
        Self::load_with_policy(db, volumes_dir, WatchPolicy::Auto).await
    }

    pub async fn load_with_policy(
        db: Db,
        volumes_dir: PathBuf,
        watch_policy: WatchPolicy,
    ) -> Result<Self, DbError> {
        Self::load_with_policy_and_capabilities(db, volumes_dir, watch_policy, HashMap::new()).await
    }

    pub(crate) async fn load_with_policy_and_capabilities(
        db: Db,
        volumes_dir: PathBuf,
        watch_policy: WatchPolicy,
        capabilities: HashMap<VolumeId, VolumeCapabilities>,
    ) -> Result<Self, DbError> {
        let volumes = db::volumes::list(&db).await?;
        let snapshot = build_snapshot(volumes, capabilities, watch_policy, HashMap::new()).await;
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                volumes_dir,
                watch_policy,
                snapshot: RwLock::new(snapshot),
                refreshing: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// Re-discovers the volumes ([`db::volumes::sync_volumes`]) and rebuilds the roots. Returns
    /// every known volume, ordered by name. A newly mounted share is registered, a missing one is
    /// marked offline (never forgotten).
    pub async fn refresh(&self) -> Result<Vec<Volume>, DbError> {
        let _one_at_a_time = self.inner.refreshing.lock().await;
        let (cached, previous) = cached_capabilities(&self.read());
        let (volumes, capabilities) = db::volumes::sync_volumes_with_cached_capabilities(
            &self.inner.db,
            &self.inner.volumes_dir,
            cached,
        )
        .await?;
        let snapshot =
            build_snapshot(volumes, capabilities, self.inner.watch_policy, previous).await;
        let volumes = snapshot.volumes.clone();
        *self.write() = snapshot;
        Ok(volumes)
    }

    /// The root of an online volume. Hold on to the returned `Arc` for the duration of one task;
    /// ask again for the next one, the volume may go offline in between.
    ///
    /// Fails with 404 `not_found` for an id that is not a known volume and with 409
    /// `volume_offline` for a known volume whose root is missing or unreadable.
    pub fn get(&self, id: VolumeId) -> Result<Arc<VolumeRoot>, AppError> {
        match self.lookup(id) {
            Lookup::Online(root) => Ok(root),
            Lookup::Offline => Err(AppError::conflict(
                "volume_offline",
                format!("volume {id} is offline"),
            )),
            Lookup::Unknown => Err(AppError::not_found(
                "not_found",
                format!("no volume with id {id}"),
            )),
        }
    }

    /// [`get`](Self::get) without the error mapping, for callers that treat an offline volume
    /// differently from an unknown one.
    pub(crate) fn lookup(&self, id: VolumeId) -> Lookup {
        let snapshot = self.read();
        if let Some(root) = snapshot.roots.get(&id) {
            Lookup::Online(Arc::clone(root))
        } else if snapshot.volumes.iter().any(|volume| volume.id == id) {
            Lookup::Offline
        } else {
            Lookup::Unknown
        }
    }

    /// Every known volume, online or not, ordered by name.
    pub fn list(&self) -> Vec<Volume> {
        self.read().volumes.clone()
    }

    /// One known volume (online or not): its name, whether it is writable and online.
    pub fn volume(&self, id: VolumeId) -> Option<Volume> {
        self.read()
            .volumes
            .iter()
            .find(|volume| volume.id == id)
            .cloned()
    }

    /// The online volumes, ordered by name.
    pub fn online(&self) -> Vec<Volume> {
        self.read()
            .volumes
            .iter()
            .filter(|volume| volume.online)
            .cloned()
            .collect()
    }

    pub fn capabilities(&self, id: VolumeId) -> Option<VolumeCapabilities> {
        self.read().capabilities.get(&id).cloned()
    }

    pub fn watch_mode(
        &self,
        id: VolumeId,
        enabled: bool,
    ) -> (apm_core::dto::WatchMode, Option<String>) {
        let fs_type = self
            .capabilities(id)
            .and_then(|capabilities| capabilities.fs_type);
        crate::hostfs::effective_watch_mode(self.inner.watch_policy, fs_type.as_deref(), enabled)
    }

    fn read(&self) -> RwLockReadGuard<'_, Snapshot> {
        self.inner
            .snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Snapshot> {
        self.inner
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Builds the roots of the online volumes (canonicalising touches the file system, so off the
/// async threads). A volume whose root cannot be opened after all is reported offline.
async fn build_snapshot(
    volumes: Vec<Volume>,
    capabilities: HashMap<VolumeId, VolumeCapabilities>,
    watch_policy: WatchPolicy,
    previous: HashMap<VolumeId, (String, VolumeCapabilities)>,
) -> Snapshot {
    let built = tokio::task::spawn_blocking(move || {
        let mut roots = HashMap::new();
        let mut volumes = volumes;
        let mut capabilities = capabilities;
        for volume in &mut volumes {
            if !volume.online {
                continue;
            }
            match VolumeRoot::new(volume.id, &volume.root) {
                Ok(root) => {
                    capabilities.entry(volume.id).or_insert_with(|| {
                        crate::hostfs::probe::probe_volume(root.path()).capabilities
                    });
                    roots.insert(volume.id, Arc::new(root));
                }
                Err(err) => {
                    tracing::warn!(volume = %volume.name, error = %err, "volume root is not usable, treating it as offline");
                    volume.online = false;
                    volume.writable = false;
                }
            }
        }
        Snapshot {
            volumes,
            roots,
            capabilities,
        }
    })
    .await;
    let snapshot = built.unwrap_or_else(|err| {
        tracing::error!(error = %err, "building the volume roots failed, every volume is offline");
        Snapshot::default()
    });
    for volume in snapshot.volumes.iter().filter(|volume| volume.online) {
        let capability = snapshot.capabilities.get(&volume.id);
        if previous
            .get(&volume.id)
            .is_some_and(|(root, old)| root == &volume.root && Some(old) == capability)
        {
            continue;
        }
        let (mode, reason) = crate::hostfs::effective_watch_mode(
            watch_policy,
            capability.and_then(|cap| cap.fs_type.as_deref()),
            true,
        );
        tracing::info!(
            volume = %volume.name,
            fs_type = capability.and_then(|cap| cap.fs_type.as_deref()).unwrap_or("unknown"),
            case_insensitive = ?capability.and_then(|cap| cap.case_insensitive),
            watch_mode = ?mode,
            reason = reason.as_deref().unwrap_or("native filesystem event support"),
            "volume capabilities computed"
        );
        if capability.is_some_and(|cap| cap.inode_reliable == Some(false)) {
            tracing::warn!(volume = %volume.name, "filesystem inode identity is unreliable");
        }
    }
    snapshot
}

fn cached_capabilities(
    snapshot: &Snapshot,
) -> (
    HashMap<String, VolumeCapabilities>,
    HashMap<VolumeId, (String, VolumeCapabilities)>,
) {
    let mut cached = HashMap::new();
    let mut previous = HashMap::new();
    for volume in snapshot
        .volumes
        .iter()
        .filter(|volume| snapshot.roots.contains_key(&volume.id))
    {
        if let Some(capabilities) = snapshot.capabilities.get(&volume.id) {
            cached.insert(volume.root.clone(), capabilities.clone());
            previous.insert(volume.id, (volume.root.clone(), capabilities.clone()));
        }
    }
    (cached, previous)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::db::TempDb;

    struct Fixture {
        _db: TempDb,
        volumes_dir: PathBuf,
        registry: VolumeRegistry,
    }

    impl Fixture {
        async fn new(volumes: &[&str]) -> Self {
            let db = Db::open_temp().await.unwrap();
            let volumes_dir = db.dir().join("volumes");
            fs::create_dir_all(&volumes_dir).unwrap();
            for name in volumes {
                fs::create_dir(volumes_dir.join(name)).unwrap();
            }
            let handle = db.handle();
            db::volumes::sync_volumes(&handle, &volumes_dir)
                .await
                .unwrap();
            let registry = VolumeRegistry::load(handle, volumes_dir.clone())
                .await
                .unwrap();
            Self {
                _db: db,
                volumes_dir,
                registry,
            }
        }

        fn id_of(&self, name: &str) -> VolumeId {
            self.registry
                .list()
                .into_iter()
                .find(|volume| volume.name == name)
                .unwrap()
                .id
        }
    }

    #[tokio::test]
    async fn load_knows_the_online_volumes_and_hands_out_their_roots() {
        let fx = Fixture::new(&["photos", "scans"]).await;
        let names: Vec<_> = fx.registry.list().into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["photos", "scans"]);
        assert_eq!(fx.registry.online().len(), 2);

        let id = fx.id_of("photos");
        let root = fx.registry.get(id).unwrap();
        assert_eq!(root.id(), id);
        assert_eq!(
            root.path(),
            fs::canonicalize(fx.volumes_dir.join("photos")).unwrap()
        );
        assert_eq!(fx.registry.volume(id).unwrap().name, "photos");
    }

    #[tokio::test]
    async fn an_unknown_volume_is_not_found() {
        let fx = Fixture::new(&["photos"]).await;
        let err = fx.registry.get(9999).unwrap_err();
        assert_eq!(err.status().as_u16(), 404);
        assert_eq!(err.code(), "not_found");
        assert!(fx.registry.volume(9999).is_none());
    }

    #[tokio::test]
    async fn refresh_picks_up_new_volumes_and_keeps_vanished_ones_offline() {
        let fx = Fixture::new(&["photos", "scans"]).await;
        let scans = fx.id_of("scans");

        fs::create_dir(fx.volumes_dir.join("archive")).unwrap();
        fs::remove_dir(fx.volumes_dir.join("scans")).unwrap();
        let volumes = fx.registry.refresh().await.unwrap();

        assert_eq!(volumes.len(), 3);
        let archive = fx.id_of("archive");
        assert!(fx.registry.get(archive).is_ok());

        let err = fx.registry.get(scans).unwrap_err();
        assert_eq!(err.status().as_u16(), 409);
        assert_eq!(err.code(), "volume_offline");
        let known = fx.registry.volume(scans).unwrap();
        assert!(!known.online && !known.writable);
        let online: Vec<_> = fx.registry.online().into_iter().map(|v| v.name).collect();
        assert_eq!(online, ["archive", "photos"]);

        fs::create_dir(fx.volumes_dir.join("scans")).unwrap();
        fx.registry.refresh().await.unwrap();
        assert_eq!(fx.id_of("scans"), scans, "a volume keeps its id");
        assert!(fx.registry.get(scans).is_ok());
    }

    #[tokio::test]
    async fn clones_share_one_snapshot() {
        let fx = Fixture::new(&["photos"]).await;
        let other = fx.registry.clone();
        fs::create_dir(fx.volumes_dir.join("late")).unwrap();
        fx.registry.refresh().await.unwrap();
        assert_eq!(other.list().len(), 2);
    }
}
