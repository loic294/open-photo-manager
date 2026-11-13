//! Persistent daily observations, not estimates of historical filesystem state.
//!
//! File-fact triggers enqueue changes atomically with index writes. FsOps supplies provenance
//! and committed operation totals on that same transaction. The collector compacts bounded
//! batches, retaining distinct-file markers only while their calendar period may receive events.
//! Snapshots use a reader; the single writer only publishes their small serialized results.

pub(crate) mod record;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate, Utc};
use opm_core::datetime::{local_to_utc_secs, parse_tz};
use opm_core::dto::{
    StatisticsActivity, StatisticsCoverage, StatisticsInventory, StatisticsKind, StatisticsQuery,
    StatisticsResponse, StatisticsSegment, StatisticsSnapshot,
};
use sqlx::{FromRow, Row, SqliteConnection};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::db::{Db, now_secs};
use crate::error::{AppError, AppResult};
use crate::settings::SettingsService;

const REFRESH: Duration = Duration::from_secs(300);
const BATCH: i64 = 1000;

#[derive(Clone)]
pub struct StatisticsService(Arc<Inner>);

struct Inner {
    db: Db,
    settings: SettingsService,
    collecting: Mutex<Option<(i64, String)>>,
    worker: std::sync::Mutex<Option<JoinHandle<()>>>,
    session: i64,
}

#[derive(FromRow)]
struct Event {
    segment_id: i64,
    id: i64,
    at: i64,
    timezone: String,
    volume_id: i64,
    volume_name: String,
    identity: String,
    action: String,
    count: i64,
    bytes: i64,
    baseline: bool,
    operation_key: Option<String>,
}

impl StatisticsService {
    pub async fn new(db: Db, settings: SettingsService) -> AppResult<Self> {
        let now = now_secs();
        let mut tx = db.begin_write().await?;
        let stored: String =
            sqlx::query_scalar("SELECT timezone FROM statistics_segments ORDER BY id DESC LIMIT 1")
                .fetch_one(&mut *tx)
                .await?;
        if stored != settings.get().statistics_timezone {
            sqlx::query("UPDATE statistics_segments SET ended_at=? WHERE ended_at IS NULL")
                .bind(now)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO statistics_segments(timezone,started_at) VALUES(?,?)")
                .bind(&settings.get().statistics_timezone)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }
        let session = sqlx::query_scalar(
            "INSERT INTO statistics_coverage(started_at,last_seen_at) VALUES(?,?) RETURNING id",
        )
        .bind(now)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Self(Arc::new(Inner {
            db,
            settings,
            collecting: Mutex::new(None),
            worker: std::sync::Mutex::new(None),
            session,
        })))
    }

    pub fn start_background(&self, shutdown: CancellationToken, tasks: &crate::tasks::TaskEngine) {
        let mut worker = self
            .0
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worker.is_some() {
            return;
        }
        let service = self.clone();
        let mut events = tasks.subscribe();
        *worker = Some(tokio::spawn(async move {
            let mut tick = tokio::time::interval(REFRESH);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut settings = service.0.settings.subscribe();
            let mut scheduled = None;
            let mut force = true;
            loop {
                let zone = service.0.settings.get().statistics_timezone.clone();
                let boundary = match next_boundary(now_secs(), &zone) {
                    Ok(at) => {
                        Duration::from_secs(u64::try_from((at - now_secs()).max(1)).unwrap_or(1))
                    }
                    Err(error) => {
                        tracing::error!(%error, "could not schedule statistics midnight");
                        REFRESH
                    }
                };
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    _ = tick.tick() => { force = true; }
                    () = tokio::time::sleep(boundary) => { force = true; }
                    () = tokio::time::sleep_until(scheduled.unwrap_or_else(|| tokio::time::Instant::now() + REFRESH)), if scheduled.is_some() => {}
                    event = events.recv() => {
                        match event {
                            Ok(opm_core::dto::ServerEvent::Task { task }) if matches!(
                                task.state, opm_core::dto::TaskState::Done | opm_core::dto::TaskState::Failed | opm_core::dto::TaskState::Cancelled | opm_core::dto::TaskState::Interrupted
                            ) => {
                                force = true;
                                scheduled.get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(1));
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                force = true;
                                scheduled.get_or_insert_with(tokio::time::Instant::now);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            _ => {}
                        }
                        continue;
                    }
                    result = settings.changed() => {
                        if result.is_err() { break; }
                        force = true;
                    }
                }
                scheduled = None;
                let result = tokio::select! {
                    () = shutdown.cancelled() => break,
                    result = service.refresh(force) => result,
                };
                match result {
                    Ok((_, true)) => {
                        force = false;
                        scheduled = Some(tokio::time::Instant::now() + Duration::from_secs(1));
                    }
                    Ok((_, false)) => {}
                    Err(error) => tracing::error!(error = %error, "statistics collection failed"),
                }
            }
        }));
    }

    pub async fn shutdown(&self) {
        let worker = self
            .0
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker
            && let Err(error) = worker.await
        {
            tracing::error!(%error, "statistics worker stopped unexpectedly");
        }
        if let Err(error) = sqlx::query(
            "UPDATE statistics_coverage SET stopped_at = ?, last_seen_at = ? WHERE id = ?",
        )
        .bind(now_secs())
        .bind(now_secs())
        .bind(self.0.session)
        .execute(self.0.db.write())
        .await
        {
            tracing::error!(%error, "could not close statistics coverage");
        }
    }

    /// Also used by API reads; expensive inventory reads are shared and cached for a minute.
    /// Returns the observation time and whether another bounded queue pass is needed.
    pub async fn refresh(&self, force: bool) -> AppResult<(i64, bool)> {
        let mut last = self.0.collecting.lock().await;
        let more = self.compact(now_secs()).await?;
        let now = now_secs();
        let timezone = self.0.settings.get().statistics_timezone.clone();
        if !force
            && let Some((at, zone)) = &*last
            && *zone == timezone
            && now >= *at
            && now - *at < 60
        {
            return Ok((*at, more));
        }
        let (segment, snapshot_timezone, inventory) = self.inventory().await?;
        let day = calendar_day(now, &snapshot_timezone)?;
        let mut tx = self.0.db.begin_write().await?;
        for row in inventory {
            sqlx::query("INSERT INTO statistics_snapshots(segment_id,day,timezone,volume_id,observed_at,document) VALUES(?,?,?,?,?,?) ON CONFLICT(segment_id,day,volume_id) DO UPDATE SET observed_at=excluded.observed_at,document=excluded.document WHERE excluded.observed_at >= statistics_snapshots.observed_at")
                .bind(segment).bind(&day).bind(&snapshot_timezone).bind(row.volume_id).bind(now)
                .bind(serde_json::to_string(&row).map_err(AppError::internal)?)
                .execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE statistics_coverage SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(self.0.session)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        *last = Some((now, snapshot_timezone));
        Ok((now, more))
    }

    async fn compact(&self, now: i64) -> AppResult<bool> {
        let mut more = true;
        // Bound each pass as well as each transaction. A large import drains over later passes.
        for _ in 0..10 {
            let mut tx = self.0.db.begin_write().await?;
            let events: Vec<Event> = sqlx::query_as(
                "SELECT e.segment_id,e.id,e.at,e.timezone,e.volume_id,e.volume_name,e.identity,e.action,e.count,e.bytes,e.baseline,e.operation_key \
                 FROM statistics_events e LEFT JOIN statistics_intents i ON i.key=e.operation_key \
                 WHERE i.state IS NULL OR i.state <> 'pending' ORDER BY e.id LIMIT ?",
            ).bind(BATCH).fetch_all(&mut *tx).await?;
            if events.is_empty() {
                more = false;
                tx.commit().await?;
                break;
            }
            for event in &events {
                accumulate(&mut tx, event).await?;
                sqlx::query("DELETE FROM statistics_events WHERE id=?")
                    .bind(event.id)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            if events.len() < BATCH as usize {
                more = false;
                break;
            }
        }
        // Leave at least two local days for late recovery and never forget a pending event's day.
        // Completed operation markers remain until their journal is gone, so replay is harmless.
        let cutoff = DateTime::from_timestamp(now - 3 * 86_400, 0)
            .ok_or_else(|| AppError::internal(anyhow::anyhow!("statistics clock outside range")))?
            .date_naive()
            .to_string();
        let mut tx = self.0.db.begin_write().await?;
        more |= sqlx::query("DELETE FROM statistics_seen WHERE rowid IN (SELECT rowid FROM statistics_seen WHERE day < ? AND NOT EXISTS(SELECT 1 FROM statistics_events) LIMIT ?)")
            .bind(cutoff).bind(BATCH).execute(&mut *tx).await?.rows_affected() == BATCH as u64;
        more |= sqlx::query("DELETE FROM statistics_operations WHERE key IN (SELECT i.key FROM statistics_intents i JOIN statistics_operations o ON o.key=i.key WHERE i.state <> 'pending' AND i.subtree=0 AND i.journal_id IS NULL AND NOT EXISTS(SELECT 1 FROM statistics_events e WHERE e.operation_key=i.key) LIMIT ?)")
            .bind(BATCH).execute(&mut *tx).await?.rows_affected() == BATCH as u64;
        more |= sqlx::query("DELETE FROM statistics_intents WHERE key IN (SELECT i.key FROM statistics_intents i WHERE i.state <> 'pending' AND i.subtree=0 AND i.journal_id IS NULL AND NOT EXISTS(SELECT 1 FROM statistics_folder_members m WHERE m.operation_key=i.key) AND NOT EXISTS(SELECT 1 FROM statistics_events e WHERE e.operation_key=i.key) LIMIT ?)")
            .bind(BATCH).execute(&mut *tx).await?.rows_affected() == BATCH as u64;
        tx.commit().await?;
        Ok(more)
    }

    async fn inventory(&self) -> AppResult<(i64, String, Vec<StatisticsInventory>)> {
        let mut tx = self.0.db.read().begin().await?;
        let (segment, timezone): (i64, String) =
            sqlx::query_as("SELECT id,timezone FROM statistics_segments ORDER BY id DESC LIMIT 1")
                .fetch_one(&mut *tx)
                .await?;
        let volumes = sqlx::query("SELECT v.id,v.name,v.online,v.last_scan_at,v.held_removals,b.complete FROM volumes v JOIN statistics_baselines b ON b.volume_id=v.id ORDER BY v.id")
            .fetch_all(&mut *tx).await?;
        let groups = sqlx::query("SELECT volume_id,kind,COUNT(*) AS files,COALESCE(SUM(size),0) AS bytes,SUM(meta_state=0) AS mp,SUM(meta_state=2) AS mf,SUM(hash_state=0) AS hp,SUM(hash_state=2) AS hf,SUM(thumb_state=0) AS tp,SUM(thumb_state=2) AS tf FROM files GROUP BY volume_id,kind")
            .fetch_all(&mut *tx).await?;
        let trash = sqlx::query("SELECT volume_id,items,bytes FROM trash_volume_totals")
            .fetch_all(&mut *tx)
            .await?;
        let mut out = Vec::new();
        for volume in volumes {
            let id: i64 = volume.try_get("id")?;
            let mut row = StatisticsInventory {
                volume_id: id,
                volume_name: volume.try_get("name")?,
                online: volume.try_get("online")?,
                last_scan_at: volume.try_get("last_scan_at")?,
                held_removals: volume.try_get("held_removals")?,
                baseline_complete: volume.try_get("complete")?,
                ..Default::default()
            };
            for group in &groups {
                if group.try_get::<i64, _>("volume_id")? != id {
                    continue;
                }
                let files: i64 = group.try_get("files")?;
                let bytes: i64 = group.try_get("bytes")?;
                row.files += files;
                row.bytes += bytes;
                row.kinds.push(StatisticsKind {
                    kind: group.try_get("kind")?,
                    files,
                    bytes,
                });
                row.metadata_pending += group.try_get::<i64, _>("mp")?;
                row.metadata_failed += group.try_get::<i64, _>("mf")?;
                row.hashes_pending += group.try_get::<i64, _>("hp")?;
                row.hashes_failed += group.try_get::<i64, _>("hf")?;
                row.thumbnails_pending += group.try_get::<i64, _>("tp")?;
                row.thumbnails_failed += group.try_get::<i64, _>("tf")?;
            }
            for item in &trash {
                if item.try_get::<i64, _>("volume_id")? == id {
                    row.trash_items = item.try_get("items")?;
                    row.trash_bytes = item.try_get("bytes")?;
                }
            }
            out.push(row);
        }
        tx.commit().await?;
        Ok((segment, timezone, out))
    }

    pub async fn history(&self, query: StatisticsQuery) -> AppResult<StatisticsResponse> {
        let timezone = self.0.settings.get().statistics_timezone.clone();
        let today = calendar_day(now_secs(), &timezone)?;
        let to = date(query.to.as_deref().unwrap_or(&today))?;
        let from = match query.from {
            Some(ref day) => date(day)?,
            None => to.checked_sub_days(Days::new(29)).ok_or_else(|| {
                AppError::bad_request("invalid_date_range", "Date is out of range")
            })?,
        };
        if from > to || (to - from).num_days() > 365 {
            return Err(AppError::bad_request(
                "invalid_date_range",
                "Choose an ordered range of at most 366 days",
            ));
        }
        let (observed_at, _) = self.refresh(false).await?;
        let mut tx = self.0.db.read().begin().await?;
        let mut snapshots = Vec::new();
        for row in sqlx::query("SELECT segment_id,day,timezone,observed_at,document FROM statistics_snapshots WHERE day BETWEEN ? AND ? AND (? IS NULL OR volume_id=?) ORDER BY day,segment_id,volume_id")
            .bind(from.to_string()).bind(to.to_string()).bind(query.volume_id).bind(query.volume_id).fetch_all(&mut *tx).await?
        {
            snapshots.push(StatisticsSnapshot {
                segment_id: row.try_get("segment_id")?,
                day: row.try_get("day")?, timezone: row.try_get("timezone")?,
                observed_at: row.try_get("observed_at")?,
                inventory: serde_json::from_str(row.try_get("document")?).map_err(AppError::internal)?,
            });
        }
        let mut activity = Vec::new();
        for row in sqlx::query("SELECT * FROM statistics_daily WHERE day BETWEEN ? AND ? AND (? IS NULL OR volume_id=?) ORDER BY day,timezone,volume_id,action")
            .bind(from.to_string()).bind(to.to_string()).bind(query.volume_id).bind(query.volume_id).fetch_all(&mut *tx).await?
        {
            activity.push(StatisticsActivity {
                segment_id: row.try_get("segment_id")?,
                day: row.try_get("day")?, timezone: row.try_get("timezone")?,
                volume_id: row.try_get("volume_id")?, volume_name: row.try_get("volume_name")?,
                action: row.try_get("action")?, count: row.try_get("count")?, bytes: row.try_get("bytes")?,
            });
        }
        let started_at = sqlx::query_scalar("SELECT MIN(started_at) FROM statistics_coverage")
            .fetch_one(&mut *tx)
            .await?;
        let pending_events = sqlx::query_scalar("SELECT COUNT(*) FROM statistics_events")
            .fetch_one(&mut *tx)
            .await?;
        let mut coverage = Vec::new();
        for row in sqlx::query("SELECT started_at,last_seen_at,stopped_at FROM statistics_coverage WHERE last_seen_at >= ? AND started_at <= ? ORDER BY started_at")
            .bind(from.and_hms_opt(0,0,0).map(|d| d.and_utc().timestamp()-86_400))
            .bind(to.and_hms_opt(0,0,0).map(|d| d.and_utc().timestamp()+2*86_400))
            .fetch_all(&mut *tx).await?
        {
            coverage.push(StatisticsCoverage {
                started_at: row.try_get("started_at")?, last_seen_at: row.try_get("last_seen_at")?,
                stopped_at: row.try_get("stopped_at")?,
            });
        }
        let mut segments = Vec::new();
        for row in sqlx::query("SELECT id,timezone,started_at,ended_at FROM statistics_segments WHERE (ended_at IS NULL OR ended_at >= ?) AND started_at <= ? ORDER BY id")
            .bind(from.and_hms_opt(0,0,0).map(|d| d.and_utc().timestamp()-86_400))
            .bind(to.and_hms_opt(0,0,0).map(|d| d.and_utc().timestamp()+2*86_400))
            .fetch_all(&mut *tx).await?
        {
            segments.push(StatisticsSegment {
                id: row.try_get("id")?, timezone: row.try_get("timezone")?,
                started_at: row.try_get("started_at")?, ended_at: row.try_get("ended_at")?,
            });
        }
        tx.commit().await?;
        Ok(StatisticsResponse {
            timezone,
            from: from.to_string(),
            to: to.to_string(),
            observed_at,
            started_at,
            pending_events,
            snapshots,
            activity,
            coverage,
            segments,
        })
    }
}

fn date(value: &str) -> AppResult<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .filter(|day| day.to_string() == value)
        .ok_or_else(|| AppError::bad_request("invalid_date_range", "Use YYYY-MM-DD dates"))
}

fn next_boundary(at: i64, timezone: &str) -> AppResult<i64> {
    let zone =
        parse_tz(timezone).ok_or_else(|| AppError::internal("invalid statistics timezone"))?;
    let instant = DateTime::from_timestamp(at, 0)
        .ok_or_else(|| AppError::internal("statistics clock outside range"))?;
    let midnight = instant
        .with_timezone(&zone)
        .date_naive()
        .succ_opt()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .ok_or_else(|| AppError::internal("statistics date outside range"))?;
    Ok(local_to_utc_secs(midnight.and_utc().timestamp(), &zone))
}

fn calendar_day(at: i64, timezone: &str) -> AppResult<String> {
    let timezone = parse_tz(timezone)
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("invalid statistics timezone")))?;
    let instant: DateTime<Utc> = DateTime::from_timestamp(at, 0)
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("statistics timestamp outside range")))?;
    Ok(instant.with_timezone(&timezone).date_naive().to_string())
}

async fn accumulate(conn: &mut SqliteConnection, event: &Event) -> AppResult<()> {
    let operation = event.action.strip_prefix("op_");
    if operation.is_none() && event.operation_key.is_some() {
        // A successful operation records its own semantic totals; failed/abandoned intents
        // leave genuine scanner observations intact rather than claiming successful work.
        let state: Option<String> =
            sqlx::query_scalar("SELECT i.state FROM statistics_intents i JOIN statistics_operations o ON o.key=i.key WHERE i.key=?")
                .bind(&event.operation_key)
                .fetch_optional(&mut *conn)
                .await?;
        if state.as_deref() == Some("done") {
            return Ok(());
        }
    }
    let action = operation.unwrap_or(if event.baseline {
        "baseline"
    } else {
        &event.action
    });
    if event.baseline && operation.is_none() && event.action != "added" {
        return Ok(());
    }
    let day = calendar_day(event.at, &event.timezone)?;
    let first = sqlx::query("INSERT INTO statistics_seen(segment_id,day,timezone,identity,action) VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING")
        .bind(event.segment_id).bind(&day).bind(&event.timezone).bind(&event.identity).bind(action)
        .execute(&mut *conn).await?.rows_affected() != 0;
    if first {
        sqlx::query("INSERT INTO statistics_daily(segment_id,day,timezone,volume_id,volume_name,action,count,bytes) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(segment_id,day,volume_id,action) DO UPDATE SET count=count+excluded.count,bytes=bytes+excluded.bytes")
            .bind(event.segment_id).bind(day).bind(&event.timezone).bind(event.volume_id).bind(&event.volume_name)
            .bind(action).bind(event.count).bind(event.bytes).execute(conn).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
