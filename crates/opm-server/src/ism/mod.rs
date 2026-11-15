//! The sync manager (ISM): authenticated discovery, the encrypted connection, volume bindings,
//! and the operations client that runs the file changes of bound volumes (`operations`).

mod operations;
pub(crate) mod wire;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opm_core::dto::{
    IsmBindingDto, IsmBindingRequest, IsmConnectRequest, IsmConnectionDto, IsmInfoDto,
    IsmStagedBindingDto, IsmStatusDto,
};
use opm_core::paths::{RelPath, VolumeId};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use tokio::sync::Mutex;
use url::Url;

use crate::db::{Db, now_secs};
use crate::error::{AppError, AppResult};
use crate::immich::crypto::SecretBox;

const PURPOSE: &[u8] = b"open-photo-manager/ism-client-token/v1";
const MAX_INFO_BYTES: usize = 256 * 1024;
/// Every request but the event stream.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// ISM sends a keep-alive comment every 15 s on its event streams.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct IsmService {
    db: Db,
    data_dir: PathBuf,
    gate: Arc<Mutex<()>>,
}

#[derive(sqlx::FromRow)]
struct StoredConnection {
    base_url: String,
    token_enc: String,
    instance_id: String,
    instance_name: String,
}

impl IsmService {
    pub fn new(db: Db, data_dir: PathBuf) -> Self {
        Self {
            db,
            data_dir,
            gate: Arc::new(Mutex::new(())),
        }
    }

    async fn stored(&self) -> AppResult<Option<StoredConnection>> {
        Ok(sqlx::query_as(
            "SELECT base_url, token_enc, instance_id, instance_name FROM ism_connection WHERE id = 1",
        )
        .fetch_optional(self.db.read())
        .await?)
    }

    pub async fn status(&self) -> AppResult<IsmStatusDto> {
        let connection = self.stored().await?.map(|stored| IsmConnectionDto {
            base_url: stored.base_url,
            instance_id: stored.instance_id,
            instance_name: stored.instance_name,
        });
        let bindings: Vec<(VolumeId, String, String, String)> = sqlx::query_as(
            "SELECT volume_id, root_id, root_name, root_prefix FROM ism_volume_roots
             ORDER BY volume_id",
        )
        .fetch_all(self.db.read())
        .await?;
        let staged: Vec<(VolumeId, Option<String>, String, Option<String>)> = sqlx::query_as(
            "SELECT volume_id, root_id, root_name, error FROM ism_binding_changes
             ORDER BY volume_id",
        )
        .fetch_all(self.db.read())
        .await?;
        Ok(IsmStatusDto {
            connection,
            bound_volumes: i64::try_from(bindings.len()).unwrap_or(i64::MAX),
            bindings: bindings
                .into_iter()
                .map(
                    |(volume_id, root_id, root_name, root_prefix)| IsmBindingDto {
                        volume_id,
                        root_id,
                        root_name,
                        root_prefix,
                    },
                )
                .collect(),
            staged: staged
                .into_iter()
                .map(
                    |(volume_id, root_id, root_name, error)| IsmStagedBindingDto {
                        volume_id,
                        root_id,
                        root_name,
                        error,
                    },
                )
                .collect(),
        })
    }

    /// Bound volumes, and binds waiting for the next start.
    async fn has_bindings(&self) -> AppResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ism_volume_roots)
                 OR EXISTS(SELECT 1 FROM ism_binding_changes WHERE root_id IS NOT NULL)",
        )
        .fetch_one(self.db.read())
        .await?)
    }

    /// The stored connection and its client, checked against the instance it was made with.
    async fn connected(&self) -> AppResult<(IsmClient, StoredConnection, IsmInfoDto)> {
        let stored = self
            .stored()
            .await?
            .ok_or_else(|| AppError::conflict("ism_not_connected", "ISM is not connected."))?;
        let token = SecretBox::load(&self.data_dir)
            .and_then(|secrets| secrets.decrypt_for(&stored.token_enc, PURPOSE))
            .map_err(|_| {
                AppError::conflict(
                    "ism_needs_reconnect",
                    "The ISM token cannot be read; reconnect.",
                )
            })?;
        let client = IsmClient::new(&stored.base_url, token.expose())?;
        let info = client.info().await?;
        if info.instance_id != stored.instance_id {
            return Err(AppError::conflict(
                "ism_instance_changed",
                "The ISM server no longer identifies as the configured instance.",
            ));
        }
        Ok((client, stored, info))
    }

    /// What `info` says now and the recoverable items of `root_id` in the ISM trash.
    pub(crate) async fn trash_entries(
        &self,
        root_id: &str,
    ) -> AppResult<(IsmInfoDto, Vec<wire::TrashEntry>)> {
        let (client, _, info) = self.connected().await?;
        let entries = client.trash_list(root_id).await?;
        Ok((info, entries))
    }

    /// Stages a bind or an unbind; it takes effect at the next start, before any file operation
    /// can run, so an operation never sees a volume change hands halfway.
    pub async fn stage_binding(&self, request: IsmBindingRequest) -> AppResult<IsmStatusDto> {
        let _guard = self.gate.lock().await;
        let volume_root: Option<String> =
            sqlx::query_scalar("SELECT root FROM volumes WHERE id = ?")
                .bind(request.volume_id)
                .fetch_optional(self.db.read())
                .await?;
        let volume_root = volume_root
            .ok_or_else(|| AppError::not_found("volume_not_found", "No such volume."))?;
        let current: Option<(String, String)> =
            sqlx::query_as("SELECT root_id, root_prefix FROM ism_volume_roots WHERE volume_id = ?")
                .bind(request.volume_id)
                .fetch_optional(self.db.read())
                .await?;
        let Some(root_id) = request.root_id else {
            if current.is_some() {
                sqlx::query(
                    "INSERT INTO ism_binding_changes (volume_id, root_id, requested_at)
                     VALUES (?, NULL, ?)
                     ON CONFLICT(volume_id) DO UPDATE SET root_id = NULL, root_name = '',
                         root_prefix = '', error = NULL, requested_at = excluded.requested_at",
                )
                .bind(request.volume_id)
                .bind(now_secs())
                .execute(self.db.write())
                .await?;
            } else {
                sqlx::query("DELETE FROM ism_binding_changes WHERE volume_id = ?")
                    .bind(request.volume_id)
                    .execute(self.db.write())
                    .await?;
            }
            return self.status().await;
        };
        let (_, _, info) = self.connected().await?;
        let root = info
            .roots
            .iter()
            .find(|root| root.root_id == root_id)
            .ok_or_else(|| AppError::conflict("ism_root_unknown", "ISM has no such root."))?;
        let opm_prefix = root.opm_prefix.as_deref().ok_or_else(|| {
            AppError::conflict(
                "ism_root_without_prefix",
                "This ISM root has no OPM path configured.",
            )
        })?;
        let prefix =
            prefix_within(Path::new(opm_prefix), Path::new(&volume_root)).ok_or_else(|| {
                AppError::conflict(
                    "ism_volume_outside_root",
                    "The volume is not inside the folder ISM manages for this root.",
                )
            })?;
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ism_volume_roots WHERE root_id = ?1 AND volume_id <> ?2
                 AND volume_id NOT IN
                     (SELECT volume_id FROM ism_binding_changes WHERE root_id IS NULL))
                 OR EXISTS(SELECT 1 FROM ism_binding_changes WHERE root_id = ?1 AND volume_id <> ?2)",
        )
        .bind(&root_id)
        .bind(request.volume_id)
        .fetch_one(self.db.read())
        .await?;
        if taken {
            return Err(AppError::conflict(
                "ism_root_taken",
                "Another volume is bound to this ISM root.",
            ));
        }
        if current.as_ref() == Some(&(root_id.clone(), prefix.clone())) {
            sqlx::query("DELETE FROM ism_binding_changes WHERE volume_id = ?")
                .bind(request.volume_id)
                .execute(self.db.write())
                .await?;
        } else {
            sqlx::query(
                "INSERT INTO ism_binding_changes
                     (volume_id, root_id, root_name, root_prefix, requested_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(volume_id) DO UPDATE SET root_id = excluded.root_id,
                     root_name = excluded.root_name, root_prefix = excluded.root_prefix,
                     error = NULL, requested_at = excluded.requested_at",
            )
            .bind(request.volume_id)
            .bind(&root_id)
            .bind(&root.name)
            .bind(&prefix)
            .bind(now_secs())
            .execute(self.db.write())
            .await?;
        }
        self.status().await
    }

    pub async fn connect(&self, input: IsmConnectRequest) -> AppResult<IsmInfoDto> {
        let _guard = self.gate.lock().await;
        let client = IsmClient::new(&input.base_url, &input.token)?;
        let info = client.info().await?;
        let previous = self.stored().await?;
        let bound = self.has_bindings().await?;
        if bound && previous.as_ref().map(|old| &old.instance_id) != Some(&info.instance_id) {
            return Err(AppError::conflict(
                "ism_bindings_exist",
                "Cannot change the ISM instance while volumes are bound.",
            ));
        }
        let secrets = SecretBox::load_or_create(&self.data_dir).map_err(AppError::internal)?;
        let encrypted = secrets
            .encrypt_for(&input.token, PURPOSE)
            .map_err(AppError::internal)?;
        sqlx::query(
            "INSERT INTO ism_connection (id, base_url, token_enc, instance_id, instance_name)
             VALUES (1, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET base_url = excluded.base_url,
                 token_enc = excluded.token_enc, instance_id = excluded.instance_id,
                 instance_name = excluded.instance_name",
        )
        .bind(client.base_url.as_str())
        .bind(encrypted)
        .bind(&info.instance_id)
        .bind(&info.instance_name)
        .execute(self.db.write())
        .await?;
        Ok(info)
    }

    pub async fn test(&self) -> AppResult<IsmInfoDto> {
        let _guard = self.gate.lock().await;
        Ok(self.connected().await?.2)
    }

    pub async fn disconnect(&self) -> AppResult<IsmStatusDto> {
        let _guard = self.gate.lock().await;
        if self.has_bindings().await? {
            return Err(AppError::conflict(
                "ism_bindings_exist",
                "Cannot disconnect while volumes are bound.",
            ));
        }
        sqlx::query("DELETE FROM ism_connection WHERE id = 1")
            .execute(self.db.write())
            .await?;
        self.status().await
    }
}

struct IsmClient {
    base_url: Url,
    authorization: HeaderValue,
    http: reqwest::Client,
}

impl IsmClient {
    fn new(base_url: &str, token: &str) -> AppResult<Self> {
        let mut url = Url::parse(base_url.trim())
            .map_err(|_| AppError::bad_request("ism_invalid_url", "Invalid ISM URL."))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(AppError::bad_request(
                "ism_invalid_url",
                "ISM URL must be HTTP(S) without credentials, query, or fragment.",
            ));
        }
        let path = format!("{}/", url.path().trim_end_matches('/'));
        url.set_path(&path);
        if token.is_empty() || token.trim() != token {
            return Err(AppError::bad_request(
                "ism_invalid_token",
                "Enter an ISM token.",
            ));
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| AppError::bad_request("ism_invalid_token", "Invalid ISM token."))?;
        authorization.set_sensitive(true);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(AppError::internal)?;
        Ok(Self {
            base_url: url,
            authorization,
            http,
        })
    }

    /// A GET answered with at most `MAX_INFO_BYTES` of JSON.
    async fn get_bytes(&self, url: Url) -> AppResult<Vec<u8>> {
        let mut response = self
            .http
            .get(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| AppError::conflict("ism_unavailable", "Cannot reach ISM."))?;
        if !response.status().is_success() {
            return Err(AppError::conflict(
                "ism_request_failed",
                format!(
                    "ISM rejected the request (HTTP {}).",
                    response.status().as_u16()
                ),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AppError::conflict("ism_unavailable", "Cannot read ISM response."))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_INFO_BYTES {
                return Err(AppError::conflict(
                    "ism_invalid_response",
                    "ISM response is too large.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// The recoverable items of an ISM root, newest first.
    async fn trash_list(&self, root_id: &str) -> AppResult<Vec<wire::TrashEntry>> {
        let mut url = self
            .base_url
            .join("api/v1/trash")
            .map_err(AppError::internal)?;
        url.query_pairs_mut().append_pair("root_id", root_id);
        let bytes = self.get_bytes(url).await?;
        serde_json::from_slice(&bytes)
            .map_err(|_| AppError::conflict("ism_invalid_response", "Invalid ISM trash listing."))
    }

    async fn info(&self) -> AppResult<IsmInfoDto> {
        let url = self
            .base_url
            .join("api/v1/client/info")
            .map_err(AppError::internal)?;
        let bytes = self.get_bytes(url).await?;
        let info: IsmInfoDto = serde_json::from_slice::<wire::Info>(&bytes)
            .map(wire::Info::into_dto)
            .map_err(|_| AppError::conflict("ism_invalid_response", "Invalid ISM information."))?;
        if info.api_version != 1
            || info.instance_id.is_empty()
            || info.token_kind != "opm"
            || !["read", "manage"]
                .iter()
                .all(|required| info.actions.iter().any(|action| action == required))
        {
            return Err(AppError::conflict(
                "ism_incompatible",
                "ISM requires API version 1 and an OPM token with read and manage permissions.",
            ));
        }
        Ok(info)
    }
}

/// The volume root relative to the folder ISM gives OPM for a root ('' when they are the same).
fn prefix_within(opm_prefix: &Path, volume_root: &Path) -> Option<String> {
    let rest = volume_root.strip_prefix(opm_prefix).ok()?;
    let parts = rest
        .components()
        .map(|part| match part {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let prefix = parts.join("/");
    (prefix.is_empty() || RelPath::new(&prefix).is_ok()).then_some(prefix)
}

/// Applies the staged binding changes. Runs at startup, before the bindings are loaded and before
/// recovery: unbinds always apply; a bind waits (with the reason) while its volume still has
/// legacy work, which OPM settles first.
///
/// # Errors
/// Only when the database cannot be read or written.
pub async fn apply_staged_bindings(db: &Db) -> anyhow::Result<()> {
    let mut tx = db.begin_write().await?;
    sqlx::query(
        "DELETE FROM ism_volume_roots WHERE volume_id IN
             (SELECT volume_id FROM ism_binding_changes WHERE root_id IS NULL)",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM ism_binding_changes WHERE root_id IS NULL")
        .execute(&mut *tx)
        .await?;
    let binds: Vec<(VolumeId, String, String, String)> = sqlx::query_as(
        "SELECT volume_id, root_id, root_name, root_prefix FROM ism_binding_changes
         ORDER BY requested_at, volume_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (volume_id, root_id, root_name, root_prefix) in binds {
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM immich_outbox
                     WHERE old_volume_id = ?1 OR new_volume_id = ?1)
                 OR EXISTS(SELECT 1 FROM immich_direct_move
                     WHERE old_volume_id = ?1 OR new_volume_id = ?1)
                 OR EXISTS(SELECT 1 FROM fs_journal WHERE state = 'pending'
                     AND (src_volume_id = ?1 OR dst_volume_id = ?1))",
        )
        .bind(volume_id)
        .fetch_one(&mut *tx)
        .await?;
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM ism_volume_roots WHERE root_id = ? AND volume_id <> ?)",
        )
        .bind(&root_id)
        .bind(volume_id)
        .fetch_one(&mut *tx)
        .await?;
        let error = if pending {
            Some(
                "The volume still has unfinished file or Immich work; it is retried at the next start.",
            )
        } else if taken {
            Some("Another volume is bound to this ISM root.")
        } else {
            None
        };
        if let Some(error) = error {
            sqlx::query("UPDATE ism_binding_changes SET error = ? WHERE volume_id = ?")
                .bind(error)
                .bind(volume_id)
                .execute(&mut *tx)
                .await?;
            tracing::warn!(volume_id, "an ISM binding is waiting: {error}");
            continue;
        }
        sqlx::query(
            "INSERT INTO ism_volume_roots (volume_id, root_id, root_name, root_prefix)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(volume_id) DO UPDATE SET root_id = excluded.root_id,
                 root_name = excluded.root_name, root_prefix = excluded.root_prefix",
        )
        .bind(volume_id)
        .bind(&root_id)
        .bind(&root_name)
        .bind(&root_prefix)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM ism_binding_changes WHERE volume_id = ?")
            .bind(volume_id)
            .execute(&mut *tx)
            .await?;
        tracing::info!(volume_id, "the volume is now managed by ISM");
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_is_placed_inside_its_root() {
        let root = Path::new("/volumes/photos");
        assert_eq!(
            prefix_within(root, Path::new("/volumes/photos")).as_deref(),
            Some("")
        );
        assert_eq!(
            prefix_within(root, Path::new("/volumes/photos/2024/trips")).as_deref(),
            Some("2024/trips")
        );
        assert_eq!(prefix_within(root, Path::new("/volumes/photos-old")), None);
        assert_eq!(prefix_within(root, Path::new("/volumes")), None);
    }
}
