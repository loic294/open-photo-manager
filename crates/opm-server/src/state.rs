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
    pub immich_outbox: immich::ImmichOutbox,
    pub immich_sync: immich::ImmichSync,
    pub index_jobs: indexer::IndexJobs,
    pub indexer: indexer::Indexer,
    pub media: media::MediaService,
    pub plans: plans::PlanService,
    pub settings: settings::SettingsService,
    /// Cancelled by `begin_shutdown`: background services and open streams stop on it.
    pub shutdown: CancellationToken,
    pub tasks: tasks::TaskEngine,
    pub trash: trash::TrashService,
    pub volumes: indexer::VolumeRegistry,
    pub watcher: indexer::watcher::Watcher,
}

impl AppState {
    /// Builds every service in dependency order. Constructors may be async and
    /// fallible (opening the database, running migrations, ...): add the
    /// dependencies you need as arguments on your own line.
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let config = Arc::new(config);

        let (db, volume_capabilities) = db::Db::from_config_with_capabilities(&config).await?;

        let settings = settings::SettingsService::load(db.clone(), &config).await?;

        let shutdown = CancellationToken::new();

        let tasks = tasks::TaskEngine::new(db.clone(), shutdown.clone()).await?;

        let volumes = indexer::VolumeRegistry::load_with_policy_and_capabilities(
            db.clone(),
            config.volumes_dir.clone(),
            config.watch_mode,
            volume_capabilities,
        )
        .await?;

        let media = media::MediaService::new(
            db.clone(),
            volumes.clone(),
            settings.clone(),
            config.data_dir.clone(),
        );

        let indexer = indexer::Indexer::new(db.clone(), settings.clone(), volumes.clone())
            .with_extractor(Arc::new(indexer::ExiftoolExtractor::new(
                "exiftool",
                config.data_dir.join("tmp").join("exiftool-home"),
                usize::from(settings.get().index.concurrency),
            )))
            .with_thumbnailer(Arc::new(media.clone()));

        let outbox_signal = immich::outbox::OutboxSignal::new();

        let immich = immich::ImmichService::with_move_database(
            db.clone(),
            config.data_dir.clone(),
            Arc::new(immich::direct_db::PostgresMoves::new(
                config.immich_database_url_file.clone(),
            )),
        );
        let immich_sync = immich::ImmichSync::new(db.clone(), immich.clone(), tasks.clone());
        let immich_outbox = immich::ImmichOutbox::new(
            db.clone(),
            immich.clone(),
            immich_sync.clone(),
            tasks.clone(),
            volumes.clone(),
            outbox_signal.clone(),
            config.immich_quiet_secs,
        );

        let fsops = fsops::FsOps::new(
            db.clone(),
            volumes.clone(),
            settings.clone(),
            indexer.clone(),
        )
        .with_hook(Arc::new(immich::outbox::OutboxHook::new(
            outbox_signal.clone(),
        )))
        .with_move_guard(Arc::new(immich_outbox.clone()));
        fsops.reconcile_at_startup(shutdown.clone()).await;

        let index_jobs = indexer::IndexJobs::new(
            indexer.clone(),
            tasks.clone(),
            fsops.clone(),
            shutdown.clone(),
        );
        let plans = plans::PlanService::new(plans::PlanDeps {
            db: db.clone(),
            fsops: fsops.clone(),
            settings: settings.clone(),
            shutdown: shutdown.clone(),
            tasks: tasks.clone(),
            volumes: volumes.clone(),
        })
        .await?;

        let trash = trash::TrashService::new(
            db.clone(),
            settings.clone(),
            tasks.clone(),
            volumes.clone(),
            fsops.clone(),
        );

        let watcher = indexer::watcher::Watcher::new(indexer::watcher::WatcherDeps {
            db: db.clone(),
            jobs: index_jobs.clone(),
            settings: settings.clone(),
            tasks: tasks.clone(),
            volumes: volumes.clone(),
            self_writes: fsops.self_writes().clone(),
            shutdown: shutdown.clone(),
        });

        let state = Self {
            config,
            db,
            fsops,
            immich,
            immich_outbox,
            immich_sync,
            index_jobs,
            indexer,
            media,
            plans,
            settings,
            shutdown,
            tasks,
            trash,
            volumes,
            watcher,
        };
        state.tasks.recover_services(&state).await?;
        Ok(state)
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
        self.watcher.shutdown().await;
        self.tasks.shutdown().await;
        self.plans.shutdown().await;
        // Keep last: every other service may still write while it stops.
        self.db.close().await;
    }
}
