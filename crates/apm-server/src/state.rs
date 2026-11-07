//! Shared application state handed to every handler.
//!
//! Keep this file tiny and line-oriented: one field and one constructor step
//! per service, alphabetical in the struct. A service owner changes only the
//! constructor line of their own service (and the service's own module).

use std::sync::Arc;

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
    pub tasks: tasks::TaskEngine,
    pub trash: trash::TrashService,
}

impl AppState {
    /// Builds every service in dependency order. Constructors may be async and
    /// fallible (opening the database, running migrations, ...): add the
    /// dependencies you need as arguments on your own line.
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let config = Arc::new(config);

        let db = db::Db::new();

        let settings = settings::SettingsService::new();

        let tasks = tasks::TaskEngine::new();

        let fsops = fsops::FsOps::new();

        let indexer = indexer::Indexer::new();

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
            tasks,
            trash,
        })
    }

    /// Called synchronously when a termination signal arrives, before the HTTP
    /// server drains: cancel background work and end long-lived streams (SSE)
    /// so connections can close. Empty until a service needs it.
    pub fn begin_shutdown(&self) {}

    /// Called after the HTTP server has drained: await workers, flush journals
    /// and close the database. Empty until a service needs it.
    pub async fn shutdown(&self) {}
}
