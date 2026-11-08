//! Owned by `updating-immich-move-paths` (migrations 0140-0149).
//! Unsupported upstream operation, pinned to Immich v3.3.3, commit
//! d2dac658fe1c67e8bbd08aa18200a32e5d45b9e4. Locks/CAS protect this
//! writer, NOT Immich's scanner: there is no unique originalPath constraint.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{ConnectOptions, Connection, FromRow, PgConnection, PgPool};
use tokio::sync::Mutex;

mod contract;
#[cfg(test)]
mod postgres_tests;
#[cfg(test)]
mod tests;

pub const RISK: &str = "Direct SQL modification is not officially supported by Immich v3.3.3. Keep a current backup. PostgreSQL transactions cannot serialize with Immich's filesystem scanner: an active or already queued scan can import a duplicate or remove an asset during a move. Pause watching and scheduled/manual scans and drain queued scan jobs before moving.";

pub(super) async fn status(
    db: &crate::db::Db,
) -> Result<apm_core::dto::ImmichDirectDatabaseStatus, crate::db::DbError> {
    let (ready, reason, checked_at): (bool, String, Option<i64>) =
        sqlx::query_as("SELECT ready, reason, checked_at FROM immich_direct_db_state WHERE id = 1")
            .fetch_one(db.read())
            .await?;
    Ok(apm_core::dto::ImmichDirectDatabaseStatus {
        ready,
        reason,
        checked_at,
        warning: RISK.to_owned(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DbMoveError {
    #[error(
        "Direct database access is not configured. Set APM_IMMICH_DATABASE_URL_FILE and restart APM."
    )]
    NotConfigured,
    #[error(
        "The database secret file or TLS options are invalid. Check the read-only secret mount and sslmode=verify-full with a trusted CA, then restart APM."
    )]
    Configuration,
    #[error(
        "The Immich database is unavailable or denied access. Check private PostgreSQL networking, TLS, and role permissions, then retry."
    )]
    Unavailable,
    #[error(
        "The API/database fingerprint is not exactly Immich v3.3.3. Check that both point to the same backed-up v3.3.3 instance; do not downgrade its database."
    )]
    Incompatible,
    #[error(
        "The asset path or update cursor changed. Refresh Immich status, finish pending updates, and rebuild/retry the move."
    )]
    Stale,
    #[error(
        "The destination already belongs to another Immich asset. Resolve the collision before retrying."
    )]
    Collision,
    #[error(
        "The asset checksum algorithm or path checksum cannot be verified. Only Immich v3.3.3 sha1 and sha1-path are accepted; refresh and check the backed-up database before retrying."
    )]
    Checksum,
    #[error(
        "A previous Immich path update for this file is still unconfirmed. Finish or retry the durable Immich outbox update, refresh status, then retry this move."
    )]
    PendingUpdate,
    #[error(
        "The asset is not external, or its owner/library does not match the connected account and mapping. Check the instance, account and mappings."
    )]
    Identity,
    #[error(
        "The destination is excluded, outside the same library, or its exclusion rules cannot be verified. Check library import paths and exclusions; cross-library in-place moves are refused."
    )]
    Destination,
    #[error(
        "This library uses exclusion patterns APM cannot verify. The supported subset is Immich v3.3.3's six default rules plus **/.apm-trash/**; review custom rules before retrying."
    )]
    ExclusionRules,
    #[error(
        "The linked sidecar is not included in this move, or changed. Move the original and its exact linked sidecar together and retry."
    )]
    Sidecar,
    #[error(
        "The linked sidecar destination is not yet confirmed by FsOps. Wait for its copy/move to finish; if it failed, repair the exact sidecar destination through APM and retry the queued update."
    )]
    SidecarPending,
}

impl DbMoveError {
    pub fn refusal(self) -> String {
        format!("No file was moved. {self}")
    }

    pub fn queued_warning(self) -> String {
        format!(
            "The file moved, but its Immich path update is NOT confirmed. The durable outbox retains it for retry. {self}"
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarMove {
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveSpec {
    pub asset_id: String,
    pub owner_id: String,
    pub library_id: String,
    pub old: String,
    pub new: String,
    pub new_name: String,
    pub sidecars: Vec<SidecarMove>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedMove {
    pub spec: MoveSpec,
    pub update_id: String,
    pub sidecar: Option<SidecarMove>,
    pub sidecar_update_id: Option<String>,
    pub checksum: Vec<u8>,
    pub checksum_algorithm: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveApplied {
    Updated,
    AlreadyApplied,
}

/// One seam for production and public-safe tests. Implementations must redact
/// errors, enforce the contract, and never disable the timestamp/cursor triggers.
#[async_trait]
pub trait MoveDatabase: Send + Sync {
    async fn probe(&self, api_version: &str) -> Result<(), DbMoveError>;
    async fn prepare(&self, spec: MoveSpec) -> Result<PreparedMove, DbMoveError>;
    async fn apply(&self, prepared: &PreparedMove) -> Result<MoveApplied, DbMoveError>;
}

/// The secret is read only here, never in handlers, config Debug or SQLite.
pub struct PostgresMoves {
    secret_file: Option<PathBuf>,
    pool: Mutex<Option<PgPool>>,
}

impl fmt::Debug for PostgresMoves {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresMoves").finish_non_exhaustive()
    }
}

impl PostgresMoves {
    pub fn new(secret_file: Option<PathBuf>) -> Self {
        Self {
            secret_file,
            pool: Mutex::new(None),
        }
    }

    async fn pool(&self) -> Result<PgPool, DbMoveError> {
        let mut cached = self.pool.lock().await;
        if let Some(pool) = &*cached {
            return Ok(pool.clone());
        }
        let path = self
            .secret_file
            .as_ref()
            .ok_or(DbMoveError::NotConfigured)?;
        let secret = tokio::fs::read_to_string(path)
            .await
            .map_err(|_| DbMoveError::Configuration)?;
        let options = connection_options(secret.trim())?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(3))
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query(
                        "SELECT set_config('search_path', 'pg_catalog, public', false),
                    set_config('statement_timeout', '5000', false)",
                    )
                    .execute(conn)
                    .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|_| DbMoveError::Unavailable)?;
        *cached = Some(pool.clone());
        Ok(pool)
    }

    async fn checked_connection(
        &self,
    ) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, DbMoveError> {
        let pool = self.pool().await?;
        let mut conn = pool.acquire().await.map_err(|_| DbMoveError::Unavailable)?;
        contract::verify(&mut conn).await?;
        Ok(conn)
    }
}

fn connection_options(secret: &str) -> Result<PgConnectOptions, DbMoveError> {
    let url = url::Url::parse(secret).map_err(|_| DbMoveError::Configuration)?;
    if !matches!(url.scheme(), "postgres" | "postgresql")
        || !url
            .query_pairs()
            .any(|(k, v)| k == "sslmode" && v == "verify-full")
        || url
            .query_pairs()
            .any(|(k, _)| !matches!(k.as_ref(), "sslmode" | "sslrootcert"))
    {
        return Err(DbMoveError::Configuration);
    }
    PgConnectOptions::from_str(secret)
        .map(|options| {
            options
                .ssl_mode(PgSslMode::VerifyFull)
                .disable_statement_logging()
        })
        .map_err(|_| DbMoveError::Configuration)
}

#[derive(FromRow)]
struct AssetRow {
    owner_id: String,
    library_id: Option<String>,
    external: bool,
    original_path: String,
    original_name: String,
    update_id: String,
    checksum: Vec<u8>,
    checksum_algorithm: String,
}

// Immich runs on Linux: library.service.ts processEntity uses Node's POSIX
// path.normalize, not Unicode normalization, case folding, or file contents.
fn normalized_path(path: &str) -> Result<String, DbMoveError> {
    if !path.starts_with('/') || path.contains('\0') {
        return Err(DbMoveError::Destination);
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let mut normalized = format!("/{}", parts.join("/"));
    if path.ends_with('/') && normalized != "/" {
        normalized.push('/');
    }
    Ok(normalized)
}

fn path_checksum(path: &str) -> Result<Vec<u8>, DbMoveError> {
    Ok(apm_core::hash::sha1_bytes(format!("path:{}", normalized_path(path)?).as_bytes()).to_vec())
}

fn destination_checksum(row: &AssetRow, spec: &MoveSpec) -> Result<Vec<u8>, DbMoveError> {
    if row.checksum.len() != apm_core::hash::DIGEST_LEN {
        return Err(DbMoveError::Checksum);
    }
    match row.checksum_algorithm.as_str() {
        "sha1-path" => path_checksum(&spec.new),
        "sha1" => Ok(row.checksum.clone()),
        _ => Err(DbMoveError::Checksum),
    }
}

fn mutation_error(error: sqlx::Error) -> DbMoveError {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        DbMoveError::Collision
    } else {
        DbMoveError::Unavailable
    }
}

#[derive(FromRow)]
struct LibraryRow {
    owner_id: String,
    import_paths: Vec<String>,
    exclusions: Vec<String>,
}

fn covered(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        let root = root.trim_end_matches('/');
        path.strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn excluded(path: &str, patterns: &[String]) -> Result<bool, DbMoveError> {
    let parts: Vec<_> = path.split('/').collect();
    let mut excluded = false;
    for pattern in patterns {
        let folder = match pattern.as_str() {
            "**/@eaDir/**" => Some("@eaDir"),
            "**/#recycle/**" => Some("#recycle"),
            "**/#snapshot/**" => Some("#snapshot"),
            "**/.stversions/**" => Some(".stversions"),
            "**/.stfolder/**" => Some(".stfolder"),
            "**/.apm-trash/**" => Some(".apm-trash"),
            "**/._*" => None,
            _ => return Err(DbMoveError::ExclusionRules),
        };
        excluded |= match folder {
            // Also reject case variants: Immich's offline sweep uses ~*.
            Some(folder) => parts.iter().any(|part| part.eq_ignore_ascii_case(folder)),
            None => parts.last().is_some_and(|name| name.starts_with("._")),
        };
    }
    Ok(excluded)
}

async fn lock_asset(conn: &mut PgConnection, spec: &MoveSpec) -> Result<AssetRow, DbMoveError> {
    sqlx::query_as::<_, AssetRow>(
        r#"SELECT "ownerId"::text AS owner_id, "libraryId"::text AS library_id,
        "isExternal" AS external, "originalPath" AS original_path,
        "originalFileName" AS original_name, "updateId"::text AS update_id,
        checksum, "checksumAlgorithm"::text AS checksum_algorithm
        FROM public.asset WHERE id = $1::uuid AND "deletedAt" IS NULL FOR UPDATE"#,
    )
    .bind(&spec.asset_id)
    .fetch_optional(conn)
    .await
    .map_err(|_| DbMoveError::Unavailable)?
    .ok_or(DbMoveError::Stale)
}

async fn validate(
    conn: &mut PgConnection,
    spec: &MoveSpec,
    row: &AssetRow,
) -> Result<LibraryRow, DbMoveError> {
    if !row.external
        || row.owner_id != spec.owner_id
        || row.library_id.as_deref() != Some(&spec.library_id)
    {
        return Err(DbMoveError::Identity);
    }
    let library = sqlx::query_as::<_, LibraryRow>(
        r#"SELECT "ownerId"::text AS owner_id, "importPaths" AS import_paths,
        "exclusionPatterns" AS exclusions FROM public.library
        WHERE id = $1::uuid AND "deletedAt" IS NULL FOR SHARE"#,
    )
    .bind(&spec.library_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|_| DbMoveError::Unavailable)?
    .ok_or(DbMoveError::Identity)?;
    if library.owner_id != spec.owner_id {
        return Err(DbMoveError::Identity);
    }
    // Fail closed on globs whose picomatch semantics this adapter cannot prove.
    if !covered(&spec.old, &library.import_paths)
        || !covered(&spec.new, &library.import_paths)
        || spec.new.split('/').any(|part| part == ".apm-trash")
        || excluded(&spec.new, &library.exclusions)?
    {
        return Err(DbMoveError::Destination);
    }
    let collision: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM public.asset
        WHERE "libraryId" = $1::uuid AND id <> $3::uuid
        AND ("originalPath" = $2 OR ("ownerId" = $4::uuid AND checksum = $5)))"#,
    )
    .bind(&spec.library_id)
    .bind(&spec.new)
    .bind(&spec.asset_id)
    .bind(&spec.owner_id)
    .bind(destination_checksum(row, spec)?)
    .fetch_one(conn)
    .await
    .map_err(|_| DbMoveError::Unavailable)?;
    if collision {
        return Err(DbMoveError::Collision);
    }
    Ok(library)
}

#[derive(FromRow)]
struct SidecarRow {
    path: String,
    update_id: String,
    edited: bool,
}

async fn sidecars(conn: &mut PgConnection, id: &str) -> Result<Vec<SidecarRow>, DbMoveError> {
    sqlx::query_as(
        r#"SELECT path, "updateId"::text AS update_id, "isEdited" AS edited
        FROM public.asset_file WHERE "assetId" = $1::uuid AND type = 'sidecar' FOR UPDATE"#,
    )
    .bind(id)
    .fetch_all(conn)
    .await
    .map_err(|_| DbMoveError::Unavailable)
}

#[async_trait]
impl MoveDatabase for PostgresMoves {
    async fn probe(&self, api_version: &str) -> Result<(), DbMoveError> {
        if !matches!(api_version, "3.3.3" | "v3.3.3") {
            return Err(DbMoveError::Incompatible);
        }

        tokio::time::timeout(Duration::from_secs(5), self.checked_connection())
            .await
            .map_err(|_| DbMoveError::Unavailable)??;
        Ok(())
    }

    async fn prepare(&self, spec: MoveSpec) -> Result<PreparedMove, DbMoveError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut conn = self.checked_connection().await?;
            let mut tx = conn.begin().await.map_err(|_| DbMoveError::Unavailable)?;
            let row = lock_asset(&mut tx, &spec).await?;
            let library = validate(&mut tx, &spec, &row).await?;
            if row.original_path != spec.old {
                return Err(DbMoveError::Stale);
            }
            if row.checksum_algorithm == "sha1-path" && row.checksum != path_checksum(&spec.old)? {
                return Err(DbMoveError::Checksum);
            }
            let rows = sidecars(&mut tx, &spec.asset_id).await?;
            if rows.len() > 1 || rows.iter().any(|row| row.edited) {
                return Err(DbMoveError::Sidecar);
            }
            let (sidecar, sidecar_update_id) = match rows.first() {
                Some(row) => {
                    let moved = spec
                        .sidecars
                        .iter()
                        .find(|moved| moved.old == row.path)
                        .ok_or(DbMoveError::Sidecar)?;
                    if !covered(&moved.new, &library.import_paths)
                        || excluded(&moved.new, &library.exclusions)?
                    {
                        return Err(DbMoveError::Destination);
                    }
                    (Some(moved.clone()), Some(row.update_id.clone()))
                }
                None => (None, None),
            };
            tx.rollback().await.map_err(|_| DbMoveError::Unavailable)?;
            Ok(PreparedMove {
                spec,
                update_id: row.update_id,
                sidecar,
                sidecar_update_id,
                checksum: row.checksum,
                checksum_algorithm: row.checksum_algorithm,
            })
        })
        .await
        .map_err(|_| DbMoveError::Unavailable)?
    }

    async fn apply(&self, prepared: &PreparedMove) -> Result<MoveApplied, DbMoveError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut conn = self.checked_connection().await?;
            let mut tx = conn.begin().await.map_err(|_| DbMoveError::Unavailable)?;
            let spec = &prepared.spec;
            let row = lock_asset(&mut tx, spec).await?;
            let library = validate(&mut tx, spec, &row).await?;
            if let Some(moved) = &prepared.sidecar
                && (!covered(&moved.new, &library.import_paths)
                    || excluded(&moved.new, &library.exclusions)?)
            {
                return Err(DbMoveError::Destination);
            }
            let rows = sidecars(&mut tx, &spec.asset_id).await?;
            if rows.len() > 1 || rows.iter().any(|row| row.edited) {
                return Err(DbMoveError::Sidecar);
            }
            let already = row.original_path == spec.new && row.original_name == spec.new_name;
            let checksum = destination_checksum(&row, spec)?;
            if row.checksum_algorithm != prepared.checksum_algorithm
                || if already {
                    row.checksum != checksum
                } else {
                    row.checksum != prepared.checksum
                }
            {
                return Err(DbMoveError::Checksum);
            }
            match (&prepared.sidecar, rows.first()) {
                (Some(moved), Some(row)) if already && row.path == moved.new => {}
                (Some(moved), Some(row))
                    if !already
                        && row.path == moved.old
                        && Some(&row.update_id) == prepared.sidecar_update_id.as_ref() => {}
                (None, None) => {}
                _ => return Err(DbMoveError::Sidecar),
            }
            if already {
                tx.rollback().await.map_err(|_| DbMoveError::Unavailable)?;
                return Ok(MoveApplied::AlreadyApplied);
            }
            if row.original_path != spec.old || row.update_id != prepared.update_id {
                return Err(DbMoveError::Stale);
            }
            let changed = sqlx::query(
                r#"UPDATE public.asset SET "originalPath" = $1, "originalFileName" = $2,
                checksum = $6
                WHERE id = $3::uuid AND "originalPath" = $4 AND "updateId" = $5::uuid
                AND checksum = $7 AND "checksumAlgorithm"::text = $8"#,
            )
            .bind(&spec.new)
            .bind(&spec.new_name)
            .bind(&spec.asset_id)
            .bind(&spec.old)
            .bind(&prepared.update_id)
            .bind(checksum)
            .bind(&prepared.checksum)
            .bind(&prepared.checksum_algorithm)
            .execute(&mut *tx)
            .await
            .map_err(mutation_error)?;
            if changed.rows_affected() != 1 {
                return Err(DbMoveError::Stale);
            }
            if let Some(moved) = &prepared.sidecar {
                let changed = sqlx::query(
                    r#"UPDATE public.asset_file SET path = $1
                    WHERE "assetId" = $2::uuid AND type = 'sidecar' AND "isEdited" = false
                    AND path = $3 AND "updateId" = $4::uuid"#,
                )
                .bind(&moved.new)
                .bind(&spec.asset_id)
                .bind(&moved.old)
                .bind(&prepared.sidecar_update_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| DbMoveError::Unavailable)?;
                if changed.rows_affected() != 1 {
                    return Err(DbMoveError::Sidecar);
                }
            }
            tx.commit().await.map_err(|_| DbMoveError::Unavailable)?;
            Ok(MoveApplied::Updated)
        })
        .await
        .map_err(|_| DbMoveError::Unavailable)?
    }
}
