//! Shared application state handed to every handler.
//!
//! Keep this file tiny and line-oriented: one field and one constructor step
//! per service, alphabetical in the struct. A service owner changes only the
//! constructor line of their own service (and the service's own module).

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::db;
use crate::fsops;
use crate::immich;
use crate::indexer;
use crate::media;
use crate::plans;
use crate::settings;
use crate::tasks;
use crate::trash;

/// Cheap-to-clone bundle of the configuration and every service. Handlers stay
/// thin and delegate to the service that owns the concern.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: db::Db,
    pub fsops: fsops::FsOps,
    pub immich: immich::ImmichService,
    pub indexer: indexer::Indexer,
    pub media: media::MediaService,
    pub plans: plans::PlanService,
    pub settings: settings::SettingsService,
    /// Cancelled by `begin_shutdown`: background services and open streams stop on it.
    pub shutdown: CancellationToken,
    pub tasks: tasks::TaskEngine,
    pub trash: trash::TrashService,
    pub volumes: indexer::VolumeRegistry,
}

impl AppState {
    /// Builds every service in dependency order. Constructors may be async and
    /// fallible (opening the database, running migrations, ...): add the
    /// dependencies you need as arguments on your own line.
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let config = Arc::new(config);

        let db = db::Db::from_config(&config).await?;

        let settings = settings::SettingsService::load(db.clone(), &config).await?;

        let shutdown = CancellationToken::new();

        let tasks = tasks::TaskEngine::new(db.clone(), shutdown.clone()).await?;

        let fsops = fsops::FsOps::new();

        let volumes = indexer::VolumeRegistry::load(db.clone(), config.volumes_dir.clone()).await?;

        let indexer = indexer::Indexer::new(db.clone(), settings.clone(), volumes.clone());

        let media = media::MediaService::new();

        let plans = plans::PlanService::new();

        let immich = immich::ImmichService::new();

        let trash = trash::TrashService::new();

        Ok(Self {
            config,
            db,
            fsops,
            immich,
            indexer,
            media,
            plans,
            settings,
            shutdown,
            tasks,
            trash,
            volumes,
        })
    }

    /// Called synchronously when a termination signal arrives, before the HTTP
    /// server drains: cancels `shutdown`, which asks every running task to stop
    /// and ends the open event streams so connections can close. Services that
    /// run their own loops watch the same token.
    pub fn begin_shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Called after the HTTP server has drained: await workers, flush journals
    /// and close the database. Await your own service's stop above the
    /// database line.
    pub async fn shutdown(&self) {
        self.tasks.shutdown().await;
        // Keep last: every other service may still write while it stops.
        self.db.close().await;
    }
}
