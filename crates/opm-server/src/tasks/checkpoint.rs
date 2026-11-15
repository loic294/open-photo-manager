//! Durable task-local state. Save a frozen input before doing work and completed units afterwards.
//! Filesystem recovery must run first: the fs journal closes the mutation/checkpoint crash window.

use opm_core::paths::TaskId;
use serde::{Serialize, de::DeserializeOwned};

use crate::db::{Db, DbError};

#[derive(Clone)]
pub(crate) struct Checkpoint {
    db: Db,
    id: TaskId,
    enabled: bool,
}

impl Checkpoint {
    pub(crate) async fn entries<T: DeserializeOwned>(
        &self,
        prefix: &str,
    ) -> Result<Vec<(String, T)>, DbError> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT key, value FROM task_checkpoints WHERE task_id = ? AND key LIKE ?",
        )
        .bind(self.id)
        .bind(format!("{prefix}%"))
        .fetch_all(self.db.read())
        .await?;
        rows.into_iter()
            .map(|(key, value)| {
                serde_json::from_str(&value)
                    .map(|value| (key, value))
                    .map_err(|err| DbError::Sqlx(sqlx::Error::Decode(Box::new(err))))
            })
            .collect()
    }

    pub(crate) async fn new(db: &Db, id: TaskId) -> Result<Self, DbError> {
        let enabled: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ? AND resumable = 1)")
                .bind(id)
                .fetch_one(db.read())
                .await?;
        Ok(Self {
            db: db.clone(),
            id,
            enabled,
        })
    }

    pub(crate) async fn load<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, DbError> {
        if !self.enabled {
            return Ok(None);
        }
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM task_checkpoints WHERE task_id = ? AND key = ?")
                .bind(self.id)
                .bind(key)
                .fetch_optional(self.db.read())
                .await?;
        value
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|err| DbError::Sqlx(sqlx::Error::Decode(Box::new(err))))
            })
            .transpose()
    }

    pub(crate) async fn save<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), DbError> {
        if !self.enabled {
            return Ok(());
        }
        let value = serde_json::to_string(value)
            .map_err(|err| DbError::Sqlx(sqlx::Error::Encode(Box::new(err))))?;
        sqlx::query(
            "INSERT INTO task_checkpoints (task_id, key, value) VALUES (?, ?, ?) \
             ON CONFLICT (task_id, key) DO UPDATE SET value = excluded.value",
        )
        .bind(self.id)
        .bind(key)
        .bind(value)
        .execute(self.db.write())
        .await?;
        Ok(())
    }
}
