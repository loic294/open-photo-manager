//! Refresh ISM-owned file rules and the local browse badge index.

use std::collections::BTreeSet;
use std::time::Duration;

use opm_core::dto::{FileSettings, FileSettingsSource, IsmInfoDto, ServerEvent};
use opm_core::paths::{RelPath, VolumeId};
use serde::Deserialize;
use sqlx::{QueryBuilder, Sqlite};
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use super::operations::{Event, EventParser};
use super::{IsmClient, IsmService, StoredConnection};
use crate::error::{AppError, AppResult};
use crate::settings::ManagedFiles;

const REFRESH: Duration = Duration::from_secs(300);
const RECONNECT: Duration = Duration::from_secs(3);

pub(super) fn supports(info: &IsmInfoDto, capability: &str) -> bool {
    info.capabilities.iter().any(|entry| entry == capability)
}

#[derive(Deserialize)]
struct SharedSettings {
    files: FileSettings,
    primary: Option<Primary>,
}

#[derive(Deserialize)]
struct Primary {
    name: String,
}

#[derive(Deserialize)]
struct AssetsPage {
    root_id: String,
    linked: bool,
    assets: Vec<Asset>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct Asset {
    path: String,
}

#[derive(Deserialize)]
struct AssetChanges {
    root_ids: Vec<String>,
}

#[derive(Deserialize)]
struct OperationChanges {
    changes: Vec<OperationChange>,
}

#[derive(Deserialize)]
struct OperationChange {
    from: Option<RootRef>,
    to: Option<RootRef>,
    location: Option<RootRef>,
}

#[derive(Deserialize)]
struct RootRef {
    root_id: String,
}

type Paths = BTreeSet<(String, String)>;

impl IsmService {
    pub fn start_background(&self, shutdown: CancellationToken) {
        let service = self.clone();
        let mut connection = self.connection_changed.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    result = connection.changed() => if result.is_err() { break; },
                    () = service.follow_mirror() => {}
                }
            }
        });
    }

    async fn follow_mirror(&self) {
        // Check before doing any network work and sleep until signalled if not configured.
        match self.stored().await {
            Ok(None) => return std::future::pending().await,
            Ok(Some(_)) => {}
            Err(error) => {
                tracing::warn!(code = error.code(), "could not load the ISM connection");
                time::sleep(RECONNECT).await;
                return;
            }
        }
        match self.connected().await {
            Ok((client, stored, info)) => {
                if supports(&info, "shared_file_settings") || supports(&info, "immich_assets") {
                    if let Err(error) = self.follow_events(&client, &stored, &info).await {
                        tracing::warn!(code = error.code(), "ISM event stream unavailable");
                        self.refresh(&client, &stored, &info).await;
                    }
                } else {
                    self.refresh(&client, &stored, &info).await;
                    time::sleep(REFRESH).await;
                }
            }
            Err(error) => {
                tracing::warn!(code = error.code(), "could not refresh the ISM connection")
            }
        }
        time::sleep(RECONNECT).await;
    }

    async fn follow_events(
        &self,
        client: &IsmClient,
        stored: &StoredConnection,
        info: &IsmInfoDto,
    ) -> AppResult<()> {
        let mut stream = client
            .http
            .get(
                client
                    .base_url
                    .join("api/v1/events")
                    .map_err(AppError::internal)?,
            )
            .header(reqwest::header::AUTHORIZATION, client.authorization.clone())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| unavailable())?;
        // Refresh after subscribing so changes committed during a paginated refresh are queued.
        self.refresh(client, stored, info).await;
        let until = Instant::now() + REFRESH;
        let mut parser = EventParser::default();
        loop {
            let chunk = tokio::select! {
                biased;
                () = time::sleep_until(until) => return Ok(()),
                result = stream.chunk() => result.map_err(|_| unavailable())?,
            };
            let Some(chunk) = chunk else { return Ok(()) };
            let events = parser.push(&chunk).ok_or_else(invalid_response)?;
            for event in events {
                if let Err(error) = self.on_event(client, stored, info, &event).await {
                    tracing::warn!(code = error.code(), "could not apply an ISM event");
                }
            }
        }
    }

    async fn on_event(
        &self,
        client: &IsmClient,
        stored: &StoredConnection,
        info: &IsmInfoDto,
        event: &Event,
    ) -> AppResult<()> {
        match event.name.as_str() {
            "settings_changed" if supports(info, "shared_file_settings") => {
                // Refetch the latest snapshot; queued events may predate the initial refresh.
                self.refresh_settings(client, stored, info).await?;
            }
            "assets_changed" if supports(info, "immich_assets") => {
                let change: AssetChanges =
                    serde_json::from_str(&event.data).map_err(|_| invalid_response())?;
                self.refresh_assets(client, stored, Some(change.root_ids.into_iter().collect()))
                    .await?;
            }
            "operation_committed" if supports(info, "immich_assets") => {
                let operation: OperationChanges =
                    serde_json::from_str(&event.data).map_err(|_| invalid_response())?;
                let roots = operation
                    .changes
                    .into_iter()
                    .flat_map(|change| {
                        [change.from, change.to, change.location]
                            .into_iter()
                            .flatten()
                    })
                    .map(|location| location.root_id)
                    .collect();
                self.refresh_assets(client, stored, Some(roots)).await?;
            }
            "resync" => self.refresh(client, stored, info).await,
            _ => {}
        }
        Ok(())
    }

    async fn refresh(&self, client: &IsmClient, stored: &StoredConnection, info: &IsmInfoDto) {
        if let Err(error) = self.refresh_settings(client, stored, info).await {
            tracing::warn!(code = error.code(), "could not refresh ISM file settings");
        }
        let assets = if supports(info, "immich_assets") {
            self.refresh_assets(client, stored, None).await
        } else {
            self.clear_assets(stored).await
        };
        if let Err(error) = assets {
            tracing::warn!(code = error.code(), "could not refresh ISM's Immich assets");
        }
    }

    async fn refresh_settings(
        &self,
        client: &IsmClient,
        stored: &StoredConnection,
        info: &IsmInfoDto,
    ) -> AppResult<()> {
        let result = if supports(info, "shared_file_settings") {
            let url = client
                .base_url
                .join("api/v1/client/settings")
                .map_err(AppError::internal)?;
            let bytes = client.get_bytes(url).await;
            bytes.and_then(|bytes| {
                let remote: SharedSettings =
                    serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
                let mut candidate = self.settings.defaults().clone();
                remote.files.apply(&mut candidate);
                let normalized = candidate.normalized().map_err(|_| invalid_response())?;
                Ok(Some(ManagedFiles {
                    source: FileSettingsSource {
                        instance_name: info.instance_name.clone(),
                        primary_name: remote.primary.map(|primary| primary.name),
                        ready: true,
                    },
                    files: Some(FileSettings::from_settings(&normalized)),
                }))
            })
        } else {
            Ok(None)
        };
        let _guard = self.gate.lock().await;
        if !self.still_connected(stored).await? {
            return Ok(());
        }
        let current = self.settings.get();
        let managed = match result {
            Ok(value) => value,
            Err(error) => {
                if current.file_settings_source.is_none() {
                    self.save_settings(Some(ManagedFiles::pending(&info.instance_name)))
                        .await?;
                }
                return Err(error);
            }
        };
        self.save_settings(managed).await
    }

    async fn save_settings(&self, managed: Option<ManagedFiles>) -> AppResult<()> {
        if self.settings.store_managed_files(managed).await? {
            self.tasks.publish(ServerEvent::SettingsChanged);
        }
        Ok(())
    }

    async fn still_connected(&self, stored: &StoredConnection) -> AppResult<bool> {
        Ok(self
            .stored()
            .await?
            .is_some_and(|active| active.token_enc == stored.token_enc))
    }

    async fn refresh_assets(
        &self,
        client: &IsmClient,
        stored: &StoredConnection,
        roots: Option<BTreeSet<String>>,
    ) -> AppResult<()> {
        let bindings: Vec<(VolumeId, String, String)> =
            sqlx::query_as("SELECT volume_id, root_id, root_prefix FROM ism_volume_roots")
                .fetch_all(self.db.read())
                .await?;
        for (volume_id, root, prefix) in bindings {
            if roots.as_ref().is_some_and(|roots| !roots.contains(&root)) {
                continue;
            }
            match client.asset_paths(&root, &prefix).await {
                Ok(paths) => {
                    self.replace_assets(stored, volume_id, &root, &prefix, paths)
                        .await?;
                }
                Err(error) => tracing::warn!(
                    volume_id,
                    code = error.code(),
                    "could not refresh ISM assets; keeping the previous snapshot"
                ),
            }
        }
        Ok(())
    }

    async fn clear_assets(&self, stored: &StoredConnection) -> AppResult<()> {
        let _guard = self.gate.lock().await;
        if self.still_connected(stored).await? {
            let affected: Vec<VolumeId> =
                sqlx::query_scalar("SELECT DISTINCT volume_id FROM ism_assets")
                    .fetch_all(self.db.read())
                    .await?;
            sqlx::query("DELETE FROM ism_assets")
                .execute(self.db.write())
                .await?;
            for volume_id in affected {
                self.tasks
                    .publish(ServerEvent::ImmichAssetsChanged { volume_id });
            }
        }
        Ok(())
    }

    async fn replace_assets(
        &self,
        stored: &StoredConnection,
        volume_id: VolumeId,
        root: &str,
        prefix: &str,
        paths: Paths,
    ) -> AppResult<()> {
        let _guard = self.gate.lock().await;
        if !self.still_connected(stored).await? {
            return Ok(());
        }
        let mut tx = self.db.begin_write().await?;
        let binding_matches: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ism_volume_roots
             WHERE volume_id = ? AND root_id = ? AND root_prefix = ?)",
        )
        .bind(volume_id)
        .bind(root)
        .bind(prefix)
        .fetch_one(&mut *tx)
        .await?;
        if !binding_matches {
            return Ok(());
        }
        let before: Paths =
            sqlx::query_as("SELECT dir_path, name FROM ism_assets WHERE volume_id = ?")
                .bind(volume_id)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .collect();
        if before == paths {
            return Ok(());
        }
        sqlx::query("DELETE FROM ism_assets WHERE volume_id = ?")
            .bind(volume_id)
            .execute(&mut *tx)
            .await?;
        for chunk in paths.into_iter().collect::<Vec<_>>().chunks(1_000) {
            QueryBuilder::<Sqlite>::new("INSERT INTO ism_assets (volume_id, dir_path, name) ")
                .push_values(chunk, |mut row, (dir, name)| {
                    row.push_bind(volume_id).push_bind(dir).push_bind(name);
                })
                .build()
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.tasks
            .publish(ServerEvent::ImmichAssetsChanged { volume_id });
        Ok(())
    }
}

impl IsmClient {
    async fn asset_paths(&self, root: &str, prefix: &str) -> AppResult<Paths> {
        let prefix = RelPath::new(prefix).map_err(|_| invalid_response())?;
        let mut paths = Paths::new();
        let mut after: Option<String> = None;
        loop {
            let mut url = self
                .base_url
                .join("api/v1/assets")
                .map_err(AppError::internal)?;
            url.query_pairs_mut()
                .append_pair("root_id", root)
                .append_pair("limit", "1000");
            if !prefix.is_root() {
                url.query_pairs_mut().append_pair("path", prefix.as_str());
            }
            if let Some(after) = &after {
                url.query_pairs_mut().append_pair("after", after);
            }
            let bytes = match self.get_bounded(url, 16 * 1024 * 1024).await {
                Ok(bytes) => bytes,
                Err(error) if error.status() == reqwest::StatusCode::FORBIDDEN => {
                    return Ok(Paths::new());
                }
                Err(error) => return Err(error),
            };
            let page: AssetsPage =
                serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
            if page.root_id != root {
                return Err(invalid_response());
            }
            if !page.linked {
                return Ok(Paths::new());
            }
            for asset in page.assets {
                let path = RelPath::new(&asset.path).map_err(|_| invalid_response())?;
                if let Some(relative) = path.strip_prefix(&prefix)
                    && let (Some(dir), Some(name)) = (relative.parent(), relative.file_name())
                {
                    paths.insert((dir.as_str().to_owned(), name.to_owned()));
                }
            }
            let Some(next) = page.next else {
                return Ok(paths);
            };
            if next.is_empty() || after.as_ref().is_some_and(|last| last >= &next) {
                return Err(invalid_response());
            }
            after = Some(next);
        }
    }
}

fn invalid_response() -> AppError {
    AppError::conflict("ism_invalid_response", "Invalid ISM response.")
}

fn unavailable() -> AppError {
    AppError::conflict("ism_unavailable", "Cannot read the ISM event stream.")
}
