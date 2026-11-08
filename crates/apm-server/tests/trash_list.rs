//! `GET /api/trash` and `GET /api/trash/batches` over the real application: the page of items,
//! its order, the filters, the summary and the deletions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use apm_core::dto::{TaskKind, ValidationErrorBody};
use axum::http::StatusCode;
use common::fsops::{disk_dirs, disk_files};
use common::trash::{Row, TrashFx, ids_of, names_of};
use serde_json::{Value, json};

const BATCH: &str = "20240101-000000-t1";

/// `count` rows `n1.jpg`, `n2.jpg`, ... in volume `a`, trashed one second apart, so that the
/// newest has the highest id. Returns their ids, oldest first.
async fn sequence(fx: &TrashFx, count: i64) -> Vec<i64> {
    let mut ids = Vec::new();
    for n in 1..=count {
        let row = Row::new(fx.a, BATCH, &format!("n{n}.jpg")).at(1_000 + n);
        ids.push(fx.insert(row).await);
    }
    ids
}

fn field_names(body: &Value) -> Vec<String> {
    let body: ValidationErrorBody = serde_json::from_value(body.clone()).unwrap();
    body.error.fields.into_iter().map(|f| f.field).collect()
}

#[tokio::test]
async fn an_empty_trash_lists_nothing() {
    let fx = TrashFx::new().await;

    let listing = fx.list("").await;
    assert_eq!(listing["items"], json!([]));
    assert_eq!(listing["total"], json!(0));
    assert_eq!(
        listing["summary"],
        json!({
            "items": 0,
            "bytes": 0,
            "max_id": 0,
            "volumes": [],
            "retention_days": null,
            "conflict_policy": "smart",
        })
    );

    let batches = fx.batches("").await;
    assert_eq!(batches["batches"], json!([]));
    assert_eq!(batches["total"], json!(0));
    assert_eq!(batches["summary"], listing["summary"]);
}

#[tokio::test]
async fn items_come_newest_first_and_page_by_offset_and_limit() {
    let fx = TrashFx::new().await;
    let ids = sequence(&fx, 7).await;
    let newest_first: Vec<i64> = ids.iter().rev().copied().collect();

    let whole = fx.list("").await;
    assert_eq!(ids_of(&whole), newest_first);
    assert_eq!(whole["total"], json!(7));

    let mut pages = Vec::new();
    for offset in [0, 3, 6] {
        let page = fx.list(&format!("limit=3&offset={offset}")).await;
        assert_eq!(page["total"], json!(7), "offset {offset}");
        assert_eq!(page["summary"]["items"], json!(7), "offset {offset}");
        pages.extend(ids_of(&page));
    }
    assert_eq!(pages, newest_first);

    let past_the_end = fx.list("limit=3&offset=7").await;
    assert_eq!(past_the_end["items"], json!([]));
    assert_eq!(past_the_end["total"], json!(7));
}

#[tokio::test]
async fn an_item_carries_what_the_page_shows() {
    let fx = TrashFx::new().await;
    let id = fx
        .insert(
            Row::new(fx.a, BATCH, "2024/Trip/IMG_1.jpg")
                .size(1_234)
                .reason("user")
                .task(7)
                .at(1_700_000_123)
                .snapshot(),
        )
        .await;
    let folder = fx
        .insert(
            Row::new(fx.b, BATCH, "Album")
                .dir()
                .size(99)
                .at(1_700_000_100),
        )
        .await;

    let listing = fx.list("").await;
    assert_eq!(
        listing["items"],
        json!([
            {
                "id": id,
                "volume_id": fx.a,
                "volume": "a",
                "name": "IMG_1.jpg",
                "dir": "2024/Trip",
                "is_dir": false,
                "size": 1_234,
                "reason": "user",
                "batch": BATCH,
                "task_id": 7,
                "trashed_at": 1_700_000_123,
                "has_snapshot": true,
            },
            {
                "id": folder,
                "volume_id": fx.b,
                "volume": "b",
                "name": "Album",
                "dir": "",
                "is_dir": true,
                "size": 99,
                "reason": null,
                "batch": BATCH,
                "task_id": null,
                "trashed_at": 1_700_000_100,
                "has_snapshot": false,
            },
        ])
    );
}

#[tokio::test]
async fn items_trashed_by_fsops_are_listed_with_their_snapshot() {
    let fx = TrashFx::new().await;
    fx.ta.write("2024/Trip/IMG_1.jpg", b"indexed");
    fx.app
        .state
        .indexer
        .scan(
            &apm_server::indexer::ScanRequest::default(),
            &apm_server::indexer::RunContext::noop(),
        )
        .await
        .unwrap();
    fx.ta.write("2024/Trip/IMG_2.jpg", b"never indexed");
    let batch = apm_server::fsops::TrashBatch::new(42);
    let indexed = fx
        .trash_as_task(fx.a, "2024/Trip/IMG_1.jpg", &batch, "user", 42)
        .await;
    let unindexed = fx
        .trash_as_task(fx.a, "2024/Trip/IMG_2.jpg", &batch, "junk", 42)
        .await;

    let listing = fx.list("sort=name&order=asc").await;
    let items = listing["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], json!(indexed.trash_item_id));
    assert_eq!(items[0]["has_snapshot"], json!(true));
    assert_eq!(items[0]["reason"], json!("user"));
    assert_eq!(items[1]["id"], json!(unindexed.trash_item_id));
    assert_eq!(items[1]["has_snapshot"], json!(false));
    assert_eq!(items[1]["reason"], json!("junk"));
    for item in items {
        assert_eq!(item["batch"], json!(batch.name()));
        assert_eq!(item["task_id"], json!(42));
        assert_eq!(item["dir"], json!("2024/Trip"));
        assert_eq!(item["volume"], json!("a"));
    }
    assert_eq!(items[0]["size"], json!(7));
    assert_eq!(items[1]["size"], json!(13));
}

#[tokio::test]
async fn equal_times_are_ordered_by_id_in_the_direction_asked() {
    let fx = TrashFx::new().await;
    let mut ids = Vec::new();
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        ids.push(fx.insert(Row::new(fx.a, BATCH, name).at(5_000)).await);
    }

    let newest = fx.list("").await;
    assert_eq!(ids_of(&newest), vec![ids[2], ids[1], ids[0]]);
    let oldest = fx.list("order=asc").await;
    assert_eq!(ids_of(&oldest), vec![ids[0], ids[1], ids[2]]);
    let explicit = fx.list("sort=trashed_at&order=desc").await;
    assert_eq!(ids_of(&explicit), vec![ids[2], ids[1], ids[0]]);
}

#[tokio::test]
async fn sorting_by_size_puts_the_largest_first_unless_asked_otherwise() {
    let fx = TrashFx::new().await;
    let mut ids = Vec::new();
    for (name, size) in [("a", 10), ("b", 300), ("c", 20), ("d", 300)] {
        ids.push(fx.insert(Row::new(fx.a, BATCH, name).size(size)).await);
    }

    let largest = fx.list("sort=size").await;
    assert_eq!(ids_of(&largest), vec![ids[3], ids[1], ids[2], ids[0]]);
    let smallest = fx.list("sort=size&order=asc").await;
    assert_eq!(ids_of(&smallest), vec![ids[0], ids[2], ids[1], ids[3]]);
}

#[tokio::test]
async fn sorting_by_name_ignores_case_and_folders_then_breaks_ties_by_path() {
    let fx = TrashFx::new().await;
    let mut ids = Vec::new();
    for path in ["z/apple.jpg", "a/Banana.jpg", "m/cherry.jpg", "b/apple.jpg"] {
        ids.push(fx.insert(Row::new(fx.a, BATCH, path)).await);
    }

    let a_to_z = fx.list("sort=name").await;
    assert_eq!(
        names_of(&a_to_z),
        ["apple.jpg", "apple.jpg", "Banana.jpg", "cherry.jpg"]
    );
    assert_eq!(ids_of(&a_to_z), vec![ids[3], ids[0], ids[1], ids[2]]);

    let z_to_a = fx.list("sort=name&order=desc").await;
    assert_eq!(ids_of(&z_to_a), vec![ids[2], ids[1], ids[0], ids[3]]);
}

#[tokio::test]
async fn sorting_by_path_ignores_case() {
    let fx = TrashFx::new().await;
    let mut ids = Vec::new();
    for path in ["z/apple.jpg", "a/Banana.jpg", "M/cherry.jpg", "b/apple.jpg"] {
        ids.push(fx.insert(Row::new(fx.a, BATCH, path)).await);
    }

    let a_to_z = fx.list("sort=path").await;
    assert_eq!(ids_of(&a_to_z), vec![ids[1], ids[3], ids[2], ids[0]]);
    let z_to_a = fx.list("sort=path&order=desc").await;
    assert_eq!(ids_of(&z_to_a), vec![ids[0], ids[2], ids[3], ids[1]]);
}

#[tokio::test]
async fn a_sorted_listing_pages_without_repeats_or_gaps() {
    let fx = TrashFx::new().await;
    for n in 0..23 {
        // Many equal sizes: the order must stay total through the id.
        fx.insert(Row::new(fx.a, BATCH, &format!("f{n:02}.jpg")).size(n % 3))
            .await;
    }

    for sort in ["trashed_at", "size", "name", "path"] {
        for order in ["asc", "desc"] {
            let whole = fx
                .list(&format!("sort={sort}&order={order}&limit=200"))
                .await;
            let mut paged = Vec::new();
            for offset in (0..23).step_by(5) {
                let page = fx
                    .list(&format!(
                        "sort={sort}&order={order}&limit=5&offset={offset}"
                    ))
                    .await;
                paged.extend(ids_of(&page));
            }
            assert_eq!(paged, ids_of(&whole), "{sort} {order}");
            assert_eq!(paged.len(), 23, "{sort} {order}");
        }
    }
}

#[tokio::test]
async fn the_volume_filter_keeps_one_volume_and_counts_it() {
    let fx = TrashFx::new().await;
    let in_a = fx.insert(Row::new(fx.a, BATCH, "x.jpg").size(10)).await;
    fx.insert(Row::new(fx.b, BATCH, "y.jpg").size(20)).await;
    fx.insert(Row::new(fx.b, BATCH, "z.jpg").size(30)).await;

    let only_a = fx.list(&format!("volume_id={}", fx.a)).await;
    assert_eq!(ids_of(&only_a), vec![in_a]);
    assert_eq!(only_a["total"], json!(1));
    // The summary stays the trash as a whole: it is what "Empty trash" quotes.
    assert_eq!(only_a["summary"]["items"], json!(3));
    assert_eq!(only_a["summary"]["bytes"], json!(60));

    let only_b = fx.list(&format!("volume_id={}", fx.b)).await;
    assert_eq!(only_b["total"], json!(2));
    assert_eq!(only_b["items"].as_array().unwrap().len(), 2);

    let nowhere = fx.list("volume_id=999").await;
    assert_eq!(nowhere["items"], json!([]));
    assert_eq!(nowhere["total"], json!(0));
}

#[tokio::test]
async fn the_batch_filter_keeps_one_deletion_of_one_volume() {
    let fx = TrashFx::new().await;
    let first = fx.insert(Row::new(fx.a, "20240101-000000-t1", "x")).await;
    let second = fx.insert(Row::new(fx.a, "20240101-000000-t1", "y")).await;
    fx.insert(Row::new(fx.a, "20240202-000000-t2", "z")).await;
    // The same batch name on another volume is another deletion.
    fx.insert(Row::new(fx.b, "20240101-000000-t1", "w")).await;

    let listing = fx
        .list(&format!(
            "volume_id={}&batch=20240101-000000-t1&order=asc",
            fx.a
        ))
        .await;
    assert_eq!(ids_of(&listing), vec![first, second]);
    assert_eq!(listing["total"], json!(2));

    let none = fx
        .list(&format!("volume_id={}&batch=20240909-000000-t9", fx.a))
        .await;
    assert_eq!(none["items"], json!([]));
    assert_eq!(none["total"], json!(0));
}

#[tokio::test]
async fn the_text_filter_matches_inside_the_path_and_treats_wildcards_as_text() {
    let fx = TrashFx::new().await;
    let upper = fx.insert(Row::new(fx.a, BATCH, "Holiday/IMG_1.jpg")).await;
    let lower = fx.insert(Row::new(fx.a, BATCH, "holiday/img_2.jpg")).await;
    let percent = fx.insert(Row::new(fx.a, BATCH, "work/100%.jpg")).await;
    let underscore = fx.insert(Row::new(fx.a, BATCH, "work/a_b.jpg")).await;
    let letter = fx.insert(Row::new(fx.a, BATCH, "work/axb.jpg")).await;
    let backslash = fx
        .insert(Row::new(fx.a, BATCH, "work/back\\slash.jpg"))
        .await;

    let ids = |mut ids: Vec<i64>| {
        ids.sort_unstable();
        ids
    };
    let found = |query: &'static str| {
        let fx = &fx;
        async move { ids(ids_of(&fx.list(query).await)) }
    };

    assert_eq!(found("q=HOLIDAY").await, vec![upper, lower]);
    assert_eq!(found("q=day/img").await, vec![upper, lower]);
    assert_eq!(found("q=100%25").await, vec![percent]);
    assert_eq!(found("q=%25").await, vec![percent]);
    assert_eq!(found("q=a_b").await, vec![underscore]);
    assert_eq!(
        found("q=_").await,
        vec![upper, lower, underscore],
        "an underscore is an underscore, not any character"
    );
    assert_eq!(found("q=%5C").await, vec![backslash]);
    assert_eq!(found("q=a%25b").await, Vec::<i64>::new());
    assert_eq!(found("q=nothing-like-this").await, Vec::<i64>::new());
    assert_eq!(found("q=axb").await, vec![letter]);

    let counted = fx.list("q=holiday&limit=1&offset=1").await;
    assert_eq!(counted["total"], json!(2));
    assert_eq!(counted["items"].as_array().unwrap().len(), 1);
    let nothing = fx.list("q=nothing-like-this").await;
    assert_eq!(nothing["total"], json!(0));
    assert_eq!(nothing["summary"]["items"], json!(6));
}

#[tokio::test]
async fn filters_combine() {
    let fx = TrashFx::new().await;
    fx.insert(Row::new(fx.a, BATCH, "cats/1.jpg")).await;
    let wanted = fx.insert(Row::new(fx.a, BATCH, "cats/2.jpg")).await;
    fx.insert(Row::new(fx.a, "20240202-000000-t2", "cats/3.jpg"))
        .await;
    fx.insert(Row::new(fx.b, BATCH, "cats/4.jpg")).await;

    let listing = fx
        .list(&format!("volume_id={}&batch={BATCH}&q=2.jpg", fx.a))
        .await;
    assert_eq!(ids_of(&listing), vec![wanted]);
    assert_eq!(listing["total"], json!(1));
}

#[tokio::test]
async fn the_summary_counts_every_volume_and_follows_the_data() {
    let fx = TrashFx::new().await;
    fx.insert(Row::new(fx.a, BATCH, "1").size(10)).await;
    fx.insert(Row::new(fx.a, BATCH, "2").size(20)).await;
    let middle = fx.insert(Row::new(fx.b, BATCH, "3").size(5)).await;
    let newest = fx.insert(Row::new(fx.a, BATCH, "4").size(30)).await;

    let summary = fx.list("").await["summary"].clone();
    assert_eq!(
        summary,
        json!({
            "items": 4,
            "bytes": 65,
            "max_id": newest,
            "volumes": [
                {"volume_id": fx.a, "name": "a", "items": 3, "bytes": 60, "online": true, "writable": true},
                {"volume_id": fx.b, "name": "b", "items": 1, "bytes": 5, "online": true, "writable": true},
            ],
            "retention_days": null,
            "conflict_policy": "smart",
        })
    );

    sqlx::query("DELETE FROM trash_items WHERE id = ?")
        .bind(newest)
        .execute(fx.app.state.db.write())
        .await
        .unwrap();
    let summary = fx.list("").await["summary"].clone();
    assert_eq!(summary["items"], json!(3));
    assert_eq!(summary["bytes"], json!(35));
    assert_eq!(
        summary["max_id"],
        json!(middle),
        "max_id is the highest id still in the trash"
    );
}

#[tokio::test]
async fn the_summary_quotes_the_settings_and_the_state_of_the_volumes() {
    let fx = TrashFx::new().await;
    fx.insert(Row::new(fx.a, BATCH, "1")).await;
    fx.insert(Row::new(fx.b, BATCH, "2")).await;
    let mut settings = (*fx.app.state.settings.get()).clone();
    settings.trash_retention_days = Some(30);
    settings.default_conflict_policy = apm_core::dto::ConflictPolicy::Replace;
    fx.app.state.settings.update(settings).await.unwrap();

    let outage = fx.take_offline(fx.b).await;
    let summary = fx.list("").await["summary"].clone();
    fx.recover(outage).await;

    assert_eq!(summary["retention_days"], json!(30));
    assert_eq!(summary["conflict_policy"], json!("replace"));
    let volumes = summary["volumes"].as_array().unwrap();
    assert_eq!(volumes[0]["name"], json!("a"));
    assert_eq!(volumes[0]["online"], json!(true));
    assert_eq!(volumes[1]["name"], json!("b"));
    assert_eq!(volumes[1]["online"], json!(false));
    assert_eq!(volumes[1]["writable"], json!(false));
    assert_eq!(
        volumes[1]["items"],
        json!(1),
        "an offline volume keeps its items listed"
    );
}

#[tokio::test]
async fn a_read_only_volume_is_reported_as_such() {
    let fx = TrashFx::new().await;
    fx.insert(Row::new(fx.a, BATCH, "1")).await;
    let Some(outage) = fx.make_read_only(fx.a).await else {
        return;
    };
    let summary = fx.list("").await["summary"].clone();
    fx.recover(outage).await;

    assert_eq!(summary["volumes"][0]["online"], json!(true));
    assert_eq!(summary["volumes"][0]["writable"], json!(false));
}

#[tokio::test]
async fn deletions_group_by_volume_and_batch_newest_first() {
    let fx = TrashFx::new().await;
    // Volume a: one deletion of three items (two users, one junk) and one of a single item.
    fx.insert(
        Row::new(fx.a, "20240101-000000-t1", "1")
            .size(10)
            .reason("user")
            .task(1)
            .at(1_000),
    )
    .await;
    fx.insert(
        Row::new(fx.a, "20240101-000000-t1", "2")
            .size(20)
            .reason("user")
            .task(1)
            .at(1_002),
    )
    .await;
    fx.insert(
        Row::new(fx.a, "20240101-000000-t1", "3")
            .size(30)
            .reason("junk")
            .task(1)
            .at(1_001),
    )
    .await;
    fx.insert(Row::new(fx.a, "20240303-000000-t3", "4").size(1).at(3_000))
        .await;
    // Volume b reuses a batch name: it is another deletion.
    fx.insert(
        Row::new(fx.b, "20240101-000000-t1", "5")
            .size(5)
            .reason("moved")
            .at(2_000),
    )
    .await;

    let body = fx.batches("").await;
    assert_eq!(body["total"], json!(3));
    assert_eq!(
        body["batches"],
        json!([
            {
                "volume_id": fx.a, "volume": "a", "batch": "20240303-000000-t3",
                "task_id": null, "trashed_at": 3_000, "items": 1, "bytes": 1, "reasons": [],
            },
            {
                "volume_id": fx.b, "volume": "b", "batch": "20240101-000000-t1",
                "task_id": null, "trashed_at": 2_000, "items": 1, "bytes": 5, "reasons": ["moved"],
            },
            {
                "volume_id": fx.a, "volume": "a", "batch": "20240101-000000-t1",
                "task_id": 1, "trashed_at": 1_000, "items": 3, "bytes": 60, "reasons": ["user", "junk"],
            },
        ])
    );
    assert_eq!(body["summary"]["items"], json!(5));

    let only_b = fx.batches(&format!("volume_id={}", fx.b)).await;
    assert_eq!(only_b["total"], json!(1));
    assert_eq!(only_b["batches"][0]["volume"], json!("b"));

    let second_page = fx.batches("limit=1&offset=1").await;
    assert_eq!(second_page["total"], json!(3));
    assert_eq!(second_page["batches"].as_array().unwrap().len(), 1);
    assert_eq!(second_page["batches"][0]["volume"], json!("b"));
}

#[tokio::test]
async fn every_listing_is_a_422_that_names_its_bad_fields() {
    let fx = TrashFx::new().await;
    let (status, body) = fx
        .get("/api/trash?limit=0&offset=-1&sort=nope&order=up&volume_id=abc")
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], json!("validation_failed"));
    let mut fields = field_names(&body);
    fields.sort();
    assert_eq!(fields, ["limit", "offset", "order", "sort", "volume_id"]);

    let (status, body) = fx.get("/api/trash?limit=201").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(field_names(&body), ["limit"]);

    let (status, body) = fx.get("/api/trash?batch=20240101-000000-t1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(field_names(&body), ["batch"]);

    let (status, body) = fx.get("/api/trash/batches?limit=0&volume_id=x").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let mut fields = field_names(&body);
    fields.sort();
    assert_eq!(fields, ["limit", "volume_id"]);
}

#[tokio::test]
async fn repeated_empty_and_unknown_query_keys_are_tolerated() {
    let fx = TrashFx::new().await;
    let ids = sequence(&fx, 5).await;

    // The last value of a repeated key wins, an empty value is an absent one, an unknown key is
    // ignored.
    let listing = fx
        .list("limit=4&limit=2&sort=&order=&q=&volume_id=&batch=&bogus=1&bogus=2")
        .await;
    assert_eq!(listing["items"].as_array().unwrap().len(), 2);
    assert_eq!(ids_of(&listing), vec![ids[4], ids[3]]);
    assert_eq!(listing["total"], json!(5));

    let batches = fx.batches("limit=1&limit=3&unknown=x").await;
    assert_eq!(batches["total"], json!(1));
}

#[tokio::test]
async fn listing_changes_nothing() {
    let fx = TrashFx::new().await;
    fx.trash(fx.a, "x/1.jpg", b"one").await;
    fx.trash(fx.b, "y/2.jpg", b"two").await;
    fx.ta.write("kept/3.jpg", b"three");
    let before = (
        fx.rows().await,
        disk_files(fx.ta.root()),
        disk_files(fx.tb.root()),
        disk_dirs(fx.ta.root()),
        disk_dirs(fx.tb.root()),
    );
    let tasks_before = fx.app.state.tasks.list(0).await.unwrap().len();

    for path in [
        "/api/trash",
        "/api/trash?sort=name&q=jpg",
        "/api/trash/batches",
    ] {
        let (status, body) = fx.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }

    let after = (
        fx.rows().await,
        disk_files(fx.ta.root()),
        disk_files(fx.tb.root()),
        disk_dirs(fx.ta.root()),
        disk_dirs(fx.tb.root()),
    );
    assert_eq!(after, before);
    assert_eq!(
        fx.app.state.tasks.list(0).await.unwrap().len(),
        tasks_before
    );
}

#[tokio::test]
async fn a_post_without_the_request_header_is_refused() {
    let fx = TrashFx::new().await;
    fx.insert(Row::new(fx.a, BATCH, "x")).await;
    let before = fx.rows().await;
    let bodies = [
        (
            "/api/trash/restore",
            json!({"target": {"kind": "ids", "ids": [1]}, "conflict_policy": null}),
        ),
        (
            "/api/trash/empty",
            json!({"volume_id": null, "up_to_id": 1}),
        ),
        ("/api/trash/sync", json!({})),
    ];
    for (path, body) in bodies {
        let status = fx.post_unmarked(path, &body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(fx.rows().await, before);
    let queued = fx
        .app
        .state
        .tasks
        .list(0)
        .await
        .unwrap()
        .into_iter()
        .filter(|task| {
            matches!(
                task.kind,
                TaskKind::TrashEmpty | TaskKind::FsMove | TaskKind::Index
            )
        })
        .count();
    assert_eq!(queued, 0);
}
