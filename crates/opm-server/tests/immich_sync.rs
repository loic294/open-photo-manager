//! The asset sync against the stateful fake Immich: what it asks for, what it stores in
//! `immich_assets` and `immich_sync_state`, what the status and the `in_immich` badge say, and how
//! it holds up when Immich pages oddly, changes, fails or disappears.
//!
//! The rows are checked with plain SQL (see `immich_support/sync.rs`); `immich_sync_control.rs`
//! covers the triggers (the endpoint, coalescing, cancelling, the loop).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use std::collections::BTreeMap;
use std::time::Duration;

use axum::http::StatusCode;
use immich_support::{Fixture, START_MS, entry, rfc3339, updated_after};
use opm_core::dto::{TaskKind, TaskState};
use serde_json::{Value, json};

const PREFIX: &str = "/mnt/photos";
const SKEW_MS: i64 = 5 * 60 * 1000;
const HOUR: Duration = Duration::from_secs(60 * 60);
const HOUR_MS: i64 = 60 * 60 * 1000;

/// An administrator connection on a server of the given version (`""` is a server that does not
/// say), one library "Photos" and one volume "photos" mapped to it at [`PREFIX`]; returns the
/// fixture, the id of the library and the id of the volume.
async fn setup_on(version: &str) -> (Fixture, String, i64) {
    let fx = Fixture::new().await;
    fx.fake.state().version = version.to_owned();
    fx.connect_ok().await;
    let (library, volume) = fx.map_library("Photos", "photos", PREFIX).await;
    (fx, library, volume)
}

async fn setup() -> (Fixture, String, i64) {
    setup_on(immich_support::VERSION).await
}

/// The same for an account that is not an administrator: no library, the mapping names a path.
async fn setup_limited_on(version: &str) -> (Fixture, String, i64) {
    let fx = Fixture::limited().await;
    fx.fake.state().version = version.to_owned();
    fx.connect_ok().await;
    // Immich still has a library; the account just cannot see or name it.
    let library = fx.fake.add_library("Photos", &[PREFIX]);
    let volume = fx.volume("photos").await;
    fx.map(vec![entry(volume, None, PREFIX)]).await;
    (fx, library, volume)
}

fn badges(entries: &[(&str, bool)]) -> BTreeMap<String, bool> {
    entries
        .iter()
        .map(|(name, flag)| ((*name).to_owned(), *flag))
        .collect()
}

/// Every search body of the fake that has no `filter` and no `cursor` (the flat generation).
fn flat(body: &Value) -> bool {
    body.get("filter").is_none() && body.get("cursor").is_none()
}

// ----------------------------------------------------------------------------
// What the first sync reads and stores
// ----------------------------------------------------------------------------

#[tokio::test]
async fn the_first_sync_stores_every_asset_with_its_location_and_counts_them() {
    let (fx, library, volume) = setup().await;
    let live = fx.fake.add_asset(&library, "/mnt/photos/2019/IMG_1.jpg");
    let offline = fx
        .fake
        .add_asset_with(&library, "/mnt/photos/2019/IMG_2.jpg", |a| {
            a.is_offline = true;
        });
    let trashed = fx
        .fake
        .add_asset_with(&library, "/mnt/photos/2020/summer/IMG_3.jpg", |a| {
            a.is_trashed = true;
        });
    let root = fx.fake.add_asset(&library, "/mnt/photos/root.jpg");
    let ghost = fx.fake.add_asset(&library, "/mnt/photos/2021/ghost.jpg");
    let outside = fx.fake.add_asset(&library, "/mnt/elsewhere/x.jpg");
    let locked = fx
        .fake
        .add_asset_with(&library, "/mnt/photos/locked.jpg", |a| {
            a.visibility = "locked".into();
        });
    for (dir, name) in [
        ("2019", "IMG_1.jpg"),
        ("2019", "IMG_2.jpg"),
        ("2020/summer", "IMG_3.jpg"),
        ("", "root.jpg"),
    ] {
        fx.index_file(volume, dir, name).await;
    }

    let before = fx.status(false).await;
    assert_eq!(before["sync"], Value::Null);
    assert_eq!(before["last_sync_at"], Value::Null);

    let task = fx.sync_ok(false).await;

    assert_eq!(task.kind, TaskKind::ImmichSync);
    assert_eq!(task.title, "Syncing with Immich");
    assert_eq!(task.items_done, 6);
    assert_eq!(fx.asset_count().await, 6);
    assert!(
        fx.stored(&locked).await.is_none(),
        "a locked asset is out of scope"
    );

    let row = fx.row(&live).await;
    assert_eq!(row.scope, library);
    assert_eq!(row.library_id.as_deref(), Some(library.as_str()));
    assert_eq!(row.original_path, "/mnt/photos/2019/IMG_1.jpg");
    assert_eq!(row.location(), Some((volume, "2019", "IMG_1.jpg")));
    assert!(!row.is_offline && !row.is_trashed);
    assert_eq!(row.updated_at, START_MS);
    assert_eq!(row.sync_run, 1);

    let row = fx.row(&offline).await;
    assert!(row.is_offline && !row.is_trashed);
    assert_eq!(row.location(), Some((volume, "2019", "IMG_2.jpg")));
    let row = fx.row(&trashed).await;
    assert!(row.is_trashed && !row.is_offline);
    assert_eq!(row.location(), Some((volume, "2020/summer", "IMG_3.jpg")));
    assert_eq!(
        fx.row(&root).await.location(),
        Some((volume, "", "root.jpg"))
    );
    assert_eq!(
        fx.row(&ghost).await.location(),
        Some((volume, "2021", "ghost.jpg"))
    );
    let row = fx.row(&outside).await;
    assert_eq!(row.location(), None, "outside every mapped prefix");
    assert_eq!(row.original_path, "/mnt/elsewhere/x.jpg");

    let scope = fx.scope(&library).await;
    assert_eq!(
        (
            scope.assets_total,
            scope.assets_live,
            scope.assets_matched,
            scope.assets_unmapped
        ),
        (6, 4, 4, 1)
    );
    assert_eq!(scope.run, 1);
    assert_eq!(scope.last_error, None);
    assert_eq!(scope.consecutive_failures, 0);
    assert!(scope.last_sync_at.is_some() && scope.last_full_at.is_some());
    assert!(
        scope.high_water_ms <= START_MS && scope.high_water_ms > START_MS - 60_000,
        "the mark is the newest updatedAt less the time the pass took: {}",
        scope.high_water_ms
    );

    let status = fx.status(false).await;
    let sync = &status["sync"];
    assert_eq!(sync["assets_total"], 6);
    assert_eq!(sync["assets_live"], 4);
    assert_eq!(sync["assets_matched"], 4);
    assert_eq!(sync["assets_unmapped"], 1);
    assert_eq!(sync["last_error"], Value::Null);
    assert_eq!(sync["last_sync_at"], json!(scope.last_sync_at));
    assert_eq!(sync["last_full_at"], json!(scope.last_full_at));
    assert_eq!(status["last_sync_at"], json!(scope.last_sync_at));
    assert_eq!(
        sync["scopes"],
        json!([{
            "scope": library,
            "library_id": library,
            "library_name": "Photos",
            "path_prefix": null,
            "assets_total": 6,
            "assets_live": 4,
            "assets_matched": 4,
            "assets_unmapped": 1,
            "last_sync_at": scope.last_sync_at,
            "last_full_at": scope.last_full_at,
            "last_error": null,
            "consecutive_failures": 0,
        }])
    );
    assert_eq!(fx.task_result(task.id).await, Some(sync.clone()));
    fx.assert_no_leak();
}

#[tokio::test]
async fn a_server_of_the_typed_generation_is_read_in_one_filtered_stream() {
    let (fx, library, _) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.fake.add_asset_with(&library, "/mnt/photos/b.jpg", |a| {
        a.visibility = "archive".into();
    });
    fx.fake.add_asset_with(&library, "/mnt/photos/c.jpg", |a| {
        a.visibility = "hidden".into();
    });

    fx.sync_ok(false).await;

    assert_eq!(
        fx.fake.search_bodies(),
        vec![json!({
            "size": 1000,
            "orderBy": { "field": "fileCreatedAt", "direction": "asc" },
            "filter": { "libraryId": { "eq": library } },
        })],
        "no trash, offline, visibility or time condition: everything the account can see"
    );
    assert_eq!(
        fx.asset_count().await,
        3,
        "archived and hidden assets are stored too"
    );
}

#[tokio::test]
async fn the_in_immich_badge_follows_the_live_assets_at_a_location() {
    let (fx, library, volume) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/2019/IMG_1.jpg");
    fx.fake
        .add_asset_with(&library, "/mnt/photos/2019/IMG_2.jpg", |a| {
            a.is_offline = true;
        });
    fx.fake
        .add_asset_with(&library, "/mnt/photos/2020/IMG_3.jpg", |a| {
            a.is_trashed = true;
        });
    fx.fake.add_asset(&library, "/mnt/photos/root.jpg");
    for (dir, name) in [
        ("2019", "IMG_1.jpg"),
        ("2019", "IMG_2.jpg"),
        // Same name in another case, and the same name in another directory.
        ("2019", "img_1.jpg"),
        ("2018", "IMG_1.jpg"),
        ("2020", "IMG_3.jpg"),
        ("", "root.jpg"),
        ("", "other.jpg"),
    ] {
        fx.index_file(volume, dir, name).await;
    }
    assert_eq!(
        fx.badges(volume, "2019").await,
        badges(&[
            ("IMG_1.jpg", false),
            ("IMG_2.jpg", false),
            ("img_1.jpg", false)
        ]),
        "nothing is in Immich before the first sync"
    );

    fx.sync_ok(false).await;

    assert_eq!(
        fx.badges(volume, "2019").await,
        badges(&[
            ("IMG_1.jpg", true),
            ("IMG_2.jpg", false),
            ("img_1.jpg", false)
        ])
    );
    assert_eq!(
        fx.badges(volume, "2018").await,
        badges(&[("IMG_1.jpg", false)])
    );
    assert_eq!(
        fx.badges(volume, "2020").await,
        badges(&[("IMG_3.jpg", false)])
    );
    assert_eq!(
        fx.badges(volume, "").await,
        badges(&[("other.jpg", false), ("root.jpg", true)])
    );
}

#[tokio::test]
async fn names_are_compared_byte_for_byte() {
    let (fx, library, volume) = setup().await;
    let composed = "caf\u{e9}.jpg";
    let decomposed = "cafe\u{301}.jpg";
    fx.fake.add_asset(&library, &format!("{PREFIX}/{composed}"));
    fx.index_file(volume, "", composed).await;
    fx.index_file(volume, "", decomposed).await;

    fx.sync_ok(false).await;

    assert_eq!(
        fx.badges(volume, "").await,
        badges(&[(composed, true), (decomposed, false)])
    );
    let rows = fx.assets().await;
    assert_eq!(rows[0].name.as_deref(), Some(composed));
}

#[tokio::test]
async fn a_path_that_only_shares_the_text_of_the_prefix_is_not_under_it() {
    let fx = Fixture::connected().await;
    let library = fx
        .fake
        .add_library("Photos", &["/mnt/photo", "/mnt/photos"]);
    let volume = fx.volume("photo").await;
    fx.map(vec![entry(volume, Some(&library), "/mnt/photo")])
        .await;
    let inside = fx.fake.add_asset(&library, "/mnt/photo/a.jpg");
    let beside = fx.fake.add_asset(&library, "/mnt/photos/b.jpg");
    fx.index_file(volume, "", "a.jpg").await;
    fx.index_file(volume, "", "b.jpg").await;
    fx.index_file(volume, "s", "b.jpg").await;

    fx.sync_ok(false).await;

    assert_eq!(
        fx.row(&inside).await.location(),
        Some((volume, "", "a.jpg"))
    );
    let beside = fx.row(&beside).await;
    assert_eq!(beside.location(), None);
    assert_eq!(beside.original_path, "/mnt/photos/b.jpg");
    assert_eq!(
        fx.badges(volume, "").await,
        badges(&[("a.jpg", true), ("b.jpg", false)])
    );
    assert_eq!(fx.badges(volume, "s").await, badges(&[("b.jpg", false)]));
    let scope = fx.scope(&library).await;
    assert_eq!((scope.assets_total, scope.assets_unmapped), (2, 1));
}

// ----------------------------------------------------------------------------
// Passes: incremental, full, failed
// ----------------------------------------------------------------------------

#[tokio::test]
async fn a_second_sync_changes_nothing_but_the_run() {
    let (fx, library, volume) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/2019/IMG_1.jpg");
    fx.fake
        .add_asset_with(&library, "/mnt/photos/2019/IMG_2.jpg", |a| {
            a.is_offline = true;
        });
    fx.index_file(volume, "2019", "IMG_1.jpg").await;
    fx.sync_ok(false).await;
    let first = fx.assets().await;
    let scope = fx.scope(&library).await;

    let task = fx.sync_ok(false).await;

    let second = fx.assets().await;
    assert_eq!(second.len(), first.len());
    for (before, after) in first.iter().zip(&second) {
        assert_eq!(after.sync_run, 2, "every row was read again: {after:?}");
        let mut same = after.clone();
        same.sync_run = before.sync_run;
        assert_eq!(&same, before);
    }
    let after = fx.scope(&library).await;
    assert_eq!(after.run, 2);
    assert_eq!(after.fingerprint, scope.fingerprint);
    assert_eq!(
        after.last_full_at, scope.last_full_at,
        "this pass was not full"
    );
    assert_eq!(after.assets_total, scope.assets_total);
    assert_eq!(task.items_done, 2);
    assert_eq!(
        fx.badges(volume, "2019").await,
        badges(&[("IMG_1.jpg", true)])
    );

    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(updated_after(&bodies[0]), None, "the first pass is full");
    assert_eq!(
        updated_after(&bodies[1]),
        Some(rfc3339(scope.high_water_ms - SKEW_MS)),
        "five minutes of skew before the mark"
    );
}

#[tokio::test]
async fn an_incremental_sync_reads_only_what_changed_and_never_deletes() {
    let (fx, library, volume) = setup().await;
    let old: Vec<String> = (0..3)
        .map(|n| {
            fx.fake
                .add_asset_with(&library, &format!("/mnt/photos/old/{n}.jpg"), |a| {
                    a.updated_at_ms = START_MS - HOUR_MS;
                })
        })
        .collect();
    let newest = fx.fake.add_asset(&library, "/mnt/photos/new/newest.jpg");
    for n in 0..3 {
        fx.index_file(volume, "old", &format!("{n}.jpg")).await;
    }
    fx.sync_ok(false).await;
    let first = fx.scope(&library).await;
    assert_eq!(first.assets_total, 4);

    fx.fake.advance(HOUR);
    fx.fake.update_asset(&old[0], |a| a.is_trashed = true);
    let added = fx.fake.add_asset(&library, "/mnt/photos/new/added.jpg");
    fx.fake.remove_asset(&old[1]);
    fx.fake.clear_calls();
    let task = fx.sync_ok(false).await;

    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        updated_after(&bodies[0]),
        Some(rfc3339(first.high_water_ms - SKEW_MS))
    );
    assert_eq!(
        task.items_done, 3,
        "the trashed one, the new one and the newest of the first pass"
    );
    let trashed = fx.row(&old[0]).await;
    assert!(trashed.is_trashed);
    assert_eq!(trashed.sync_run, 2);
    assert_eq!(trashed.updated_at, START_MS + HOUR_MS);
    assert_eq!(fx.row(&added).await.sync_run, 2);
    assert_eq!(
        fx.row(&newest).await.sync_run,
        2,
        "within five minutes of the mark, so read again"
    );
    assert_eq!(fx.row(&old[2]).await.sync_run, 1, "unchanged and not read");
    assert!(
        fx.stored(&old[1]).await.is_some(),
        "an incremental pass sees changes, not deletions"
    );

    let after = fx.scope(&library).await;
    assert_eq!(after.run, 2);
    assert_eq!(after.last_full_at, first.last_full_at);
    assert!(
        after.high_water_ms > first.high_water_ms + HOUR_MS - 60_000,
        "the mark follows the newest change: {}",
        after.high_water_ms
    );
    assert_eq!(after.assets_total, 5);
    assert_eq!(after.assets_live, 4);
    assert_eq!(
        fx.badges(volume, "old").await,
        badges(&[("0.jpg", false), ("1.jpg", true), ("2.jpg", true)]),
        "the trashed asset lost its badge at once; the vanished one keeps it until a full pass"
    );
}

#[tokio::test]
async fn a_full_sync_deletes_what_immich_no_longer_has() {
    let (fx, library, volume) = setup().await;
    let kept = fx.fake.add_asset(&library, "/mnt/photos/kept.jpg");
    let gone = fx.fake.add_asset(&library, "/mnt/photos/gone.jpg");
    fx.index_file(volume, "", "kept.jpg").await;
    fx.index_file(volume, "", "gone.jpg").await;
    fx.sync_ok(false).await;
    fx.fake.remove_asset(&gone);

    fx.sync_ok(false).await;
    assert!(fx.stored(&gone).await.is_some());

    fx.fake.clear_calls();
    fx.sync_ok(true).await;

    assert!(fx.stored(&gone).await.is_none());
    assert_eq!(fx.row(&kept).await.sync_run, 3);
    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(updated_after(&bodies[0]), None);
    let scope = fx.scope(&library).await;
    assert_eq!((scope.assets_total, scope.assets_matched), (1, 1));
    assert_eq!(
        fx.badges(volume, "").await,
        badges(&[("gone.jpg", false), ("kept.jpg", true)])
    );
}

#[tokio::test]
async fn the_daily_full_pass_deletes_what_immich_no_longer_has() {
    let (fx, library, _) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/kept.jpg");
    let gone = fx.fake.add_asset(&library, "/mnt/photos/gone.jpg");
    fx.sync_ok(false).await;
    fx.fake.remove_asset(&gone);

    fx.age_full_syncs(23 * 60 * 60).await;
    fx.sync_ok(false).await;
    assert!(
        fx.stored(&gone).await.is_some(),
        "a full pass 23 hours ago is recent enough"
    );

    fx.age_full_syncs(2 * 60 * 60).await;
    fx.fake.clear_calls();
    fx.sync_ok(false).await;

    assert!(fx.stored(&gone).await.is_none());
    assert_eq!(updated_after(&fx.fake.search_bodies()[0]), None);
    let scope = fx.scope(&library).await;
    assert!(
        scope.last_full_at >= scope.last_sync_at,
        "the full pass is recorded as one"
    );
}

#[tokio::test]
async fn restart_continues_the_same_immich_run_from_its_saved_page() {
    let (fx, library, _) = setup().await;
    add_many(&fx, &library, 5);
    fx.fake.page_cap(Some(2));
    fx.fake.hold_searches_after(1, HOUR);
    let id = fx.submit_sync(true).await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fx.fake.search_bodies().len() >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let waiting_page = fx.fake.search_bodies()[1].clone();
    fx.app.state.begin_shutdown();
    assert_eq!(fx.finish(id).await.state, TaskState::Interrupted);
    fx.app.state.shutdown().await;
    let previous_requests = fx.fake.search_bodies().len();
    fx.fake.heal();
    let recovered = opm_server::state::AppState::new((*fx.app.state.config).clone())
        .await
        .unwrap();
    let task = tokio::time::timeout(Duration::from_secs(15), recovered.tasks.wait_finished(id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Done, "{task:?}");
    assert_eq!(fx.fake.search_bodies()[previous_requests], waiting_page);
    let runs: Vec<i64> = sqlx::query_scalar("SELECT DISTINCT sync_run FROM immich_assets")
        .fetch_all(recovered.db.read())
        .await
        .unwrap();
    assert_eq!(runs, vec![1]);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM immich_assets")
        .fetch_one(recovered.db.read())
        .await
        .unwrap();
    assert_eq!(count, 5);
    recovered.begin_shutdown();
    recovered.shutdown().await;
}

#[tokio::test]
async fn a_full_sync_that_fails_halfway_deletes_nothing() {
    let (fx, library, _) = setup().await;
    let ids: Vec<String> = (0..5)
        .map(|n| fx.fake.add_asset(&library, &format!("/mnt/photos/{n}.jpg")))
        .collect();
    fx.sync_ok(false).await;
    let before = fx.scope(&library).await;
    fx.fake.remove_asset(&ids[4]);
    fx.fake.page_cap(Some(2));
    fx.fake.fail_search_after(1, 500);

    let task = fx.sync(true).await;

    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.expect("a failed task says why");
    assert!(error.contains("500"), "{error}");
    assert_eq!(fx.asset_count().await, 5, "nothing was deleted");
    assert_eq!(fx.row(&ids[0]).await.sync_run, 2, "the first page was kept");
    assert_eq!(fx.row(&ids[2]).await.sync_run, 1);
    let scope = fx.scope(&library).await;
    assert_eq!(scope.consecutive_failures, 1);
    assert_eq!(scope.last_error.as_deref(), Some(error.as_str()));
    assert_eq!(scope.last_sync_at, before.last_sync_at);
    assert_eq!(scope.last_full_at, before.last_full_at);
    assert_eq!(scope.high_water_ms, before.high_water_ms);
    assert_eq!(
        scope.assets_total, 5,
        "the counts are those of the last good pass"
    );
    let status = fx.status(false).await;
    assert_eq!(status["sync"]["last_error"], json!(error));
    assert_eq!(status["sync"]["scopes"][0]["consecutive_failures"], 1);

    fx.fake.heal();
    fx.fake.page_cap(None);
    fx.sync_ok(true).await;

    assert_eq!(fx.asset_count().await, 4);
    assert!(fx.stored(&ids[4]).await.is_none());
    let scope = fx.scope(&library).await;
    assert_eq!(scope.consecutive_failures, 0);
    assert_eq!(scope.last_error, None);
    assert_eq!(fx.status(false).await["sync"]["last_error"], Value::Null);
}

// ----------------------------------------------------------------------------
// Pages
// ----------------------------------------------------------------------------

fn add_many(fx: &Fixture, library: &str, count: usize) {
    for n in 0..count {
        fx.fake
            .add_asset(library, &format!("/mnt/photos/bulk/{n:05}.jpg"));
    }
}

#[tokio::test]
async fn every_page_is_followed_by_its_cursor() {
    let (fx, library, _) = setup().await;
    add_many(&fx, &library, 2_500);

    let task = fx.sync_ok(false).await;

    assert_eq!(fx.asset_count().await, 2_500);
    assert_eq!(task.items_done, 2_500);
    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 3, "1000 + 1000 + 500");
    assert!(bodies[0].get("cursor").is_none());
    assert!(bodies[1]["cursor"].is_string() && bodies[2]["cursor"].is_string());
    assert_ne!(bodies[1]["cursor"], bodies[2]["cursor"]);
    assert!(bodies.iter().all(|body| body["size"] == 1000));
}

#[tokio::test]
async fn a_server_that_returns_less_than_asked_is_followed_to_its_end() {
    let (fx, library, _) = setup().await;
    add_many(&fx, &library, 2_500);
    fx.fake.page_cap(Some(300));

    fx.sync_ok(false).await;

    assert_eq!(fx.asset_count().await, 2_500);
    assert_eq!(
        fx.fake.search_bodies().len(),
        9,
        "8 pages of 300 and one of 100"
    );
}

#[tokio::test]
async fn an_empty_page_that_is_not_the_last_does_not_end_the_stream() {
    let (fx, library, _) = setup().await;
    add_many(&fx, &library, 1_200);
    fx.fake.stutter(true);

    fx.sync_ok(false).await;

    assert_eq!(fx.asset_count().await, 1_200);
    assert!(fx.fake.search_bodies().len() >= 4);
}

// ----------------------------------------------------------------------------
// Servers of the flat generation, and servers that do not say what they are
// ----------------------------------------------------------------------------

#[tokio::test]
async fn an_older_server_is_read_in_the_flat_style_with_a_pass_for_hidden_assets() {
    let (fx, library, _) = setup_on("v3.1.0").await;
    add_many(&fx, &library, 1_100);
    let archived = fx
        .fake
        .add_asset_with(&library, "/mnt/photos/archived.jpg", |a| {
            a.visibility = "archive".into();
        });
    let hidden = fx
        .fake
        .add_asset_with(&library, "/mnt/photos/motion.mp4", |a| {
            a.visibility = "hidden".into();
        });
    let locked = fx
        .fake
        .add_asset_with(&library, "/mnt/photos/locked.jpg", |a| {
            a.visibility = "locked".into();
        });
    let trashed = fx
        .fake
        .add_asset_with(&library, "/mnt/photos/trashed.jpg", |a| {
            a.is_trashed = true;
        });

    let task = fx.sync_ok(false).await;

    let bodies = fx.fake.search_bodies();
    assert!(
        bodies.iter().all(flat),
        "no typed filter and no cursor: {bodies:?}"
    );
    assert_eq!(bodies.len(), 3, "two pages, then the hidden assets");
    let main = |page: i64| {
        json!({
            "size": 1000,
            "page": page,
            "order": "asc",
            "libraryId": library,
            "withDeleted": true,
        })
    };
    assert_eq!(bodies[0], main(1));
    assert_eq!(bodies[1], main(2));
    let mut hidden_search = main(1);
    hidden_search["visibility"] = json!("hidden");
    assert_eq!(bodies[2], hidden_search);

    assert_eq!(fx.asset_count().await, 1_103);
    assert!(fx.stored(&archived).await.is_some());
    assert!(fx.stored(&hidden).await.is_some());
    assert!(fx.stored(&trashed).await.is_some_and(|row| row.is_trashed));
    assert!(fx.stored(&locked).await.is_none());
    assert_eq!(task.items_done, 1_103);
    assert_eq!(fx.scope(&library).await.assets_total, 1_103);
}

#[tokio::test]
async fn a_server_that_does_not_say_its_version_gets_the_hidden_pass_and_each_asset_is_stored_once()
{
    let (fx, library, _) = setup_on("").await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    let hidden = fx
        .fake
        .add_asset_with(&library, "/mnt/photos/motion.mp4", |a| {
            a.visibility = "hidden".into();
        });

    fx.sync_ok(false).await;

    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 2, "the typed search, then the hidden assets");
    assert_eq!(bodies[1]["filter"]["visibility"], json!({ "eq": "hidden" }));
    assert_eq!(fx.asset_count().await, 2);
    assert!(fx.stored(&hidden).await.is_some());
    assert_eq!(fx.scope(&library).await.assets_total, 2);
}

// ----------------------------------------------------------------------------
// A limited account reads by path prefix
// ----------------------------------------------------------------------------

#[tokio::test]
async fn a_limited_account_reads_the_assets_under_the_mapped_prefix() {
    let (fx, library, volume) = setup_limited_on(immich_support::VERSION).await;
    let inside = fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.fake.add_asset(&library, "/mnt/photos2/b.jpg");
    fx.fake.add_asset(&library, "/backup/mnt/photos/c.jpg");
    fx.index_file(volume, "", "a.jpg").await;
    fx.fake.clear_calls();

    fx.sync_ok(false).await;

    let key = format!("path:{PREFIX}");
    assert_eq!(
        fx.fake.search_bodies(),
        vec![json!({
            "size": 1000,
            "orderBy": { "field": "fileCreatedAt", "direction": "asc" },
            "filter": { "originalPath": { "startsWith": "/mnt/photos/" } },
        })]
    );
    assert_eq!(
        fx.fake.count("GET", "/api/libraries"),
        0,
        "a limited account cannot list libraries"
    );
    assert_eq!(fx.asset_count().await, 1);
    let row = fx.row(&inside).await;
    assert_eq!(row.scope, key);
    assert_eq!(row.location(), Some((volume, "", "a.jpg")));
    assert_eq!(fx.badges(volume, "").await, badges(&[("a.jpg", true)]));
    let scope = fx.scope(&key).await;
    assert_eq!((scope.assets_total, scope.assets_matched), (1, 1));
    assert_eq!(
        fx.status(false).await["sync"]["scopes"][0],
        json!({
            "scope": key,
            "library_id": null,
            "library_name": null,
            "path_prefix": PREFIX,
            "assets_total": 1,
            "assets_live": 1,
            "assets_matched": 1,
            "assets_unmapped": 0,
            "last_sync_at": scope.last_sync_at,
            "last_full_at": scope.last_full_at,
            "last_error": null,
            "consecutive_failures": 0,
        })
    );
}

#[tokio::test]
async fn a_limited_account_on_an_older_server_drops_the_substring_matches() {
    let (fx, library, volume) = setup_limited_on("v3.1.0").await;
    let inside = fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.fake.add_asset(&library, "/backup/mnt/photos/c.jpg");
    fx.fake.add_asset(&library, "/mnt/photos2/b.jpg");

    fx.sync_ok(false).await;

    let bodies = fx.fake.search_bodies();
    assert!(bodies.iter().all(flat), "{bodies:?}");
    assert_eq!(bodies[0]["originalPath"], "/mnt/photos/");
    assert!(bodies[0].get("libraryId").is_none());
    assert_eq!(
        fx.asset_count().await,
        1,
        "the server matched a substring; only the asset under the prefix is kept"
    );
    assert_eq!(
        fx.row(&inside).await.location(),
        Some((volume, "", "a.jpg"))
    );
}

// ----------------------------------------------------------------------------
// Several scopes, and what changes under a scope
// ----------------------------------------------------------------------------

#[tokio::test]
async fn each_mapped_library_is_a_scope_of_its_own() {
    let (fx, photos, photo_volume) = setup().await;
    let videos = fx.fake.add_library("Videos", &["/mnt/videos"]);
    let video_volume = fx.volume("videos").await;
    fx.map(vec![
        entry(photo_volume, Some(&photos), PREFIX),
        entry(video_volume, Some(&videos), "/mnt/videos"),
    ])
    .await;
    fx.fake.add_asset(&photos, "/mnt/photos/a.jpg");
    fx.fake.add_asset(&photos, "/mnt/photos/b.jpg");
    let clip = fx.fake.add_asset(&videos, "/mnt/videos/clip.mp4");
    fx.index_file(video_volume, "", "clip.mp4").await;
    fx.fake.clear_calls();

    let task = fx.sync_ok(false).await;

    assert_eq!(task.items_done, 3);
    assert_eq!(fx.fake.count("GET", "/api/libraries"), 1);
    let libraries: Vec<String> = fx
        .fake
        .search_bodies()
        .iter()
        .map(|body| {
            body["filter"]["libraryId"]["eq"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(libraries.len(), 2);
    assert!(libraries.contains(&photos) && libraries.contains(&videos));
    assert_eq!(fx.scopes().await.len(), 2);
    assert_eq!(fx.scope(&photos).await.assets_total, 2);
    assert_eq!(fx.scope(&videos).await.assets_total, 1);
    let row = fx.row(&clip).await;
    assert_eq!(row.scope, videos);
    assert_eq!(row.location(), Some((video_volume, "", "clip.mp4")));

    let status = fx.status(false).await;
    assert_eq!(status["sync"]["assets_total"], 3);
    assert_eq!(status["sync"]["assets_matched"], 1);
    assert_eq!(status["sync"]["scopes"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_library_that_no_longer_exists_fails_only_its_own_scope() {
    let (fx, photos, _) = setup().await;
    let videos = fx.fake.add_library("Videos", &["/mnt/videos"]);
    let video_volume = fx.volume("videos").await;
    let photo_volume = fx.volume("photos").await;
    fx.map(vec![
        entry(photo_volume, Some(&photos), PREFIX),
        entry(video_volume, Some(&videos), "/mnt/videos"),
    ])
    .await;
    fx.fake.add_asset(&photos, "/mnt/photos/a.jpg");
    fx.fake.add_asset(&videos, "/mnt/videos/clip.mp4");
    fx.fake
        .state()
        .libraries
        .retain(|library| library.id != videos);
    fx.fake.clear_calls();

    let task = fx.sync(false).await;

    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.unwrap();
    assert!(
        error.contains("Videos") && error.contains("no longer exists"),
        "{error}"
    );
    assert_eq!(fx.asset_count().await, 1, "the other library was read");
    assert_eq!(fx.scope(&photos).await.last_error, None);
    let failed = fx.scope(&videos).await;
    assert_eq!(failed.last_error.as_deref(), Some(error.as_str()));
    assert_eq!(failed.consecutive_failures, 1);
    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 1, "no search for the library that is gone");
    assert_eq!(bodies[0]["filter"]["libraryId"]["eq"], json!(photos));
}

#[tokio::test]
async fn editing_the_mappings_reads_everything_again_and_follows_the_new_volume() {
    let fx = Fixture::connected().await;
    let library = fx.fake.add_library("Photos", &["/mnt/photos", "/mnt/more"]);
    let photos = fx.volume("photos").await;
    fx.map(vec![entry(photos, Some(&library), PREFIX)]).await;
    let farther = fx.fake.add_asset(&library, "/mnt/more/a.jpg");
    fx.fake.add_asset(&library, "/mnt/photos/b.jpg");
    let more = fx.volume("more").await;
    fx.index_file(more, "", "a.jpg").await;
    fx.sync_ok(false).await;
    let first = fx.scope(&library).await;
    assert_eq!(fx.row(&farther).await.location(), None);
    assert_eq!(fx.badges(more, "").await, badges(&[("a.jpg", false)]));

    fx.map(vec![
        entry(photos, Some(&library), PREFIX),
        entry(more, Some(&library), "/mnt/more"),
    ])
    .await;
    fx.fake.clear_calls();
    fx.sync_ok(false).await;

    let bodies = fx.fake.search_bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        updated_after(&bodies[0]),
        None,
        "new mappings, so a full read"
    );
    assert_eq!(fx.row(&farther).await.location(), Some((more, "", "a.jpg")));
    assert_eq!(fx.badges(more, "").await, badges(&[("a.jpg", true)]));
    let scope = fx.scope(&library).await;
    assert_ne!(scope.fingerprint, first.fingerprint);
    assert_eq!((scope.assets_total, scope.assets_unmapped), (2, 0));
    assert_eq!(fx.scopes().await.len(), 1);
}

#[tokio::test]
async fn removing_the_mappings_drops_what_was_synced() {
    let (fx, library, volume) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.index_file(volume, "", "a.jpg").await;
    fx.sync_ok(false).await;
    assert_eq!(fx.badges(volume, "").await, badges(&[("a.jpg", true)]));

    fx.map(Vec::new()).await;

    assert_eq!(fx.status(false).await["sync"], Value::Null);
    assert!(
        fx.app
            .state
            .immich_sync
            .submit_if_due()
            .await
            .unwrap()
            .is_none(),
        "nothing is mapped, so there is nothing to sync"
    );
    assert_eq!(fx.asset_count().await, 0);
    assert!(fx.scopes().await.is_empty());
    assert_eq!(fx.badges(volume, "").await, badges(&[("a.jpg", false)]));
    let (status, body) = fx.start_sync(None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

// ----------------------------------------------------------------------------
// When Immich cannot be read
// ----------------------------------------------------------------------------

#[tokio::test]
async fn an_unreachable_immich_fails_the_task_and_keeps_what_was_synced() {
    let (mut fx, library, volume) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.index_file(volume, "", "a.jpg").await;
    fx.sync_ok(false).await;
    let before = fx.scope(&library).await;
    fx.fake.stop().await;

    let task = fx.sync(false).await;

    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.unwrap();
    assert!(error.contains("unreachable"), "{error}");
    assert_eq!(fx.asset_count().await, 1);
    assert_eq!(fx.badges(volume, "").await, badges(&[("a.jpg", true)]));
    let scope = fx.scope(&library).await;
    assert_eq!(scope.last_error.as_deref(), Some(error.as_str()));
    assert_eq!(scope.consecutive_failures, 1);
    assert_eq!(scope.last_sync_at, before.last_sync_at);

    let task = fx.sync(false).await;

    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(fx.scope(&library).await.consecutive_failures, 2);
    let status = fx.status(false).await;
    assert_eq!(status["sync"]["last_error"], json!(error));
    assert_eq!(status["needs_reconnect"], false);
    assert_eq!(
        status["sync"]["assets_total"], 1,
        "the last good counts stay"
    );
}

#[tokio::test]
async fn a_key_that_immich_rejects_fails_the_sync_and_asks_to_connect_again() {
    let (fx, library, volume) = setup().await;
    fx.fake.add_asset(&library, "/mnt/photos/a.jpg");
    fx.index_file(volume, "", "a.jpg").await;
    fx.sync_ok(false).await;
    fx.fake.revoke_all_keys();

    let task = fx.sync(false).await;

    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.unwrap();
    assert!(error.contains("rejected the API key"), "{error}");
    assert_eq!(fx.asset_count().await, 1);
    assert_eq!(fx.scope(&library).await.consecutive_failures, 1);
    let status = fx.status(false).await;
    assert_eq!(status["needs_reconnect"], true);
    assert_eq!(status["sync"]["last_error"], json!(error));

    // The status already asks for a new connection; the sync is not refused on top of that, it
    // tries again and says the same.
    let task = fx.sync(false).await;
    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(task.error.as_deref(), Some(error.as_str()));
    assert_eq!(fx.scope(&library).await.consecutive_failures, 2);
    assert_eq!(fx.asset_count().await, 1);
}

#[tokio::test]
async fn a_scope_that_fails_does_not_stop_the_others() {
    let (fx, photos, photo_volume) = setup().await;
    let videos = fx.fake.add_library("Videos", &["/mnt/videos"]);
    let video_volume = fx.volume("videos").await;
    fx.map(vec![
        entry(photo_volume, Some(&photos), PREFIX),
        entry(video_volume, Some(&videos), "/mnt/videos"),
    ])
    .await;
    fx.fake.add_asset(&photos, "/mnt/photos/a.jpg");
    fx.fake.add_asset(&videos, "/mnt/videos/clip.mp4");
    fx.fake.clear_calls();
    fx.fake.fail_search_after(1, 500);

    let task = fx.sync(false).await;

    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.unwrap();
    assert!(error.contains("500"), "{error}");
    assert!(!error.contains("more"), "only one scope failed: {error}");
    assert_eq!(fx.fake.count("POST", "/api/search/metadata"), 2);
    let scopes = fx.scopes().await;
    let failed: Vec<_> = scopes.iter().filter(|s| s.last_error.is_some()).collect();
    let read: Vec<_> = scopes.iter().filter(|s| s.last_sync_at.is_some()).collect();
    assert_eq!((failed.len(), read.len()), (1, 1), "{scopes:?}");
    assert_eq!(failed[0].consecutive_failures, 1);
    assert_eq!(failed[0].last_error.as_deref(), Some(error.as_str()));
    assert_eq!(read[0].consecutive_failures, 0);
    assert_eq!(
        fx.asset_count().await,
        1,
        "what the good scope read is kept"
    );

    fx.fake.heal();
    let task = fx.sync_ok(false).await;

    assert_eq!(task.items_done, 2);
    assert_eq!(fx.asset_count().await, 2);
    for scope in fx.scopes().await {
        assert_eq!(scope.consecutive_failures, 0, "{scope:?}");
        assert_eq!(scope.last_error, None);
    }
}

#[tokio::test]
async fn a_failure_every_scope_would_meet_is_recorded_on_each_and_stops_the_run() {
    let (fx, photos, photo_volume) = setup().await;
    let videos = fx.fake.add_library("Videos", &["/mnt/videos"]);
    let video_volume = fx.volume("videos").await;
    fx.map(vec![
        entry(photo_volume, Some(&photos), PREFIX),
        entry(video_volume, Some(&videos), "/mnt/videos"),
    ])
    .await;
    fx.fake.add_asset(&photos, "/mnt/photos/a.jpg");
    fx.fake.add_asset(&videos, "/mnt/videos/clip.mp4");
    fx.sync_ok(false).await;
    fx.fake.clear_calls();
    fx.fake.fail_search_after(0, 401);

    let task = fx.sync(false).await;

    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.unwrap();
    assert!(error.contains("rejected the API key"), "{error}");
    assert!(error.ends_with("(and 1 more)"), "{error}");
    assert_eq!(
        fx.fake.count("POST", "/api/search/metadata"),
        1,
        "the second scope was not tried against a key Immich rejects"
    );
    for scope in fx.scopes().await {
        assert_eq!(scope.consecutive_failures, 1, "{scope:?}");
        assert!(scope.last_error.is_some(), "{scope:?}");
    }
    assert_eq!(fx.asset_count().await, 2);
}

#[tokio::test]
async fn a_key_revoked_before_the_run_is_found_out_by_the_one_look_at_the_libraries() {
    let (fx, photos, photo_volume) = setup().await;
    let videos = fx.fake.add_library("Videos", &["/mnt/videos"]);
    let video_volume = fx.volume("videos").await;
    fx.map(vec![
        entry(photo_volume, Some(&photos), PREFIX),
        entry(video_volume, Some(&videos), "/mnt/videos"),
    ])
    .await;
    fx.fake.add_asset(&photos, "/mnt/photos/a.jpg");
    fx.fake.add_asset(&videos, "/mnt/videos/clip.mp4");
    fx.sync_ok(false).await;
    fx.fake.revoke_all_keys();
    fx.fake.clear_calls();

    let task = fx.sync(false).await;

    assert_eq!(task.state, TaskState::Failed);
    let error = task.error.unwrap();
    assert!(error.ends_with("(and 1 more)"), "{error}");
    assert_eq!(fx.fake.count("GET", "/api/libraries"), 1);
    assert_eq!(fx.fake.count("POST", "/api/search/metadata"), 0);
    for scope in fx.scopes().await {
        assert_eq!(scope.consecutive_failures, 1, "{scope:?}");
        assert!(scope.last_error.is_some(), "{scope:?}");
    }
    assert_eq!(fx.asset_count().await, 2);
}
