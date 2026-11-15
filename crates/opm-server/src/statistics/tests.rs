use super::*;
use crate::db::testing::{insert_file, insert_volume};

#[tokio::test]
async fn upgrading_an_existing_index_seeds_identities_without_inventing_additions() {
    use sqlx::Connection;
    let mut conn = SqliteConnection::connect(":memory:").await.unwrap();
    for migration in sqlx::migrate!("./migrations")
        .iter()
        .filter(|migration| migration.version < 60)
    {
        sqlx::raw_sql(migration.sql.clone())
            .execute(&mut conn)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO volumes(id,name,root,writable,online,created_at,last_scan_at) VALUES(1,'photos','/volumes/photos',1,1,1,1)")
        .execute(&mut conn).await.unwrap();
    sqlx::query("INSERT INTO files(volume_id,dir_path,name,name_norm,stem_norm,ext,kind,size,mtime) VALUES(1,'','one.jpg','one.jpg','one','jpg','image',10,1)")
        .execute(&mut conn).await.unwrap();
    sqlx::query("INSERT INTO settings(key,value,updated_at) VALUES('app','{}',1)")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../migrations/0060_statistics.sql"))
        .execute(&mut conn)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM statistics_events")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("UPDATE files SET mtime=2")
        .execute(&mut conn)
        .await
        .unwrap();
    let event: Event = sqlx::query_as("SELECT * FROM statistics_events")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(event.action, "modified");
    assert!(!event.baseline);
    accumulate(&mut conn, &event).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT SUM(count) FROM statistics_daily WHERE action='modified'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

async fn drain(db: &Db) {
    let mut tx = db.begin_write().await.unwrap();
    let events: Vec<Event> = sqlx::query_as("SELECT segment_id,id,at,timezone,volume_id,volume_name,identity,action,count,bytes,baseline,operation_key FROM statistics_events ORDER BY id")
        .fetch_all(&mut *tx).await.unwrap();
    for event in events {
        accumulate(&mut tx, &event).await.unwrap();
    }
    sqlx::query("DELETE FROM statistics_events")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn count(db: &Db, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(SUM(count),0) FROM statistics_daily WHERE action=?")
        .bind(action)
        .fetch_one(db.read())
        .await
        .unwrap()
}

#[tokio::test]
async fn retention_is_bounded_and_requests_follow_up_until_finished() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let config = crate::config::Config::for_tests(std::path::Path::new("target"));
    let settings = SettingsService::load(db.clone(), &config).await.unwrap();
    let service = StatisticsService::new(db.clone(), settings).await.unwrap();
    sqlx::query("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<2005) INSERT INTO statistics_seen(segment_id,day,timezone,identity,action) SELECT 1,'2000-01-01','UTC',CAST(i AS TEXT),'added' FROM n")
        .execute(db.write()).await.unwrap();
    for remaining in [1005, 5, 0] {
        assert_eq!(service.compact(now_secs()).await.unwrap(), remaining != 0);
        let actual: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM statistics_seen")
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(actual, remaining);
    }
}

#[test]
fn local_calendar_respects_midnight_and_dst() {
    let zone = "America/Los_Angeles";
    let midnight = DateTime::parse_from_rfc3339("2026-11-13T08:00:00Z")
        .unwrap()
        .timestamp();
    assert_eq!(calendar_day(midnight - 1, zone).unwrap(), "2026-11-12");
    assert_eq!(calendar_day(midnight, zone).unwrap(), "2026-11-13");
    for (stamp, expected) in [
        ("2026-03-08T09:59:59Z", "2026-03-08"),
        ("2026-03-08T10:00:00Z", "2026-03-08"),
        ("2026-11-01T08:30:00Z", "2026-11-01"),
        ("2026-11-01T09:30:00Z", "2026-11-01"),
    ] {
        let at = DateTime::parse_from_rfc3339(stamp).unwrap().timestamp();
        assert_eq!(calendar_day(at, zone).unwrap(), expected);
    }
    assert!(date("2026-02-30").is_err());
    assert!(date("2026-1-01").is_err());
    for (midnight, hours) in [("2026-03-08T08:00:00Z", 23), ("2026-11-01T07:00:00Z", 25)] {
        let at = DateTime::parse_from_rfc3339(midnight).unwrap().timestamp();
        assert_eq!(next_boundary(at, zone).unwrap() - at, hours * 3600);
    }
}

#[tokio::test]
async fn pending_move_observations_wait_for_commit_and_do_not_hide_siblings() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let volume = insert_volume(&db, "photos").await;
    sqlx::query("UPDATE volumes SET last_scan_at=1")
        .execute(db.write())
        .await
        .unwrap();
    let source = insert_file(&db, volume, "source", "one.jpg").await;
    sqlx::query("UPDATE files SET mtime=2 WHERE id=?")
        .bind(source)
        .execute(db.write())
        .await
        .unwrap();
    drain(&db).await;
    let journal: i64 = sqlx::query_scalar(
        "INSERT INTO fs_journal(op,state,src_volume_id,src_path,dst_volume_id,dst_path,created_at) VALUES('move','pending',?,'source/one.jpg',?,'destination/one.jpg',unixepoch()) RETURNING id"
    ).bind(volume).bind(volume).fetch_one(db.write()).await.unwrap();
    let destination = insert_file(&db, volume, "destination", "one.jpg").await;
    insert_file(&db, volume, "destination/one.jpg.sibling", "other.jpg").await;
    sqlx::query("DELETE FROM files WHERE id=?")
        .bind(source)
        .execute(db.write())
        .await
        .unwrap();
    let config = crate::config::Config::for_tests(std::path::Path::new("target"));
    let settings = SettingsService::load(db.clone(), &config).await.unwrap();
    let service = StatisticsService::new(db.clone(), settings).await.unwrap();
    service.compact(now_secs()).await.unwrap();
    assert_eq!(
        count(&db, "added").await,
        2,
        "only the unrelated sibling is new"
    );
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM statistics_events")
        .fetch_one(db.read())
        .await
        .unwrap();
    assert_eq!(pending, 2);
    let mut tx = db.begin_write().await.unwrap();
    record::begin(&mut tx, journal).await.unwrap();
    record::moved_identity(&mut tx, journal, destination)
        .await
        .unwrap();
    sqlx::query("UPDATE fs_journal SET state='done' WHERE id=?")
        .bind(journal)
        .execute(&mut *tx)
        .await
        .unwrap();
    record::finish(&mut tx, journal, volume, &[]).await.unwrap();
    tx.commit().await.unwrap();
    service.compact(now_secs()).await.unwrap();
    assert_eq!(count(&db, "added").await, 2);
    assert_eq!(count(&db, "removed").await, 0);
    sqlx::query("UPDATE files SET mtime=3 WHERE id=?")
        .bind(destination)
        .execute(db.write())
        .await
        .unwrap();
    service.compact(now_secs()).await.unwrap();
    assert_eq!(
        count(&db, "modified").await,
        1,
        "a move preserves same-day distinctness even if the scanner replaced its row"
    );
}

#[tokio::test]
async fn baselines_actual_changes_distinctness_and_reused_ids() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let volume = insert_volume(&db, "photos").await;
    let old = insert_file(&db, volume, "", "baseline.jpg").await;
    drain(&db).await;
    assert_eq!(count(&db, "baseline").await, 1);
    assert_eq!(count(&db, "added").await, 0);
    sqlx::query("UPDATE volumes SET last_scan_at=1 WHERE id=?")
        .bind(volume)
        .execute(db.write())
        .await
        .unwrap();
    let file = insert_file(&db, volume, "", "new.jpg").await;
    sqlx::query(
        "UPDATE files SET size=size,mtime=mtime,meta_state=1,hash_state=1,thumb_state=1 WHERE id=?",
    )
    .bind(file)
    .execute(db.write())
    .await
    .unwrap();
    drain(&db).await;
    assert_eq!(count(&db, "modified").await, 0);
    sqlx::query("UPDATE files SET size=size+1 WHERE id=?")
        .bind(file)
        .execute(db.write())
        .await
        .unwrap();
    drain(&db).await;
    sqlx::query("UPDATE files SET mtime=mtime+1 WHERE id=?")
        .bind(file)
        .execute(db.write())
        .await
        .unwrap();
    drain(&db).await;
    assert_eq!(count(&db, "modified").await, 1);
    sqlx::query("DELETE FROM files WHERE id=?")
        .bind(file)
        .execute(db.write())
        .await
        .unwrap();
    let new = insert_file(&db, volume, "", "replacement.jpg").await;
    assert_eq!(new, file);
    drain(&db).await;
    assert_eq!(count(&db, "added").await, 2);
    assert_eq!(count(&db, "removed").await, 1);
    assert!(old < file);
}

#[tokio::test]
async fn operation_context_is_transactional_and_replay_safe() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let volume = insert_volume(&db, "photos").await;
    sqlx::query("UPDATE volumes SET last_scan_at=1")
        .execute(db.write())
        .await
        .unwrap();
    let file = insert_file(&db, volume, "", "one.jpg").await;
    drain(&db).await;
    let journal: i64 = sqlx::query_scalar("INSERT INTO fs_journal(op,state,src_volume_id,src_path,created_at) VALUES('trash','pending',?,'one.jpg',unixepoch()) RETURNING id")
        .bind(volume).fetch_one(db.write()).await.unwrap();
    let mut tx = db.begin_write().await.unwrap();
    record::begin(&mut tx, journal).await.unwrap();
    sqlx::query("DELETE FROM files WHERE id=?")
        .bind(file)
        .execute(&mut *tx)
        .await
        .unwrap();
    record::finish(&mut tx, journal, volume, &[("duplicates", 1, 100)])
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(count(&db, "duplicates").await, 0);
    let mut tx = db.begin_write().await.unwrap();
    record::begin(&mut tx, journal).await.unwrap();
    sqlx::query("UPDATE fs_journal SET state='done' WHERE id=?")
        .bind(journal)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM files WHERE id=?")
        .bind(file)
        .execute(&mut *tx)
        .await
        .unwrap();
    record::finish(&mut tx, journal, volume, &[("duplicates", 1, 100)])
        .await
        .unwrap();
    record::finish(&mut tx, journal, volume, &[("duplicates", 1, 100)])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    drain(&db).await;
    assert_eq!(count(&db, "duplicates").await, 1);
    assert_eq!(
        count(&db, "removed").await,
        0,
        "FsOps supplies its own semantic removal total"
    );
}

#[tokio::test]
#[ignore = "100,000-row statistics throughput measurement"]
async fn statistics_bulk_costs() {
    let temp = Db::open_temp().await.unwrap();
    let db = temp.handle();
    let volume = insert_volume(&db, "photos").await;
    sqlx::query("UPDATE volumes SET last_scan_at=1")
        .execute(db.write())
        .await
        .unwrap();
    for enabled in [false, true] {
        let mut tx = db.begin_write().await.unwrap();
        if !enabled {
            sqlx::query("DROP TRIGGER statistics_file_insert")
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        let start = std::time::Instant::now();
        sqlx::query("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<100000) INSERT INTO files(volume_id,dir_path,name,name_norm,stem_norm,ext,kind,size,mtime) SELECT ?,'',printf('%d.jpg',i),printf('%d.jpg',i),printf('%d',i),'jpg','image',1024,1 FROM n")
            .bind(volume).execute(&mut *tx).await.unwrap();
        eprintln!("100k insert statistics={enabled}: {:?}", start.elapsed());
        if enabled {
            tx.commit().await.unwrap();
        } else {
            tx.rollback().await.unwrap();
        }
    }
    let config = crate::config::Config::for_tests(std::path::Path::new("target"));
    let settings = SettingsService::load(db.clone(), &config).await.unwrap();
    let service = StatisticsService::new(db.clone(), settings).await.unwrap();
    let start = std::time::Instant::now();
    for _ in 0..10 {
        assert!(service.compact(now_secs()).await.unwrap());
    }
    assert!(!service.compact(now_secs()).await.unwrap());
    eprintln!("100k event compaction: {:?}", start.elapsed());
    assert_eq!(count(&db, "added").await, 100000);
    let start = std::time::Instant::now();
    let (_, _, inventory) = service.inventory().await.unwrap();
    eprintln!("100k inventory reader: {:?}", start.elapsed());
    assert_eq!(inventory[0].files, 100000);
}
