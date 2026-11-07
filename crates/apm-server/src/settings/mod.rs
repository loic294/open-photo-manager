//! Typed application settings: one JSON document in the `settings` table, a cheap in-memory
//! snapshot for readers, and a `watch` channel that announces changes.
//!
//! Owned by todo `designing-database-schema`. [`Settings`] is `apm_core::dto::SettingsDto`, the
//! body of `GET /api/settings`; its validation rules and its lenient loading live next to it in
//! `apm_core::dto::settings` (pure, no IO). This service adds persistence and notification. Other
//! modules read settings through it, never from the table:
//!
//! * [`SettingsService::get`]: the current snapshot, an `Arc` (cloning it is free). Take one per
//!   operation (a scan, a plan) instead of reading a field at a time, so the operation sees one
//!   consistent document.
//! * [`SettingsService::subscribe`]: a `watch::Receiver` for services that must react to a change
//!   (the watcher, the junk rules). The receiver starts out having seen the current value:
//!   `changed().await` completes at the next update, `borrow()` is the current document.
//! * [`SettingsService::update`]: validates, normalises, stores and publishes a full document.
//!
//! The stored document holds only what a user saved: nothing is written at startup. Until then the
//! defaults apply, and they follow the environment (`APM_INDEX_CONCURRENCY`, the CPU count). A
//! stored document is laid over the defaults key by key, so an upgrade that adds a setting, or a
//! stored value that no longer validates, never stops the server from starting (see
//! `SettingsDto::from_stored`).

use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;

use anyhow::Context as _;
use apm_core::dto::{FieldError, SettingsDto, ValidationErrorBody};
use axum::http::StatusCode;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{Mutex, watch};

use crate::config::Config;
use crate::db::{Db, DbError, now_secs};
use crate::error::AppError;

/// The settings document the rest of the server reads.
pub type Settings = SettingsDto;

/// Key of the settings document in the `settings` table.
pub const SETTINGS_KEY: &str = "app";

/// The settings of the server. Cheap to clone; every clone shares the same state.
#[derive(Clone, Debug)]
pub struct SettingsService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    db: Db,
    defaults: Settings,
    current: watch::Sender<Arc<Settings>>,
    /// Held while a document is stored and published, so that racing updates can not leave the
    /// table and the snapshot holding different documents.
    update_lock: Mutex<()>,
}

impl SettingsService {
    /// Reads the stored document (if any) and builds the service. A stored document that is
    /// damaged is repaired in memory and reported in the log; only a failing database is an error.
    pub async fn load(db: Db, config: &Config) -> anyhow::Result<Self> {
        let cpus = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        let defaults = Settings::defaults_for(config.index_concurrency, cpus);

        let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(SETTINGS_KEY)
            .fetch_optional(db.read())
            .await
            .context("cannot read the stored settings")?;
        let settings = match stored {
            None => defaults.clone(),
            Some(text) => match serde_json::from_str::<Value>(&text) {
                Ok(document) => {
                    let loaded = Settings::from_stored(&document, &defaults);
                    for issue in &loaded.issues {
                        tracing::warn!(
                            field = %issue.field,
                            problem = %issue.message,
                            "ignored a stored setting"
                        );
                    }
                    loaded.settings
                }
                Err(err) => {
                    tracing::warn!(error = %err, "the stored settings are not JSON, using the defaults");
                    defaults.clone()
                }
            },
        };

        let (current, _) = watch::channel(Arc::new(settings));
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                defaults,
                current,
                update_lock: Mutex::new(()),
            }),
        })
    }

    /// The current settings.
    pub fn get(&self) -> Arc<Settings> {
        Arc::clone(&self.inner.current.borrow())
    }

    /// A receiver that is notified when [`update`](Self::update) changes the settings.
    pub fn subscribe(&self) -> watch::Receiver<Arc<Settings>> {
        self.inner.current.subscribe()
    }

    /// The factory settings of this installation (for a "reset" button).
    pub fn defaults(&self) -> &Settings {
        &self.inner.defaults
    }

    /// Validates and normalises `proposed`, stores it and publishes it to every subscriber; returns
    /// the normalised document. Nothing is stored or published when validation fails. Saving a
    /// document equal to the current one stores it again but notifies nobody.
    ///
    /// Storing and publishing run in their own task, so a caller that is dropped halfway (a client
    /// that hangs up) can not leave the two out of step.
    pub async fn update(&self, proposed: Settings) -> Result<Arc<Settings>, SettingsError> {
        let settings = proposed.normalized().map_err(ValidationError::new)?;
        let document = serde_json::to_string(&settings)?;
        let service = self.clone();
        tokio::spawn(async move { service.store_and_publish(settings, &document).await })
            .await
            .map_err(|err| DbError::Setup(format!("the settings update was interrupted: {err}")))?
    }

    async fn store_and_publish(
        &self,
        settings: Settings,
        document: &str,
    ) -> Result<Arc<Settings>, SettingsError> {
        let _one_update_at_a_time = self.inner.update_lock.lock().await;
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(SETTINGS_KEY)
        .bind(document)
        .bind(now_secs())
        .execute(self.inner.db.write())
        .await
        .map_err(DbError::from)?;

        let published = Arc::new(settings);
        self.inner.current.send_if_modified(|current| {
            if **current == *published {
                false
            } else {
                *current = Arc::clone(&published);
                true
            }
        });
        Ok(published)
    }
}

/// The rejected fields of a settings document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    pub fields: Vec<FieldError>,
}

impl ValidationError {
    fn new(fields: Vec<FieldError>) -> Self {
        Self { fields }
    }

    /// Every field message in one line.
    pub fn message(&self) -> String {
        ValidationErrorBody::new(self.fields.clone()).error.message
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid settings: {}", self.message())
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error(transparent)]
    Invalid(#[from] ValidationError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("cannot encode the settings: {0}")]
    Encode(#[from] serde_json::Error),
}

impl From<SettingsError> for AppError {
    /// A rejected document is a 422 `validation_failed` (the HTTP handler adds the per-field list);
    /// the rest follows the mapping of [`DbError`].
    fn from(err: SettingsError) -> Self {
        match err {
            SettingsError::Invalid(err) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ValidationErrorBody::CODE,
                err.message(),
            ),
            SettingsError::Db(err) => err.into(),
            SettingsError::Encode(err) => Self::internal(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use apm_core::dto::ConflictPolicy;
    use serde_json::json;

    use super::*;

    async fn service(db: &Db) -> SettingsService {
        let config = Config::for_tests(std::path::Path::new("/unused"));
        SettingsService::load(db.clone(), &config).await.unwrap()
    }

    async fn store(db: &Db, document: &str) {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES ('app', ?, 1) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(document)
        .execute(db.write())
        .await
        .unwrap();
    }

    async fn stored(db: &Db) -> Option<String> {
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'app'")
            .fetch_optional(db.read())
            .await
            .unwrap()
    }

    fn fields(err: &SettingsError) -> Vec<&str> {
        match err {
            SettingsError::Invalid(err) => err.fields.iter().map(|f| f.field.as_str()).collect(),
            other => panic!("not a validation error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_new_installation_runs_on_the_defaults_and_stores_nothing() {
        let db = Db::open_temp().await.unwrap();
        let settings = service(&db).await;

        let cpus = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        assert_eq!(*settings.get(), Settings::defaults_for(1, cpus));
        assert_eq!(settings.get().index.concurrency, 1);
        assert_eq!(*settings.get(), *settings.defaults());
        assert_eq!(stored(&db).await, None);
    }

    #[tokio::test]
    async fn the_defaults_follow_the_operators_index_concurrency() {
        let db = Db::open_temp().await.unwrap();
        let mut config = Config::for_tests(db.dir());
        config.index_concurrency = 6;
        let settings = SettingsService::load(db.handle(), &config).await.unwrap();
        assert_eq!(settings.get().index.concurrency, 6);
    }

    #[tokio::test]
    async fn an_update_is_normalised_stored_and_returned() {
        let db = Db::open_temp().await.unwrap();
        let settings = service(&db).await;
        let mut proposed = settings.get().as_ref().clone();
        proposed.sidecar_extensions = vec![".XMP".into(), "xmp".into(), " Dop ".into()];
        proposed.name_max_len = 143;
        proposed.default_conflict_policy = ConflictPolicy::Skip;
        proposed.trash_retention_days = Some(30);
        proposed.index.concurrency = 3;

        let saved = settings.update(proposed).await.unwrap();

        assert_eq!(saved.sidecar_extensions, ["xmp", "dop"]);
        assert_eq!(*settings.get(), *saved);
        assert_eq!(settings.get().name_max_len, 143);
        let document: Value = serde_json::from_str(&stored(&db).await.unwrap()).unwrap();
        assert_eq!(document["sidecar_extensions"], json!(["xmp", "dop"]));
        assert_eq!(document["default_conflict_policy"], json!("skip"));
        assert_eq!(document["trash_retention_days"], json!(30));
        assert_eq!(document["index"]["concurrency"], json!(3));

        // A restart finds the same settings.
        let reloaded = service(&db).await;
        assert_eq!(*reloaded.get(), *saved);
        // Saving the stored document again changes nothing.
        let again = reloaded.update(saved.as_ref().clone()).await.unwrap();
        assert_eq!(*again, *saved);
    }

    #[tokio::test]
    async fn a_rejected_update_reports_every_field_and_changes_nothing() {
        let db = Db::open_temp().await.unwrap();
        let settings = service(&db).await;
        let before = settings.get();
        let receiver = settings.subscribe();
        let mut proposed = before.as_ref().clone();
        proposed.name_max_len = 3;
        proposed.index.concurrency = 0;
        proposed.sidecar_extensions = vec!["fine".into(), "not valid!".into()];

        let err = settings.update(proposed).await.unwrap_err();

        let mut rejected = fields(&err);
        rejected.sort_unstable();
        assert_eq!(
            rejected,
            ["index.concurrency", "name_max_len", "sidecar_extensions[1]"]
        );
        assert!(err.to_string().contains("name_max_len"), "{err}");
        assert_eq!(settings.get(), before);
        assert_eq!(stored(&db).await, None);
        assert!(!receiver.has_changed().unwrap());

        let app_error = AppError::from(err);
        assert_eq!(app_error.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(app_error.code(), "validation_failed");
    }

    #[tokio::test]
    async fn subscribers_see_changes_but_not_repeats() {
        let db = Db::open_temp().await.unwrap();
        let settings = service(&db).await;
        let mut receiver = settings.subscribe();
        assert!(!receiver.has_changed().unwrap(), "starts out up to date");

        let mut proposed = settings.get().as_ref().clone();
        proposed.junk_names.push("scratch".into());
        let saved = settings.update(proposed.clone()).await.unwrap();

        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .expect("the change is announced")
            .unwrap();
        assert_eq!(*receiver.borrow_and_update(), saved);
        assert!(receiver.borrow().junk_names.contains(&"scratch".to_owned()));

        settings.update(proposed).await.unwrap();
        assert!(
            !receiver.has_changed().unwrap(),
            "an identical document is not a change"
        );

        // A subscriber created later starts from the current document.
        let late = settings.subscribe();
        assert_eq!(*late.borrow(), saved);
    }

    #[tokio::test]
    async fn a_partial_stored_document_is_completed_with_the_defaults() {
        let db = Db::open_temp().await.unwrap();
        store(
            &db,
            r#"{"name_max_len": 143, "index": {"concurrency": 4}, "keep_cross_volume_source_in_trash": true}"#,
        )
        .await;
        let settings = service(&db).await;

        let defaults = settings.defaults();
        let current = settings.get();
        assert_eq!(current.name_max_len, 143);
        assert_eq!(current.index.concurrency, 4);
        assert!(current.keep_cross_volume_source_in_trash);
        assert_eq!(current.sidecar_extensions, defaults.sidecar_extensions);
        assert_eq!(current.junk_names, defaults.junk_names);
        assert_eq!(
            current.index.hash_concurrency,
            defaults.index.hash_concurrency
        );
        assert_eq!(
            current.index.rescan_interval_minutes,
            defaults.index.rescan_interval_minutes
        );
        assert_eq!(
            current.default_conflict_policy,
            defaults.default_conflict_policy
        );
    }

    #[tokio::test]
    async fn damaged_stored_documents_fall_back_to_the_defaults() {
        let db = Db::open_temp().await.unwrap();
        let defaults = service(&db).await.defaults().clone();

        for document in [
            "null",
            "[]",
            "42",
            r#""text""#,
            "{}",
            r#"{"unknown": true, "name_max_len": "wide", "sidecar_extensions": ["x y"], "index": 3}"#,
        ] {
            store(&db, document).await;
            let settings = service(&db).await;
            assert_eq!(*settings.get(), defaults, "{document}");
        }

        // One bad value does not take the good ones down with it.
        store(&db, r#"{"name_max_len": 2, "trash_retention_days": 14}"#).await;
        let settings = service(&db).await;
        assert_eq!(settings.get().name_max_len, defaults.name_max_len);
        assert_eq!(settings.get().trash_retention_days, Some(14));
    }

    #[tokio::test]
    async fn concurrent_updates_leave_the_table_and_the_snapshot_in_step() {
        let db = Db::open_temp().await.unwrap();
        let settings = service(&db).await;
        let mut tasks = Vec::new();
        for len in 100..120_u16 {
            let settings = settings.clone();
            tasks.push(tokio::spawn(async move {
                let mut proposed = settings.get().as_ref().clone();
                proposed.name_max_len = len;
                settings.update(proposed).await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let document: Value = serde_json::from_str(&stored(&db).await.unwrap()).unwrap();
        assert_eq!(document["name_max_len"], json!(settings.get().name_max_len));
        assert_eq!(*service(&db).await.get(), *settings.get());
    }

    #[tokio::test]
    async fn a_failing_database_is_reported_and_the_snapshot_keeps_its_value() {
        let db = Db::open_temp().await.unwrap();
        let settings = service(&db).await;
        let before = settings.get();
        db.close().await;

        let mut proposed = before.as_ref().clone();
        proposed.name_max_len = 200;
        let err = settings.update(proposed).await.unwrap_err();

        assert!(matches!(err, SettingsError::Db(_)), "{err:?}");
        assert_eq!(settings.get(), before);
        let app_error = AppError::from(err);
        assert_eq!(app_error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn loading_fails_with_context_when_the_database_cannot_be_read() {
        let db = Db::open_temp().await.unwrap();
        db.close().await;
        let config = Config::for_tests(std::path::Path::new("/unused"));
        let err = SettingsService::load(db.handle(), &config)
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("cannot read the stored settings"),
            "{err:#}"
        );
    }
}
