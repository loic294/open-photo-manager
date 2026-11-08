//! What the plan engine shows in the browse API (flags on real files, ghost files, folder data)
//! and the hand-off of the files a plan involves as a selection. The real application runs
//! around the scripted planner of `common::fake_planner`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;

use apm_core::dto::{FooterCounts, Overlay, PlanSide, SortKey, SortOrder};
use apm_core::paths::{FileId, PlanId, VolumeId};
use apm_server::plans::overlay::{FolderOverlay, Window};
use axum::http::{Method, StatusCode};
use common::browse::{self, error_code};
use common::fake_plan::{child, ghost_folder};
use common::fake_planner::{Call, Harness};
use common::seed::Seed;
use serde_json::{Value, json};

/// Volume `lib`: `inbox` holds a.jpg, b.jpg and c.jpg and the folder `2024` (two files);
/// `source` holds s1.jpg and s2.jpg, the files a plan would copy into `inbox`.
struct World {
    lib: VolumeId,
    real: [FileId; 3],
    sources: [FileId; 2],
}

async fn world(h: &Harness) -> World {
    let seed = Seed::new(h.db());
    let lib = seed.volume("lib").await;
    let mut real = [0; 3];
    for (slot, name) in real.iter_mut().zip(["a.jpg", "b.jpg", "c.jpg"]) {
        *slot = seed.file(lib, "inbox", name).insert().await;
    }
    for name in ["x1.jpg", "x2.jpg"] {
        seed.file(lib, "inbox/2024", name).insert().await;
    }
    let mut sources = [0; 2];
    for (slot, name) in sources.iter_mut().zip(["s1.jpg", "s2.jpg"]) {
        *slot = seed.file(lib, "source", name).insert().await;
    }
    World { lib, real, sources }
}

/// The request of a pane that lists `dir` of the volume with the plan on `side`.
fn pane(world: &World, dir: &str, plan: PlanId, side: &str) -> Value {
    json!({
        "volume_id": world.lib,
        "dir": dir,
        "plan": { "plan_id": plan, "side": side },
    })
}

fn overlay(matched: bool, planned: bool, will_delete: bool) -> Value {
    json!({ "matched": matched, "planned": planned, "will_delete": will_delete })
}

fn overlays(response: &Value) -> Vec<Value> {
    response["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["overlay"].clone())
        .collect()
}

fn ghost_row(id: FileId, world: &World, dir: &str, name: &str) -> Value {
    json!({ "id": id, "volume_id": world.lib, "dir": dir, "name": name })
}

/// The calls the browse API made to the planner.
fn overlay_calls(h: &Harness) -> Vec<Call> {
    h.planner()
        .calls()
        .into_iter()
        .filter(|call| {
            matches!(
                call,
                Call::ItemFlags { .. } | Call::GhostItems { .. } | Call::FolderOverlay { .. }
            )
        })
        .collect()
}

async fn hand_over(h: &Harness, plan: PlanId, body: Value) -> (StatusCode, Value) {
    h.app
        .call(
            Method::POST,
            &format!("/api/plans/{plan}/selection"),
            Some(body),
        )
        .await
}

// ---- files ----

#[tokio::test]
async fn real_files_carry_the_flags_of_the_side_they_are_listed_from() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [a, b, c] = w.real;
    let plan = h
        .new_plan(json!({ "matched": [a], "will_delete": [b], "comparison_matched": [c] }))
        .await;

    let (status, source) = browse::items(&h.app, pane(&w, "inbox", plan, "source")).await;
    assert_eq!(status, StatusCode::OK, "{source}");
    assert_eq!(browse::names(&source, "items"), ["a.jpg", "b.jpg", "c.jpg"]);
    assert_eq!(
        overlays(&source),
        [
            overlay(true, false, false),
            overlay(false, false, true),
            overlay(false, false, false)
        ]
    );
    assert_eq!(source["total"], 3);

    let (status, comparison) = browse::items(&h.app, pane(&w, "inbox", plan, "comparison")).await;
    assert_eq!(status, StatusCode::OK, "{comparison}");
    assert_eq!(
        overlays(&comparison),
        [
            overlay(false, false, false),
            overlay(false, false, false),
            overlay(true, false, false)
        ]
    );

    assert_eq!(
        overlay_calls(&h)
            .into_iter()
            .filter_map(|call| match call {
                Call::ItemFlags { plan_id, side, ids } => Some((plan_id, side, ids)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [
            (plan, PlanSide::Source, vec![a, b, c]),
            (plan, PlanSide::Comparison, vec![a, b, c]),
        ],
        "the planner is asked about the ids of the page, in listing order"
    );
}

#[tokio::test]
async fn the_ghosts_of_a_directory_follow_its_real_files() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [a, b, c] = w.real;
    let [s1, s2] = w.sources;
    let plan = h
        .new_plan(json!({
            "matched": [a],
            "ghosts": [
                ghost_row(s2, &w, "inbox", "g2.jpg"),
                ghost_row(s1, &w, "inbox", "g1.jpg"),
                ghost_row(s1, &w, "elsewhere", "g3.jpg"),
            ],
        }))
        .await;

    let (status, items) = browse::items(&h.app, pane(&w, "inbox", plan, "source")).await;
    assert_eq!(status, StatusCode::OK, "{items}");
    assert_eq!(
        browse::names(&items, "items"),
        ["a.jpg", "b.jpg", "c.jpg", "g1.jpg", "g2.jpg"],
        "real files first, then the ghosts of this directory by name"
    );
    assert_eq!(items["total"], 5);
    assert_eq!(browse::item_ids(&items), [a, b, c, s1, s2]);
    let overlays = overlays(&items);
    assert_eq!(overlays[0], overlay(true, false, false));
    assert_eq!(
        overlays[3..],
        [overlay(false, true, false), overlay(false, true, false)]
    );
    assert_eq!(items["items"][3]["volume_id"], w.lib);
    assert_eq!(items["items"][3]["dir"], "inbox");
    assert_eq!(items["items"][3]["name"], "g1.jpg");

    let asked = overlay_calls(&h);
    let Some(Call::GhostItems { view, .. }) = asked
        .iter()
        .find(|call| matches!(call, Call::GhostItems { .. }))
    else {
        panic!("the planner was not asked for ghosts: {asked:?}");
    };
    assert_eq!(view.volume_id, w.lib);
    assert_eq!(view.dir.as_str(), "inbox");
    assert_eq!((view.sort, view.order), (SortKey::Name, SortOrder::Asc));

    let (status, elsewhere) = browse::items(&h.app, pane(&w, "elsewhere", plan, "source")).await;
    assert_eq!(status, StatusCode::OK, "{elsewhere}");
    assert_eq!(browse::names(&elsewhere, "items"), ["g3.jpg"]);
    assert_eq!(elsewhere["total"], 1);
}

#[tokio::test]
async fn a_page_can_span_the_real_files_and_the_ghosts() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [s1, s2] = w.sources;
    let plan = h
        .new_plan(json!({
            "ghosts": [
                ghost_row(s1, &w, "inbox", "g1.jpg"),
                ghost_row(s2, &w, "inbox", "g2.jpg"),
            ],
        }))
        .await;

    let mut first = pane(&w, "inbox", plan, "source");
    first["limit"] = json!(4);
    let (status, page) = browse::items(&h.app, first).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        browse::names(&page, "items"),
        ["a.jpg", "b.jpg", "c.jpg", "g1.jpg"]
    );
    assert_eq!(page["total"], 5);

    let mut second = pane(&w, "inbox", plan, "source");
    second["limit"] = json!(4);
    second["offset"] = json!(4);
    let (status, page) = browse::items(&h.app, second).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(browse::names(&page, "items"), ["g2.jpg"]);
    assert_eq!(page["total"], 5);
    assert_eq!(page["offset"], 4);

    let windows: Vec<Window> = overlay_calls(&h)
        .into_iter()
        .filter_map(|call| match call {
            Call::GhostItems { window, .. } => Some(window),
            _ => None,
        })
        .collect();
    assert_eq!(
        windows,
        [Window { skip: 0, take: 1 }, Window { skip: 1, take: 4 }],
        "the engine gets the window the browse API computed, unchanged"
    );
}

#[tokio::test]
async fn select_all_lists_real_files_only_and_never_asks_the_planner() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [s1, _] = w.sources;
    let plan = h
        .new_plan(json!({ "ghosts": [ghost_row(s1, &w, "inbox", "g1.jpg")] }))
        .await;

    let (status, ids) = browse::ids(&h.app, pane(&w, "inbox", plan, "source")).await;
    assert_eq!(status, StatusCode::OK, "{ids}");
    assert_eq!(browse::id_list(&ids), w.real, "a ghost is never selectable");
    assert!(overlay_calls(&h).is_empty());

    let (status, ids) = browse::ids(&h.app, pane(&w, "inbox", 4242, "source")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "ids ignore the plan altogether: {ids}"
    );
    assert_eq!(browse::id_list(&ids), w.real);
}

#[tokio::test]
async fn a_plan_that_is_running_still_shows_its_overlay() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [a, ..] = w.real;
    let plan = h.new_plan(json!({ "matched": [a] })).await;
    let task = h.start(plan, json!({ "hold": true })).await;

    let (status, items) = browse::items(&h.app, pane(&w, "inbox", plan, "source")).await;
    assert_eq!(status, StatusCode::OK, "{items}");
    assert_eq!(overlays(&items)[0], overlay(true, false, false));

    h.planner().open_exec_gate();
    h.finish(task.id).await;
}

#[tokio::test]
async fn an_executed_plan_has_nothing_left_to_show() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [a, b, _] = w.real;
    let [s1, _] = w.sources;
    let plan = h
        .new_plan(json!({
            "matched": [a],
            "will_delete": [b],
            "ghosts": [ghost_row(s1, &w, "inbox", "g1.jpg")],
        }))
        .await;
    h.planner().set_folder_overlay(FolderOverlay {
        ghosts: FooterCounts {
            in_folder: 1,
            in_subfolders: 0,
        },
        ..FolderOverlay::default()
    });
    h.run(plan, json!({})).await;

    let (status, items) = browse::items(&h.app, pane(&w, "inbox", plan, "source")).await;
    assert_eq!(status, StatusCode::OK, "{items}");
    assert_eq!(browse::names(&items, "items"), ["a.jpg", "b.jpg", "c.jpg"]);
    assert_eq!(items["total"], 3);
    assert!(
        overlays(&items)
            .iter()
            .all(|flags| *flags == overlay(false, false, false))
    );

    let (status, plain) =
        browse::folders(&h.app, json!({ "volume_id": w.lib, "dir": "inbox" })).await;
    assert_eq!(status, StatusCode::OK, "{plain}");
    let (status, folders) = browse::folders(&h.app, pane(&w, "inbox", plan, "source")).await;
    assert_eq!(status, StatusCode::OK, "{folders}");
    assert_eq!(folders, plain, "the folder list is the real one");
    assert!(overlay_calls(&h).is_empty(), "the planner is never asked");
}

// ---- folders ----

#[tokio::test]
async fn the_folder_list_merges_the_overlay_of_the_planner() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let plan = h.new_plan(json!({})).await;

    let (status, plain) =
        browse::folders(&h.app, json!({ "volume_id": w.lib, "dir": "inbox" })).await;
    assert_eq!(status, StatusCode::OK, "{plain}");
    assert_eq!(browse::names(&plain, "folders"), ["2024"]);
    let real_counts = browse::counts(&plain);
    let (real_count, real_total) = real_counts[0];
    let footer_in_folder = plain["counts"]["in_folder"].as_i64().unwrap();
    let footer_in_subfolders = plain["counts"]["in_subfolders"].as_i64().unwrap();

    h.planner().set_folder_overlay(FolderOverlay {
        per_child: HashMap::from([(
            "2024".to_owned(),
            child(
                Some(4),
                Overlay {
                    matched: false,
                    planned: false,
                    will_delete: true,
                },
                2,
                5,
            ),
        )]),
        ghost_folders: vec![ghost_folder(w.lib, "inbox/new", 3, 9, Some(1))],
        ghosts: FooterCounts {
            in_folder: 2,
            in_subfolders: 3,
        },
        dir_planned: false,
    });
    let (status, folders) = browse::folders(&h.app, pane(&w, "inbox", plan, "comparison")).await;
    assert_eq!(status, StatusCode::OK, "{folders}");
    assert_eq!(browse::names(&folders, "folders"), ["2024", "new"]);
    assert_eq!(
        browse::counts(&folders),
        [(real_count + 2, real_total + 5), (3, 9)]
    );
    assert_eq!(folders["folders"][0]["matches"], 4);
    assert_eq!(
        folders["folders"][0]["overlay"],
        overlay(false, false, true)
    );
    assert_eq!(
        folders["folders"][1]["overlay"],
        overlay(false, true, false)
    );
    assert_eq!(folders["folders"][1]["path"], "inbox/new");
    assert_eq!(folders["counts"]["in_folder"], footer_in_folder + 2);
    assert_eq!(folders["counts"]["in_subfolders"], footer_in_subfolders + 3);

    let asked = overlay_calls(&h);
    let [
        Call::FolderOverlay {
            plan_id,
            side,
            view,
            real_children,
        },
    ] = asked.as_slice()
    else {
        panic!("expected one folder overlay call: {asked:?}");
    };
    assert_eq!((*plan_id, *side), (plan, PlanSide::Comparison));
    assert_eq!((view.volume_id, view.dir.as_str()), (w.lib, "inbox"));
    assert_eq!(real_children, &["2024".to_owned()]);
}

#[tokio::test]
async fn a_directory_that_does_not_exist_yet_is_listed_when_the_plan_creates_it() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let plan = h.new_plan(json!({})).await;
    let request = pane(&w, "inbox/new", plan, "source");

    let (status, body) = browse::folders(&h.app, request.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "folder_not_found");

    h.planner().set_folder_overlay(FolderOverlay {
        dir_planned: true,
        ..FolderOverlay::default()
    });
    let (status, body) = browse::folders(&h.app, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["folders"].as_array().unwrap().is_empty());
}

// ---- plans the engine does not know ----

#[tokio::test]
async fn a_plan_that_is_gone_is_a_404_for_every_listing_that_uses_it() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let now = h.clock.now();
    let expiring = h.new_plan(json!({ "matched": [w.real[0]] })).await;
    h.clock.advance(2 * 60 * 60 + 1);
    let now_later = h.clock.now();
    let building = h
        .insert_raw("cleanup", "building", now_later, now_later + 7200)
        .await;
    let unplanned = h
        .insert_raw("duplicates", "ready", now, now_later + 7200)
        .await;

    for (what, plan) in [
        ("unknown", 4242),
        ("expired", expiring),
        ("still building", building),
        ("of a kind without a planner", unplanned),
    ] {
        let (status, body) = browse::items(&h.app, pane(&w, "inbox", plan, "source")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "items, {what}: {body}");
        assert_eq!(error_code(&body), "plan_not_found", "items, {what}");

        let (status, body) = browse::folders(&h.app, pane(&w, "inbox", plan, "source")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "folders, {what}: {body}");
        assert_eq!(error_code(&body), "plan_not_found", "folders, {what}");
    }
    assert!(overlay_calls(&h).is_empty(), "no planner was asked");
}

#[tokio::test]
async fn browsing_a_plan_keeps_it_alive() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let plan = h.new_plan(json!({ "matched": [w.real[0]] })).await;
    let created = h.clock.now();
    assert_eq!(h.raw_expires_at(plan).await, created + 7200);

    h.clock.advance(7000);
    let (status, body) = browse::items(&h.app, pane(&w, "inbox", plan, "source")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    h.settle().await;
    assert_eq!(h.raw_expires_at(plan).await, created + 7000 + 7200);
}

// ---- hand-off as a selection ----

#[tokio::test]
async fn the_files_a_plan_involves_become_a_selection() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [a, b, c] = w.real;
    let [s1, _] = w.sources;
    let missing: FileId = 999_999;
    let plan = h
        .new_plan(json!({
            "matched": [a, missing],
            "will_delete": [b],
            "comparison_matched": [c],
            "ghosts": [ghost_row(s1, &w, "inbox", "g1.jpg")],
            "filler": 3,
        }))
        .await;

    let (status, selection) = hand_over(&h, plan, json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(
        selection["item_count"], 3,
        "a, b and the source of the ghost"
    );
    assert_eq!(selection["total_size"], 3_000);
    assert_eq!(selection["label"], format!("cleanup plan {plan}"));
    assert_eq!(
        selection["expires_at"].as_i64().unwrap() - selection["created_at"].as_i64().unwrap(),
        24 * 60 * 60
    );
    assert!(h.planner().calls().contains(&Call::InvolvedIds {
        plan_id: plan,
        side: PlanSide::Source,
    }));

    let id = selection["id"].as_i64().unwrap();
    let (status, inbox) = browse::items(
        &h.app,
        json!({ "volume_id": w.lib, "dir": "inbox", "filter": { "selection_id": id } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{inbox}");
    assert_eq!(browse::names(&inbox, "items"), ["a.jpg", "b.jpg"]);
    let (status, source) = browse::items(
        &h.app,
        json!({ "volume_id": w.lib, "dir": "source", "filter": { "selection_id": id } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{source}");
    assert_eq!(browse::names(&source, "items"), ["s1.jpg"]);
}

#[tokio::test]
async fn the_other_side_of_a_plan_has_its_own_files() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [a, _, c] = w.real;
    let plan = h
        .new_plan(json!({ "matched": [a], "comparison_matched": [c] }))
        .await;

    let (status, selection) = hand_over(&h, plan, json!({ "side": "comparison" })).await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(selection["item_count"], 1);
    assert!(h.planner().calls().contains(&Call::InvolvedIds {
        plan_id: plan,
        side: PlanSide::Comparison,
    }));
    let (status, inbox) = browse::items(
        &h.app,
        json!({
            "volume_id": w.lib,
            "dir": "inbox",
            "filter": { "selection_id": selection["id"] },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{inbox}");
    assert_eq!(browse::names(&inbox, "items"), ["c.jpg"]);
}

#[tokio::test]
async fn a_hand_off_can_be_restricted_to_some_files() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let [a, b, c] = w.real;
    let plan = h
        .new_plan(json!({ "matched": [a], "will_delete": [b] }))
        .await;

    let (status, selection) = hand_over(&h, plan, json!({ "restrict_ids": [b, c] })).await;
    assert_eq!(status, StatusCode::CREATED, "{selection}");
    assert_eq!(
        selection["item_count"], 1,
        "only b is both involved and allowed"
    );

    let (status, none) = hand_over(&h, plan, json!({ "restrict_ids": [] })).await;
    assert_eq!(status, StatusCode::CREATED, "{none}");
    assert_eq!(none["item_count"], 0, "an empty restriction allows nothing");
    assert_eq!(none["total_size"], 0);
}

#[tokio::test]
async fn handing_files_over_keeps_the_plan_alive() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let plan = h.new_plan(json!({ "matched": [w.real[0]] })).await;
    let created = h.clock.now();

    h.clock.advance(7000);
    let (status, body) = hand_over(&h, plan, json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    h.settle().await;
    assert_eq!(h.raw_expires_at(plan).await, created + 7000 + 7200);
}

#[tokio::test]
async fn an_executed_plan_involves_no_files_any_more() {
    let h = Harness::new().await;
    let w = world(&h).await;
    let plan = h.new_plan(json!({ "matched": [w.real[0]] })).await;
    h.run(plan, json!({})).await;

    let (status, body) = hand_over(&h, plan, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "plan_executed");
}

#[tokio::test]
async fn a_plan_that_is_not_there_hands_nothing_over() {
    let h = Harness::new().await;
    let now = h.clock.now();
    let building = h.insert_raw("cleanup", "building", now, now + 7200).await;

    for plan in [4242, building] {
        let (status, body) = hand_over(&h, plan, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{plan}: {body}");
        assert_eq!(error_code(&body), "plan_not_found");
    }

    let unplanned = h.insert_raw("duplicates", "ready", now, now + 7200).await;
    let (status, body) = hand_over(&h, unplanned, json!({})).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert_eq!(error_code(&body), "not_implemented");

    let (status, body) = h.app.call(Method::GET, "/api/selections/1", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no selection was created: {body}"
    );
}
