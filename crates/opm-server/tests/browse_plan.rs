//! The browse service functions against a hand-written plan provider ([`FakePlan`]): how ghosts,
//! flags and folder data are merged into the real listings, with no plan engine behind the trait.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use common::TestApp;
use common::fake_plan::{Call, FakePlan, child, empty_overlay, ghost, ghost_folder};
use common::seed::Seed;
use opm_core::dto::{
    BrowseRequest, FileItem, Filter, FlagFilter, FolderEntry, FooterCounts, ItemsRequest, Overlay,
    PlanRef, PlanSide, SortKey, SortOrder,
};
use opm_core::paths::{FileId, RelPath, VolumeId};
use opm_server::api::browse::service;
use opm_server::error::AppError;
use opm_server::plans::overlay::{FolderOverlay, GhostView, Window};

const PLAN: PlanRef = PlanRef {
    plan_id: 9,
    side: PlanSide::Source,
};

/// Volume `lib`: `inbox` holds a.jpg, b.jpg and c.jpg and the folders 2024 (2 files), misc (1) and
/// zeta (3); `source` holds the four files a plan copies into `inbox`. Volume `other` holds one
/// file and volume `empty` none.
struct World {
    lib: VolumeId,
    empty: VolumeId,
    real: [FileId; 3],
    sources: [FileId; 4],
}

impl World {
    /// The four ghosts of `inbox`, in listing order.
    fn ghosts(&self) -> Vec<FileItem> {
        ["g1.jpg", "g2.jpg", "g3.jpg", "g4.jpg"]
            .iter()
            .zip(self.sources)
            .map(|(name, source)| ghost(source, self.lib, "inbox", name))
            .collect()
    }
}

async fn world(app: &TestApp) -> World {
    let seed = Seed::new(&app.state.db);
    let lib = seed.volume("lib").await;
    let mut real = [0; 3];
    for (slot, name) in real.iter_mut().zip(["a.jpg", "b.jpg", "c.jpg"]) {
        *slot = seed.file(lib, "inbox", name).insert().await;
    }
    for (dir, name) in [
        ("inbox/2024", "x1.jpg"),
        ("inbox/2024", "x2.jpg"),
        ("inbox/misc", "y1.jpg"),
        ("inbox/zeta", "z1.jpg"),
        ("inbox/zeta", "z2.jpg"),
        ("inbox/zeta", "z3.jpg"),
    ] {
        seed.file(lib, dir, name).insert().await;
    }
    let mut sources = [0; 4];
    for (slot, name) in sources
        .iter_mut()
        .zip(["s1.jpg", "s2.jpg", "s3.jpg", "s4.jpg"])
    {
        *slot = seed.file(lib, "source", name).insert().await;
    }
    let other = seed.volume("other").await;
    seed.file(other, "pics", "p1.jpg").insert().await;
    let empty = seed.volume("empty").await;
    World {
        lib,
        empty,
        real,
        sources,
    }
}

fn browse(volume: Option<VolumeId>, dir: &str, plan: Option<PlanRef>) -> BrowseRequest {
    BrowseRequest {
        volume_id: volume,
        dir: dir.to_owned(),
        plan,
        ..BrowseRequest::default()
    }
}

fn listing(volume: Option<VolumeId>, dir: &str, plan: Option<PlanRef>) -> ItemsRequest {
    ItemsRequest {
        browse: browse(volume, dir, plan),
        ..ItemsRequest::default()
    }
}

fn page(mut request: ItemsRequest, offset: u32, limit: u32) -> ItemsRequest {
    request.offset = offset;
    request.limit = limit;
    request
}

/// What the provider is asked about: the default filter and order.
fn view(volume: VolumeId, dir: &str) -> GhostView {
    GhostView {
        volume_id: volume,
        dir: RelPath::new(dir).unwrap(),
        filter: Filter::default(),
        sort: SortKey::Name,
        order: SortOrder::Asc,
    }
}

fn names(items: &[FileItem]) -> Vec<&str> {
    items.iter().map(|item| item.name.as_str()).collect()
}

fn folder_names(folders: &[FolderEntry]) -> Vec<&str> {
    folders.iter().map(|folder| folder.name.as_str()).collect()
}

fn overlays(items: &[FileItem]) -> Vec<Overlay> {
    items.iter().map(|item| item.overlay).collect()
}

fn flags(matched: bool, will_delete: bool) -> Overlay {
    Overlay {
        matched,
        planned: false,
        will_delete,
    }
}

fn flag_calls(fake: &FakePlan) -> Vec<Call> {
    let calls = fake.calls();
    calls
        .into_iter()
        .filter(|call| matches!(call, Call::ItemFlags { .. }))
        .collect()
}

fn ghost_calls(fake: &FakePlan) -> Vec<Call> {
    let calls = fake.calls();
    calls
        .into_iter()
        .filter(|call| matches!(call, Call::GhostItems { .. }))
        .collect()
}

fn folder_calls(fake: &FakePlan) -> Vec<Call> {
    let calls = fake.calls();
    calls
        .into_iter()
        .filter(|call| matches!(call, Call::FolderOverlay { .. }))
        .collect()
}

fn stale() -> AppError {
    AppError::conflict("plan_stale", "plan 9 changed")
}

// ---------------------------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn without_a_plan_the_provider_is_never_called() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::failing(stale)
        .with_ghosts(world.ghosts())
        .with_flags([(world.real[0], flags(true, true))]);

    let items = service::items(
        &app.state.db,
        &fake,
        &listing(Some(world.lib), "inbox", None),
    )
    .await
    .unwrap();
    assert_eq!(names(&items.items), ["a.jpg", "b.jpg", "c.jpg"]);
    assert_eq!(items.total, 3);
    assert!(
        overlays(&items.items)
            .iter()
            .all(|flags| *flags == Overlay::default())
    );

    let folders = service::folders(
        &app.state.db,
        &fake,
        &browse(Some(world.lib), "inbox", None),
    )
    .await
    .unwrap();
    assert_eq!(folder_names(&folders.folders), ["2024", "misc", "zeta"]);
    assert!(
        folders
            .folders
            .iter()
            .all(|folder| folder.matches.is_none())
    );
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn ghosts_are_listed_after_the_real_files() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default().with_ghosts(world.ghosts());

    let request = listing(Some(world.lib), "inbox", Some(PLAN));
    let response = service::items(&app.state.db, &fake, &request)
        .await
        .unwrap();

    assert_eq!(
        names(&response.items),
        [
            "a.jpg", "b.jpg", "c.jpg", "g1.jpg", "g2.jpg", "g3.jpg", "g4.jpg"
        ]
    );
    assert_eq!(response.total, 7, "the real files and the ghosts");
    assert_eq!(response.offset, 0);
    assert_eq!(
        response.items[3..],
        world.ghosts()[..],
        "ghosts are passed through as the provider built them"
    );
    assert_eq!(
        response.items[3].id, world.sources[0],
        "a ghost wears its source's id"
    );
    assert!(response.items[3..].iter().all(|item| item.overlay.planned));
    assert!(response.items[..3].iter().all(|item| !item.overlay.planned));

    assert_eq!(
        flag_calls(&fake),
        [Call::ItemFlags {
            plan: PLAN,
            ids: world.real.to_vec(),
        }],
        "the flags are asked for the real files of the page, in listing order"
    );
    assert_eq!(
        ghost_calls(&fake),
        [Call::GhostItems {
            plan: PLAN,
            view: view(world.lib, "inbox"),
            window: Window { skip: 0, take: 197 },
        }],
        "the ghosts fill whatever room the default page of 200 has left"
    );
    assert!(folder_calls(&fake).is_empty());
}

#[tokio::test]
async fn flags_apply_to_the_real_files_only() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    // A ghost that derives from a real file of the same page wears the same id.
    let copy = ghost(world.real[0], world.lib, "inbox", "a copy.jpg");
    let fake = FakePlan::default()
        .with_ghosts(vec![copy.clone()])
        .with_flags([
            (world.real[0], flags(true, false)),
            (world.real[2], flags(false, true)),
        ]);

    let request = listing(Some(world.lib), "inbox", Some(PLAN));
    let response = service::items(&app.state.db, &fake, &request)
        .await
        .unwrap();

    assert_eq!(
        names(&response.items),
        ["a.jpg", "b.jpg", "c.jpg", "a copy.jpg"]
    );
    assert_eq!(
        overlays(&response.items),
        [
            flags(true, false),
            Overlay::default(),
            flags(false, true),
            Overlay {
                planned: true,
                ..Overlay::default()
            },
        ],
        "a file the plan says nothing about keeps the default flags, and the ghost keeps its own"
    );
    assert_eq!(response.items[3], copy);
}

/// (offset, limit, names on the page, ghost window asked, indexes of the real files asked for)
type PageCut = (u32, u32, &'static [&'static str], Window, &'static [usize]);

#[tokio::test]
async fn pages_cut_the_real_files_then_the_ghosts() {
    let app = TestApp::new().await;
    let world = world(&app).await;

    let cases: [PageCut; 7] = [
        (
            0,
            3,
            &["a.jpg", "b.jpg", "c.jpg"],
            Window { skip: 0, take: 0 },
            &[0, 1, 2],
        ),
        (
            2,
            3,
            &["c.jpg", "g1.jpg", "g2.jpg"],
            Window { skip: 0, take: 2 },
            &[2],
        ),
        (
            3,
            3,
            &["g1.jpg", "g2.jpg", "g3.jpg"],
            Window { skip: 0, take: 3 },
            &[],
        ),
        (
            4,
            2,
            &["g2.jpg", "g3.jpg"],
            Window { skip: 1, take: 2 },
            &[],
        ),
        (6, 5, &["g4.jpg"], Window { skip: 3, take: 5 }, &[]),
        (7, 5, &[], Window { skip: 4, take: 5 }, &[]),
        (100, 5, &[], Window { skip: 97, take: 5 }, &[]),
    ];
    for (offset, limit, expected, window, asked) in cases {
        let fake = FakePlan::default().with_ghosts(world.ghosts());
        let request = page(listing(Some(world.lib), "inbox", Some(PLAN)), offset, limit);
        let response = service::items(&app.state.db, &fake, &request)
            .await
            .unwrap();

        let case = format!("offset {offset} limit {limit}");
        assert_eq!(names(&response.items), expected, "{case}");
        assert_eq!(response.total, 7, "{case}");
        assert_eq!(response.offset, offset, "{case}");
        assert_eq!(
            ghost_calls(&fake),
            [Call::GhostItems {
                plan: PLAN,
                view: view(world.lib, "inbox"),
                window,
            }],
            "{case}: the provider is always asked, a full page of real files asks for the total"
        );
        let asked: Vec<Call> = if asked.is_empty() {
            Vec::new()
        } else {
            vec![Call::ItemFlags {
                plan: PLAN,
                ids: asked.iter().map(|index| world.real[*index]).collect(),
            }]
        };
        assert_eq!(
            flag_calls(&fake),
            asked,
            "{case}: no real file on the page, no flags asked"
        );
    }
}

#[tokio::test]
async fn walking_the_pages_lists_every_file_once() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default().with_ghosts(world.ghosts());
    let everything = [
        "a.jpg", "b.jpg", "c.jpg", "g1.jpg", "g2.jpg", "g3.jpg", "g4.jpg",
    ];

    for limit in 1..=9_u32 {
        let mut walked = Vec::new();
        let mut offset = 0_u32;
        loop {
            let request = page(listing(Some(world.lib), "inbox", Some(PLAN)), offset, limit);
            let response = service::items(&app.state.db, &fake, &request)
                .await
                .unwrap();
            assert_eq!(response.total, 7, "limit {limit} offset {offset}");
            assert_eq!(response.offset, offset);
            let left = usize::try_from(7_u32.saturating_sub(offset)).unwrap();
            let size = usize::try_from(limit).unwrap();
            assert_eq!(
                response.items.len(),
                left.min(size),
                "limit {limit} offset {offset}"
            );
            if response.items.is_empty() {
                break;
            }
            walked.extend(response.items.iter().map(|item| item.name.clone()));
            offset += limit;
        }
        assert_eq!(walked, everything, "limit {limit}");
    }
}

#[tokio::test]
async fn the_ghost_window_uses_the_clamped_limit() {
    let app = TestApp::new().await;
    let world = world(&app).await;

    for (limit, take) in [
        (0, 197),
        (1, 0),
        (4, 1),
        (500, 497),
        (1_000, 497),
        (u32::MAX, 497),
    ] {
        let fake = FakePlan::default().with_ghosts(world.ghosts());
        let request = page(listing(Some(world.lib), "inbox", Some(PLAN)), 0, limit);
        service::items(&app.state.db, &fake, &request)
            .await
            .unwrap();
        let Some(Call::GhostItems { window, .. }) = ghost_calls(&fake).pop() else {
            panic!("limit {limit}: the provider was not asked for ghosts");
        };
        assert_eq!(window, Window { skip: 0, take }, "limit {limit}");
    }
}

#[tokio::test]
async fn a_provider_returning_too_many_ghosts_cannot_overfill_a_page() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default()
        .with_ghosts(world.ghosts())
        .ignoring_the_window();

    let cases: [(u32, u32, &[&str]); 4] = [
        (0, 3, &["a.jpg", "b.jpg", "c.jpg"]),
        (2, 1, &["c.jpg"]),
        (2, 3, &["c.jpg", "g1.jpg", "g2.jpg"]),
        (2, 5, &["c.jpg", "g1.jpg", "g2.jpg", "g3.jpg", "g4.jpg"]),
    ];
    for (offset, limit, expected) in cases {
        let request = page(listing(Some(world.lib), "inbox", Some(PLAN)), offset, limit);
        let response = service::items(&app.state.db, &fake, &request)
            .await
            .unwrap();
        assert_eq!(
            names(&response.items),
            expected,
            "offset {offset} limit {limit}"
        );
        assert_eq!(response.total, 7, "offset {offset} limit {limit}");
    }
}

#[tokio::test]
async fn the_provider_is_asked_in_the_requests_own_terms() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let raw = Filter {
        cameras: vec!["Sony".to_owned(), " Canon ".to_owned(), "Canon".to_owned()],
        ratings: vec![5, 3, 5],
        flags: vec![FlagFilter::Rejected, FlagFilter::Selected],
        tags: vec![" trip".to_owned()],
        selection_id: None,
    };
    let canonical = raw.normalise().unwrap();
    assert_ne!(
        canonical, raw,
        "the test needs a filter that is not canonical"
    );
    let expected = GhostView {
        volume_id: world.lib,
        dir: RelPath::new("inbox").unwrap(),
        filter: canonical,
        sort: SortKey::Date,
        order: SortOrder::Desc,
    };

    let fake = FakePlan::default();
    let mut request = listing(Some(world.lib), "inbox", Some(PLAN));
    request.browse.filter = raw.clone();
    request.sort = SortKey::Date;
    request.order = SortOrder::Desc;
    service::items(&app.state.db, &fake, &request)
        .await
        .unwrap();
    assert_eq!(
        ghost_calls(&fake),
        [Call::GhostItems {
            plan: PLAN,
            view: expected.clone(),
            window: Window { skip: 0, take: 200 },
        }]
    );

    let fake = FakePlan::default();
    let mut request = browse(Some(world.lib), "inbox", Some(PLAN));
    request.filter = raw;
    service::folders(&app.state.db, &fake, &request)
        .await
        .unwrap();
    let Some(Call::FolderOverlay { view: asked, .. }) = folder_calls(&fake).pop() else {
        panic!("the provider was not asked about the folders");
    };
    assert_eq!(
        asked,
        GhostView {
            sort: SortKey::Name,
            order: SortOrder::Asc,
            ..expected
        },
        "the folder list is asked in the default order"
    );
}

#[tokio::test]
async fn the_side_of_the_plan_is_only_forwarded() {
    let app = TestApp::new().await;
    let world = world(&app).await;

    for side in [PlanSide::Source, PlanSide::Comparison] {
        let plan = PlanRef { plan_id: 4, side };
        let fake = FakePlan::default().with_ghosts(world.ghosts());
        service::items(
            &app.state.db,
            &fake,
            &listing(Some(world.lib), "inbox", Some(plan)),
        )
        .await
        .unwrap();
        service::folders(
            &app.state.db,
            &fake,
            &browse(Some(world.lib), "inbox", Some(plan)),
        )
        .await
        .unwrap();

        let calls = fake.calls();
        assert_eq!(calls.len(), 3, "{side:?}");
        for call in calls {
            let (Call::ItemFlags { plan: used, .. }
            | Call::GhostItems { plan: used, .. }
            | Call::FolderOverlay { plan: used, .. }) = call;
            assert_eq!(used, plan);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn folders_merge_the_plan_into_the_real_folders() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let overlay = FolderOverlay {
        per_child: HashMap::from([
            ("2024".to_owned(), child(Some(2), Overlay::default(), 1, 2)),
            ("zeta".to_owned(), child(Some(0), flags(false, true), 0, 0)),
            (
                "elsewhere".to_owned(),
                child(Some(9), flags(true, false), 9, 9),
            ),
        ]),
        ghost_folders: vec![
            ghost_folder(world.lib, "inbox/2025", 4, 4, Some(1)),
            ghost_folder(world.lib, "inbox/Archive", 1, 1, None),
        ],
        ghosts: FooterCounts {
            in_folder: 4,
            in_subfolders: 5,
        },
        dir_planned: false,
    };
    let fake = FakePlan::default().with_folder_overlay(overlay);

    let request = browse(Some(world.lib), "inbox", Some(PLAN));
    let response = service::folders(&app.state.db, &fake, &request)
        .await
        .unwrap();

    assert_eq!(
        folder_names(&response.folders),
        ["2024", "2025", "Archive", "misc", "zeta"],
        "ghost folders sort with the real ones, by normalised name"
    );
    let summary: Vec<(i64, i64, Option<i64>, Overlay)> = response
        .folders
        .iter()
        .map(|folder| (folder.count, folder.total, folder.matches, folder.overlay))
        .collect();
    let planned = Overlay {
        planned: true,
        ..Overlay::default()
    };
    assert_eq!(
        summary,
        [
            (3, 4, Some(2), Overlay::default()),
            (4, 4, Some(1), planned),
            (1, 1, None, planned),
            (1, 1, None, Overlay::default()),
            (3, 3, Some(0), flags(false, true)),
        ],
        "a real folder adds its ghosts to its counts; a folder the plan does not mention is left alone"
    );
    assert_eq!(response.folders[1].path, "inbox/2025");
    assert_eq!(response.folders[0].path, "inbox/2024");
    assert_eq!(
        response.counts,
        FooterCounts {
            in_folder: 3 + 4,
            in_subfolders: 6 + 5,
        },
        "the footer is the real counts plus the ghosts"
    );
    assert_eq!(
        folder_calls(&fake),
        [Call::FolderOverlay {
            plan: PLAN,
            view: view(world.lib, "inbox"),
            real_children: vec!["2024".to_owned(), "misc".to_owned(), "zeta".to_owned()],
        }],
        "the provider learns the real children in listing order"
    );
    assert!(flag_calls(&fake).is_empty() && ghost_calls(&fake).is_empty());
}

#[tokio::test]
async fn a_plan_that_adds_nothing_changes_nothing() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default();

    let with_plan = service::folders(
        &app.state.db,
        &fake,
        &browse(Some(world.lib), "inbox", Some(PLAN)),
    )
    .await
    .unwrap();
    let without = service::folders(
        &app.state.db,
        &fake,
        &browse(Some(world.lib), "inbox", None),
    )
    .await
    .unwrap();
    assert_eq!(with_plan, without);
}

#[tokio::test]
async fn the_footer_count_equals_the_items_total() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default()
        .with_ghosts(world.ghosts())
        .with_folder_overlay(FolderOverlay {
            ghosts: FooterCounts {
                in_folder: 4,
                in_subfolders: 0,
            },
            ..empty_overlay()
        });

    for plan in [None, Some(PLAN)] {
        let items = service::items(
            &app.state.db,
            &fake,
            &listing(Some(world.lib), "inbox", plan),
        )
        .await
        .unwrap();
        let folders = service::folders(
            &app.state.db,
            &fake,
            &browse(Some(world.lib), "inbox", plan),
        )
        .await
        .unwrap();
        assert_eq!(folders.counts.in_folder, items.total, "plan {plan:?}");
    }
}

#[tokio::test]
async fn a_planned_directory_missing_from_the_index_lists_its_ghosts() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default()
        .with_ghosts(vec![
            ghost(world.sources[0], world.lib, "inbox/new", "g1.jpg"),
            ghost(world.sources[1], world.lib, "inbox/new", "g2.jpg"),
        ])
        .with_folder_overlay(FolderOverlay {
            ghost_folders: vec![ghost_folder(world.lib, "inbox/new/day1", 3, 3, None)],
            ghosts: FooterCounts {
                in_folder: 2,
                in_subfolders: 3,
            },
            dir_planned: true,
            ..empty_overlay()
        });

    let folders = service::folders(
        &app.state.db,
        &fake,
        &browse(Some(world.lib), "inbox/new", Some(PLAN)),
    )
    .await
    .unwrap();
    assert_eq!(folder_names(&folders.folders), ["day1"]);
    assert_eq!(folders.folders[0].path, "inbox/new/day1");
    assert_eq!(
        folders.counts,
        FooterCounts {
            in_folder: 2,
            in_subfolders: 3,
        }
    );
    assert_eq!(
        folder_calls(&fake),
        [Call::FolderOverlay {
            plan: PLAN,
            view: view(world.lib, "inbox/new"),
            real_children: Vec::new(),
        }],
        "a directory that is not indexed has no real children"
    );

    let items = service::items(
        &app.state.db,
        &fake,
        &listing(Some(world.lib), "inbox/new", Some(PLAN)),
    )
    .await
    .unwrap();
    assert_eq!(names(&items.items), ["g1.jpg", "g2.jpg"]);
    assert_eq!(items.total, 2);
    assert!(items.items.iter().all(|item| item.overlay.planned));
    assert!(
        flag_calls(&fake).is_empty(),
        "there is no real file to flag"
    );
}

#[tokio::test]
async fn a_missing_directory_nobody_plans_is_folder_not_found() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default();

    let error = service::folders(
        &app.state.db,
        &fake,
        &browse(Some(world.lib), "inbox/nowhere", Some(PLAN)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status(), StatusCode::NOT_FOUND);
    assert_eq!(error.code(), "folder_not_found");
    assert_eq!(
        folder_calls(&fake).len(),
        1,
        "only the provider can tell that the plan creates the directory"
    );

    let error = service::folders(
        &app.state.db,
        &fake,
        &browse(Some(world.lib), "inbox/nowhere", None),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "folder_not_found");
    assert_eq!(folder_calls(&fake).len(), 1, "no plan, no question");

    let items = service::items(
        &app.state.db,
        &fake,
        &listing(Some(world.lib), "inbox/nowhere", Some(PLAN)),
    )
    .await
    .unwrap();
    assert_eq!(
        (items.items.len(), items.total),
        (0, 0),
        "listing items is lenient"
    );
}

#[tokio::test]
async fn ghost_folders_fill_an_empty_volume_root() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default().with_folder_overlay(FolderOverlay {
        ghost_folders: vec![ghost_folder(world.empty, "2025", 2, 2, None)],
        ghosts: FooterCounts {
            in_folder: 0,
            in_subfolders: 2,
        },
        ..empty_overlay()
    });

    let with_plan = service::folders(
        &app.state.db,
        &fake,
        &browse(Some(world.empty), "", Some(PLAN)),
    )
    .await
    .unwrap();
    assert_eq!(folder_names(&with_plan.folders), ["2025"]);
    assert_eq!(
        with_plan.counts,
        FooterCounts {
            in_folder: 0,
            in_subfolders: 2,
        }
    );

    let without = service::folders(&app.state.db, &fake, &browse(Some(world.empty), "", None))
        .await
        .unwrap();
    assert!(without.folders.is_empty());
    assert_eq!(without.counts, FooterCounts::default());
}

// ---------------------------------------------------------------------------------------------
// Errors, validation and the endpoints that ignore the plan
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_provider_error_is_returned_unchanged() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::failing(stale);

    let errors = [
        service::items(
            &app.state.db,
            &fake,
            &listing(Some(world.lib), "inbox", Some(PLAN)),
        )
        .await
        .unwrap_err(),
        service::items(
            &app.state.db,
            &fake,
            &page(listing(Some(world.lib), "inbox", Some(PLAN)), 50, 10),
        )
        .await
        .unwrap_err(),
        service::folders(
            &app.state.db,
            &fake,
            &browse(Some(world.lib), "inbox", Some(PLAN)),
        )
        .await
        .unwrap_err(),
        service::folders(
            &app.state.db,
            &fake,
            &browse(Some(world.lib), "inbox/nowhere", Some(PLAN)),
        )
        .await
        .unwrap_err(),
    ];
    for error in errors {
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.code(), "plan_stale");
        assert_eq!(error.message(), "plan 9 changed");
    }
}

#[tokio::test]
async fn bad_requests_never_reach_the_provider() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::default();

    let mut bad_filter = listing(Some(world.lib), "inbox", Some(PLAN));
    bad_filter.browse.filter.ratings = vec![6];
    let errors = [
        service::items(&app.state.db, &fake, &bad_filter)
            .await
            .unwrap_err(),
        service::folders(&app.state.db, &fake, &bad_filter.browse)
            .await
            .unwrap_err(),
        service::items(
            &app.state.db,
            &fake,
            &listing(Some(world.lib), "../etc", Some(PLAN)),
        )
        .await
        .unwrap_err(),
        service::folders(
            &app.state.db,
            &fake,
            &browse(Some(world.lib), "a//b", Some(PLAN)),
        )
        .await
        .unwrap_err(),
        service::items(&app.state.db, &fake, &listing(None, "inbox", Some(PLAN)))
            .await
            .unwrap_err(),
    ];
    for error in errors {
        assert_eq!(
            error.status(),
            StatusCode::BAD_REQUEST,
            "{}",
            error.message()
        );
        assert_eq!(error.code(), "bad_request");
    }

    let errors = [
        service::items(
            &app.state.db,
            &fake,
            &listing(Some(9_999), "inbox", Some(PLAN)),
        )
        .await
        .unwrap_err(),
        service::folders(&app.state.db, &fake, &browse(Some(9_999), "", Some(PLAN)))
            .await
            .unwrap_err(),
    ];
    for error in errors {
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.code(), "not_found");
    }
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn the_volumes_root_never_asks_the_provider() {
    let app = TestApp::new().await;
    let world = world(&app).await;
    let fake = FakePlan::failing(stale).with_ghosts(world.ghosts());

    let folders = service::folders(&app.state.db, &fake, &browse(None, "", Some(PLAN)))
        .await
        .unwrap();
    assert_eq!(folder_names(&folders.folders), ["empty", "lib", "other"]);
    assert!(
        folders
            .folders
            .iter()
            .all(|folder| folder.overlay == Overlay::default() && folder.matches.is_none())
    );

    let items = service::items(&app.state.db, &fake, &listing(None, "", Some(PLAN)))
        .await
        .unwrap();
    assert_eq!((items.items.len(), items.total), (0, 0));
    assert!(fake.calls().is_empty());

    // The root of ONE volume is a directory like any other.
    let error = service::folders(
        &app.state.db,
        &fake,
        &browse(Some(world.lib), "", Some(PLAN)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "plan_stale");
    assert_eq!(folder_calls(&fake).len(), 1);
}

#[tokio::test]
async fn ids_and_facets_describe_real_files_whatever_the_plan() {
    let app = TestApp::new().await;
    let world = world(&app).await;

    let with_plan = listing(Some(world.lib), "inbox", Some(PLAN));
    let without = listing(Some(world.lib), "inbox", None);
    let ids = service::ids(&app.state.db, &with_plan).await.unwrap();
    assert_eq!(ids.ids, world.real, "no ghost, whatever the plan says");
    assert_eq!(ids, service::ids(&app.state.db, &without).await.unwrap());

    let facets = service::facets(&app.state.db, &with_plan.browse)
        .await
        .unwrap();
    assert_eq!(
        facets,
        service::facets(&app.state.db, &without.browse)
            .await
            .unwrap()
    );
}
