//! `FileQuery::items`, `ids`, `count`, `facets` and `file_detail` against the in-memory reference
//! of `query_support`, plus what only a database shows: stable paging, bound values, the condition
//! embedded in other statements, the limits.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod query_support;

use axum::http::StatusCode;
use common::seed::Seed;
use opm_core::dto::{
    CaptureSource, Facets, FileItem, FileKind, Filter, FlagFilter, Overlay, Scope, SidecarDto,
    SortKey, SortOrder,
};
use opm_core::paths::FileId;
use opm_server::db::Db;
use opm_server::error::AppError;
use opm_server::query::{FileQuery, KindFilter, QueryError, QueryScope, file_detail};
use query_support as qs;
use query_support::{
    CANON, DROP_TAG, FacetsExpect, Fixture, MatrixFilter, QUOTE_CAMERA, SONY, Spec, WILD_CAMERA,
};
use sqlx::{QueryBuilder, Sqlite};

const KINDS: [KindFilter; 2] = [KindFilter::Assets, KindFilter::All];

const SORTS: [(SortKey, SortOrder); 6] = [
    (SortKey::Name, SortOrder::Asc),
    (SortKey::Name, SortOrder::Desc),
    (SortKey::Date, SortOrder::Asc),
    (SortKey::Date, SortOrder::Desc),
    (SortKey::Size, SortOrder::Asc),
    (SortKey::Size, SortOrder::Desc),
];

fn count_of(len: usize) -> i64 {
    i64::try_from(len).unwrap()
}

fn item_ids(items: &[FileItem]) -> Vec<FileId> {
    items.iter().map(|item| item.id).collect()
}

/// The names of the first 500 items of the query.
async fn names(db: &Db, query: &FileQuery) -> Vec<String> {
    query
        .items(db, 0, 500)
        .await
        .unwrap()
        .into_iter()
        .map(|item| item.name)
        .collect()
}

fn pick<'a>(filters: &'a [MatrixFilter], labels: &[&str]) -> Vec<&'a MatrixFilter> {
    labels
        .iter()
        .map(|label| {
            filters
                .iter()
                .find(|matrix| matrix.label == *label)
                .unwrap_or_else(|| panic!("no filter labelled {label}"))
        })
        .collect()
}

async fn check_against_reference(
    fixture: &Fixture,
    scope: &Scope,
    matrix: &MatrixFilter,
    kinds: KindFilter,
) {
    let db = fixture.db();
    let what = format!("{scope:?} / {} / {kinds:?}", matrix.label);
    let query = qs::query(scope, &matrix.filter, kinds);
    let expected =
        fixture
            .model
            .expected_ids(scope, &matrix.filter, kinds, SortKey::Name, SortOrder::Asc);
    assert_eq!(query.ids(db).await.unwrap(), expected, "ids: {what}");
    assert_eq!(
        query.count(db).await.unwrap(),
        count_of(expected.len()),
        "count: {what}"
    );
    let page = query.items(db, 0, 40).await.unwrap();
    let first: Vec<FileId> = expected.iter().copied().take(40).collect();
    assert_eq!(item_ids(&page), first, "items: {what}");
}

#[tokio::test]
async fn every_scope_agrees_with_the_reference() {
    let mut fixture = Fixture::library().await;
    let filters = qs::filter_matrix(&mut fixture).await;
    let probe = pick(
        &filters,
        &[
            "none",
            "camera, other case",
            "unrated or three",
            "selected or rejected",
            "tags, other case",
            "selection and tag",
        ],
    );
    for scope in &qs::scope_matrix(&fixture) {
        for matrix in &probe {
            for kinds in KINDS {
                check_against_reference(&fixture, scope, matrix, kinds).await;
            }
        }
    }
}

#[tokio::test]
async fn every_filter_agrees_with_the_reference() {
    let mut fixture = Fixture::library().await;
    let filters = qs::filter_matrix(&mut fixture).await;
    let scopes = [
        qs::scope(Some(fixture.photos), "", true),
        qs::scope(Some(fixture.photos), "2024", true),
        qs::scope(Some(fixture.photos), "a/b", false),
        qs::scope(Some(fixture.backup), "", false),
        qs::scope(None, "", true),
        qs::scope(None, "2024", true),
    ];
    for matrix in &filters {
        for scope in &scopes {
            for kinds in KINDS {
                check_against_reference(&fixture, scope, matrix, kinds).await;
            }
        }
    }
}

#[tokio::test]
async fn the_matrix_filters_select_something_unless_they_name_nothing() {
    let mut fixture = Fixture::library().await;
    let filters = qs::filter_matrix(&mut fixture).await;
    let everywhere = qs::scope(None, "", true);
    let total = qs::query(&everywhere, &Filter::default(), KindFilter::Assets)
        .count(fixture.db())
        .await
        .unwrap();
    assert!(total > 100, "the library is too small to mean anything");
    for matrix in &filters {
        let count = qs::query(&everywhere, &matrix.filter, KindFilter::Assets)
            .count(fixture.db())
            .await
            .unwrap();
        if matrix.may_be_empty {
            assert_eq!(count, 0, "{} should match nothing", matrix.label);
        } else {
            assert!(count > 0, "{} matches nothing", matrix.label);
        }
        if !["none", "blank entries"].contains(&matrix.label) {
            assert!(count < total, "{} excludes nothing", matrix.label);
        }
    }
}

#[tokio::test]
async fn every_sort_agrees_with_the_reference_and_pages_are_stable() {
    let mut fixture = Fixture::library().await;
    let filters = qs::filter_matrix(&mut fixture).await;
    let probe = pick(&filters, &["none", "tags, other case", "everything"]);
    let scopes = [
        qs::scope(None, "", true),
        qs::scope(Some(fixture.photos), "2024", true),
        qs::scope(Some(fixture.photos), "", false),
    ];
    let db = fixture.db();
    for scope in &scopes {
        for matrix in &probe {
            for kinds in KINDS {
                for (sort, order) in SORTS {
                    let what = format!(
                        "{scope:?} / {} / {kinds:?} / {sort:?} {order:?}",
                        matrix.label
                    );
                    let query = qs::query(scope, &matrix.filter, kinds).sorted(sort, order);
                    let expected =
                        fixture
                            .model
                            .expected_ids(scope, &matrix.filter, kinds, sort, order);
                    assert_eq!(query.ids(db).await.unwrap(), expected, "ids: {what}");

                    // Names, capture times and sizes tie all over the library: the pages only
                    // concatenate to the whole list when the order is total.
                    let mut paged = Vec::new();
                    let mut offset = 0;
                    loop {
                        let page = query.items(db, offset, 7).await.unwrap();
                        if page.is_empty() {
                            break;
                        }
                        offset += count_of(page.len());
                        paged.extend(item_ids(&page));
                    }
                    assert_eq!(paged, expected, "paged: {what}");
                }
            }
        }
    }
}

#[tokio::test]
async fn files_without_a_capture_time_come_last_in_both_directions() {
    let mut fixture = Fixture::empty().await;
    let photos = fixture.photos;
    for (name, capture) in [
        ("a.jpg", Some(200)),
        ("b.jpg", Some(200)),
        ("c.jpg", Some(100)),
        ("d.jpg", None),
        ("e.jpg", None),
        ("f.jpg", Some(300)),
    ] {
        let spec = Spec {
            capture,
            ..Spec::default()
        };
        fixture.add(photos, "d", name, spec).await;
    }
    let db = fixture.db();
    let base = qs::query(
        &qs::scope(Some(photos), "d", false),
        &Filter::default(),
        KindFilter::Assets,
    );
    let ascending = base.clone().sorted(SortKey::Date, SortOrder::Asc);
    assert_eq!(
        names(db, &ascending).await,
        ["c.jpg", "a.jpg", "b.jpg", "f.jpg", "d.jpg", "e.jpg"]
    );
    let descending = base.sorted(SortKey::Date, SortOrder::Desc);
    assert_eq!(
        names(db, &descending).await,
        ["f.jpg", "a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg"]
    );
}

#[tokio::test]
async fn items_carry_every_field_of_the_row() {
    let fixture = Fixture::empty().await;
    let db = fixture.db();
    let seed = Seed::new(db);
    let photo = seed
        .file(fixture.photos, "2024/trip", "IMG_1.jpg")
        .size(5_000)
        .mtime(1_700_000_123)
        .capture(1_718_000_000)
        .capture_src(CaptureSource::Xmp)
        .camera("Canon EOS R5")
        .rating(4)
        .flag(FlagFilter::Rejected)
        .dimensions(8192, 5464)
        .insert()
        .await;
    let clip = seed
        .file(fixture.photos, "2024/trip", "clip.mp4")
        .duration_ms(61_500)
        .flag(FlagFilter::Selected)
        .insert()
        .await;
    let query = qs::query(
        &qs::scope(Some(fixture.photos), "2024/trip", false),
        &Filter::default(),
        KindFilter::Assets,
    );
    let items = query.items(db, 0, 10).await.unwrap();
    let expected = vec![
        FileItem {
            id: clip,
            volume_id: fixture.photos,
            dir: "2024/trip".to_owned(),
            name: "clip.mp4".to_owned(),
            kind: FileKind::Video,
            size: 1000,
            capture_ts: None,
            capture_src: None,
            camera: None,
            rating: 0,
            flag: 1,
            width: None,
            height: None,
            duration_ms: Some(61_500),
            mtime: 1_700_000_000,
            in_immich: false,
            overlay: Overlay::default(),
        },
        FileItem {
            id: photo,
            volume_id: fixture.photos,
            dir: "2024/trip".to_owned(),
            name: "IMG_1.jpg".to_owned(),
            kind: FileKind::Image,
            size: 5_000,
            capture_ts: Some(1_718_000_000),
            capture_src: Some(CaptureSource::Xmp),
            camera: Some("Canon EOS R5".to_owned()),
            rating: 4,
            flag: -1,
            width: Some(8192),
            height: Some(5464),
            duration_ms: None,
            mtime: 1_700_000_123,
            in_immich: false,
            overlay: Overlay::default(),
        },
    ];
    assert_eq!(items, expected);
}

/// Five files of the directory `d` of the volume `photos`, and what is known about them:
///
/// | file | camera | rating | flag | tags |
/// |------|--------|--------|------|------|
/// | a.jpg | Canon | 5 | selected | holiday, family |
/// | b.jpg | Canon (other case) | 3 | rejected | holiday |
/// | c.jpg | Sony | 0 | none | |
/// | d.jpg | none | 4 | none | work |
/// | e.jpg | Sony | 5 | selected | family, work |
async fn five_files() -> (Fixture, [FileId; 5]) {
    let mut fixture = Fixture::empty().await;
    let photos = fixture.photos;
    let specs = [
        (
            "a.jpg",
            Some(CANON),
            5,
            Some(FlagFilter::Selected),
            vec!["holiday", "family"],
        ),
        (
            "b.jpg",
            Some("canon eos r5"),
            3,
            Some(FlagFilter::Rejected),
            vec!["holiday"],
        ),
        ("c.jpg", Some(SONY), 0, None, vec![]),
        ("d.jpg", None, 4, None, vec!["work"]),
        (
            "e.jpg",
            Some(SONY),
            5,
            Some(FlagFilter::Selected),
            vec!["family", "work"],
        ),
    ];
    let mut ids = Vec::new();
    for (name, camera, rating, flag, tags) in specs {
        let spec = Spec {
            camera,
            rating,
            flag,
            tags,
            ..Spec::default()
        };
        ids.push(fixture.add(photos, "d", name, spec).await);
    }
    (fixture, ids.try_into().unwrap())
}

async fn five_files_matching(fixture: &Fixture, filter: Filter) -> Vec<String> {
    let query = qs::query(
        &qs::scope(Some(fixture.photos), "d", false),
        &filter,
        KindFilter::Assets,
    );
    names(fixture.db(), &query).await
}

#[tokio::test]
async fn each_dimension_selects_what_it_says() {
    let (mut fixture, [_, b, c, d, _]) = five_files().await;
    let selection = fixture.select(&[b, c, d]).await;
    let matching = |filter: Filter| five_files_matching(&fixture, filter);

    // Cameras: any of them, whatever the case.
    assert_eq!(
        matching(qs::filter(&["CANON EOS R5"], &[], &[], &[], None)).await,
        ["a.jpg", "b.jpg"]
    );
    assert_eq!(
        matching(qs::filter(&[CANON, SONY], &[], &[], &[], None)).await,
        ["a.jpg", "b.jpg", "c.jpg", "e.jpg"]
    );
    assert!(
        matching(qs::filter(&["Nobody Cam"], &[], &[], &[], None))
            .await
            .is_empty()
    );

    // Ratings: 0 is unrated.
    assert_eq!(
        matching(qs::filter(&[], &[0], &[], &[], None)).await,
        ["c.jpg"]
    );
    assert_eq!(
        matching(qs::filter(&[], &[5], &[], &[], None)).await,
        ["a.jpg", "e.jpg"]
    );
    assert_eq!(
        matching(qs::filter(&[], &[0, 4], &[], &[], None)).await,
        ["c.jpg", "d.jpg"]
    );

    // Flags: an unflagged file is in no non-empty flag list.
    let selected = [FlagFilter::Selected];
    let rejected = [FlagFilter::Rejected];
    let both = [FlagFilter::Selected, FlagFilter::Rejected];
    assert_eq!(
        matching(qs::filter(&[], &[], &selected, &[], None)).await,
        ["a.jpg", "e.jpg"]
    );
    assert_eq!(
        matching(qs::filter(&[], &[], &rejected, &[], None)).await,
        ["b.jpg"]
    );
    assert_eq!(
        matching(qs::filter(&[], &[], &both, &[], None)).await,
        ["a.jpg", "b.jpg", "e.jpg"]
    );

    // Tags: any of them, whatever the case.
    assert_eq!(
        matching(qs::filter(&[], &[], &[], &["Holiday"], None)).await,
        ["a.jpg", "b.jpg"]
    );
    assert_eq!(
        matching(qs::filter(&[], &[], &[], &["holiday", "work"], None)).await,
        ["a.jpg", "b.jpg", "d.jpg", "e.jpg"]
    );

    // Dimensions combine with AND.
    assert_eq!(
        matching(qs::filter(&[CANON], &[5], &[], &[], None)).await,
        ["a.jpg"]
    );
    assert_eq!(
        matching(qs::filter(&[CANON], &[], &[], &["family"], None)).await,
        ["a.jpg"]
    );
    assert!(
        matching(qs::filter(&[SONY], &[], &rejected, &[], None))
            .await
            .is_empty()
    );

    // A selection restricts to its members, on top of the other dimensions.
    assert_eq!(
        matching(qs::filter(&[], &[], &[], &[], Some(selection))).await,
        ["b.jpg", "c.jpg", "d.jpg"]
    );
    assert_eq!(
        matching(qs::filter(&[], &[], &[], &["holiday"], Some(selection))).await,
        ["b.jpg"]
    );

    // Nothing selected: everything.
    assert_eq!(matching(Filter::default()).await.len(), 5);
}

#[tokio::test]
async fn assets_leave_out_sidecars_and_all_does_not() {
    let mut fixture = Fixture::empty().await;
    let photos = fixture.photos;
    for name in ["a.jpg", "b.mp4", "c.wav", "d.xmp", "e.txt"] {
        fixture.add(photos, "k", name, Spec::default()).await;
    }
    let db = fixture.db();
    let scope = qs::scope(Some(photos), "k", false);
    let assets = qs::query(&scope, &Filter::default(), KindFilter::default());
    assert_eq!(
        names(db, &assets).await,
        ["a.jpg", "b.mp4", "c.wav", "e.txt"]
    );
    assert_eq!(assets.count(db).await.unwrap(), 4);
    let all = qs::query(&scope, &Filter::default(), KindFilter::All);
    assert_eq!(
        names(db, &all).await,
        ["a.jpg", "b.mp4", "c.wav", "d.xmp", "e.txt"]
    );
    assert_eq!(all.count(db).await.unwrap(), 5);
}

#[tokio::test]
async fn scopes_pick_exactly_their_directories() {
    let mut fixture = Fixture::empty().await;
    let (photos, backup) = (fixture.photos, fixture.backup);
    for (volume_id, dir, name) in [
        (photos, "", "r.jpg"),
        (photos, "2024", "x.jpg"),
        (photos, "2024/05", "y.jpg"),
        (photos, "2024/05/day 1", "z.jpg"),
        (photos, "2024-05", "p.jpg"),
        (photos, "2024x", "q.jpg"),
        (photos, "2025", "s.jpg"),
        (photos, "100%_done", "u.jpg"),
        (photos, "100abcdone", "v.jpg"),
        (photos, "it's", "w.jpg"),
        (backup, "", "br.jpg"),
        (backup, "2024", "bx.jpg"),
    ] {
        fixture.add(volume_id, dir, name, Spec::default()).await;
    }
    let db = fixture.db();
    let cases: [(Option<i64>, &str, bool, &[&str]); 17] = [
        (
            Some(photos),
            "",
            true,
            &["p", "q", "r", "s", "u", "v", "w", "x", "y", "z"],
        ),
        (Some(photos), "", false, &["r"]),
        (Some(photos), "2024", true, &["x", "y", "z"]),
        (Some(photos), "2024", false, &["x"]),
        (Some(photos), "2024/05", true, &["y", "z"]),
        (Some(photos), "2024/05/day 1", false, &["z"]),
        (Some(photos), "2024-05", true, &["p"]),
        (Some(photos), "100%_done", true, &["u"]),
        (Some(photos), "it's", true, &["w"]),
        (None, "2024", true, &["bx", "x", "y", "z"]),
        (None, "", false, &["br", "r"]),
        (
            None,
            "",
            true,
            &["br", "bx", "p", "q", "r", "s", "u", "v", "w", "x", "y", "z"],
        ),
        (Some(backup), "2024", false, &["bx"]),
        (Some(photos), "nowhere", true, &[]),
        (Some(9999), "", true, &[]),
        (Some(photos), "2024/05/", true, &["y", "z"]),
        (Some(photos), "./2024", true, &["x", "y", "z"]),
    ];
    for (volume_id, dir, recursive, expected) in cases {
        let scope = qs::scope(volume_id, dir, recursive);
        let query = qs::query(&scope, &Filter::default(), KindFilter::Assets);
        let expected: Vec<String> = expected.iter().map(|stem| format!("{stem}.jpg")).collect();
        assert_eq!(names(db, &query).await, expected, "{scope:?}");
        assert_eq!(
            query.count(db).await.unwrap(),
            count_of(expected.len()),
            "{scope:?}"
        );
    }
}

#[tokio::test]
async fn hostile_values_are_bound_not_spliced() {
    let fixture = Fixture::library().await;
    let cameras = qs::filter(&[QUOTE_CAMERA, WILD_CAMERA], &[], &[], &[], None);
    let tags = qs::filter(&[], &[], &[], &[DROP_TAG, "%", "_"], None);
    let both = qs::filter(
        &[QUOTE_CAMERA, WILD_CAMERA],
        &[],
        &[],
        &[DROP_TAG, "%", "_"],
        None,
    );
    let scopes = [
        qs::scope(Some(fixture.photos), "", true),
        qs::scope(Some(fixture.photos), "it's", true),
    ];
    for (filter, matches_something) in [(&cameras, true), (&tags, true), (&both, false)] {
        for scope in &scopes {
            let what = format!("{scope:?} / {filter:?}");
            let query = qs::query(scope, filter, KindFilter::Assets);
            let mut statement = QueryBuilder::<Sqlite>::new("SELECT f.id FROM files f WHERE ");
            query.push_where(&mut statement, "f");
            let sql = statement.sql().as_str().to_owned();
            for fragment in ["DROP", "OR '1'", "X-T4", "it's", "100", "%"] {
                assert!(!sql.contains(fragment), "{fragment} in {sql}");
            }

            let mut ids: Vec<FileId> = statement
                .build_query_scalar()
                .fetch_all(fixture.db().read())
                .await
                .unwrap();
            ids.sort_unstable();
            let mut expected = fixture.model.expected_ids(
                scope,
                filter,
                KindFilter::Assets,
                SortKey::Name,
                SortOrder::Asc,
            );
            expected.sort_unstable();
            if matches_something && scope.dir.is_empty() {
                assert!(!expected.is_empty(), "nothing matches: {what}");
            }
            assert_eq!(ids, expected, "{what}");
        }
    }

    // The statements above would have dropped the table had the tag been spliced into the SQL.
    let files: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
        .fetch_one(fixture.db().read())
        .await
        .unwrap();
    assert_eq!(files, count_of(fixture.model.rows.len()));
}

#[tokio::test]
async fn invalid_input_is_a_bad_request() {
    fn assert_bad_request(error: QueryError) {
        let app = AppError::from(error);
        assert_eq!(app.status(), StatusCode::BAD_REQUEST);
        assert_eq!(app.code(), "bad_request");
    }

    let root = QueryScope::try_from(&Scope::default()).unwrap();
    let too_many: Vec<String> = (0..201).map(|n| format!("tag{n}")).collect();
    let filters = [
        Filter {
            ratings: vec![6],
            ..Filter::default()
        },
        Filter {
            tags: too_many,
            ..Filter::default()
        },
        Filter {
            cameras: vec!["c".repeat(201)],
            ..Filter::default()
        },
    ];
    for filter in &filters {
        let error = FileQuery::new(root.clone(), filter).unwrap_err();
        assert!(matches!(error, QueryError::Filter(_)), "{error:?}");
        assert_bad_request(error);
    }
    for dir in ["../outside", "/absolute", "a/../../b", "nul\0byte"] {
        let scope = Scope {
            dir: dir.to_owned(),
            ..Scope::default()
        };
        let error = QueryScope::try_from(&scope).unwrap_err();
        assert!(matches!(error, QueryError::Scope(_)), "{dir}: {error:?}");
        assert_bad_request(error);
    }
}

#[tokio::test]
async fn the_query_keeps_the_canonical_filter() {
    let root = QueryScope::try_from(&Scope::default()).unwrap();
    let padded = qs::filter(
        &[" Canon EOS R5 ", "canon eos r5", " "],
        &[3, 3],
        &[],
        &["b", "A", "a "],
        None,
    );
    let query = FileQuery::new(root, &padded)
        .unwrap()
        .kinds(KindFilter::All)
        .sorted(SortKey::Size, SortOrder::Desc);
    assert_eq!(query.filter().cameras.len(), 1);
    assert!(query.filter().cameras[0].eq_ignore_ascii_case("canon eos r5"));
    assert_eq!(query.filter().ratings, [3]);
    assert_eq!(query.filter().tags.len(), 2);
    assert_eq!(query.kind_filter(), KindFilter::All);
    assert_eq!(query.sort(), (SortKey::Size, SortOrder::Desc));
    assert_eq!(query.scope().dir.as_str(), "");
}

#[tokio::test]
async fn pages_are_clamped() {
    let fixture = Fixture::empty().await;
    let db = fixture.db();
    let seed = Seed::new(db);
    for n in 0..510 {
        seed.file(fixture.photos, "many", &format!("f{n:04}.jpg"))
            .insert()
            .await;
    }
    let query = qs::query(
        &qs::scope(Some(fixture.photos), "many", false),
        &Filter::default(),
        KindFilter::Assets,
    );
    assert_eq!(query.count(db).await.unwrap(), 510);
    assert_eq!(query.items(db, 0, 10_000).await.unwrap().len(), 500);
    assert_eq!(query.items(db, 0, 0).await.unwrap().len(), 1);
    assert_eq!(query.items(db, 0, -3).await.unwrap().len(), 1);
    assert_eq!(query.items(db, 505, 100).await.unwrap().len(), 5);
    assert!(query.items(db, 100_000, 10).await.unwrap().is_empty());
    let from_the_start = query.items(db, 0, 3).await.unwrap();
    assert_eq!(query.items(db, -5, 3).await.unwrap(), from_the_start);
}

/// `push_where` is the one place that turns a query into SQL, so other crates' statements embed it.
mod embedding {
    use super::*;

    fn statement(
        prefix: &str,
        query: &FileQuery,
        alias: &str,
        suffix: &str,
    ) -> QueryBuilder<Sqlite> {
        let mut statement = QueryBuilder::new(prefix);
        query.push_where(&mut statement, alias);
        statement.push(suffix);
        statement
    }

    async fn fixture_and_query() -> (Fixture, FileQuery) {
        let mut fixture = Fixture::library().await;
        let filters = qs::filter_matrix(&mut fixture).await;
        let matrix = pick(&filters, &["tags, other case"])[0];
        let scope = qs::scope(Some(fixture.photos), "2024", true);
        let query = qs::query(&scope, &matrix.filter, KindFilter::Assets);
        (fixture, query)
    }

    #[tokio::test]
    async fn insert_select_fills_a_selection() {
        let (fixture, query) = fixture_and_query().await;
        let db = fixture.db();
        let selection = Seed::new(db).selection(&[]).await;
        let mut insert = QueryBuilder::<Sqlite>::new(
            "INSERT INTO selection_items (selection_id, file_id) SELECT ",
        );
        insert.push_bind(selection);
        insert.push(", f.id FROM files f WHERE ");
        query.push_where(&mut insert, "f");
        let inserted = insert
            .build()
            .execute(db.write())
            .await
            .unwrap()
            .rows_affected();

        let mut expected = query.ids(db).await.unwrap();
        expected.sort_unstable();
        assert!(!expected.is_empty());
        assert_eq!(inserted, u64::try_from(expected.len()).unwrap());
        let members: Vec<FileId> = sqlx::query_scalar(
            "SELECT file_id FROM selection_items WHERE selection_id = ? ORDER BY file_id",
        )
        .bind(selection)
        .fetch_all(db.read())
        .await
        .unwrap();
        assert_eq!(members, expected);
    }

    #[tokio::test]
    async fn case_when_counts_the_matches() {
        let (fixture, query) = fixture_and_query().await;
        let db = fixture.db();
        let mut statement = statement(
            "SELECT COALESCE(SUM(CASE WHEN ",
            &query,
            "f",
            " THEN 1 ELSE 0 END), 0), COUNT(*) FROM files f",
        );
        let (matching, all): (i64, i64) = statement
            .build_query_as()
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(matching, query.count(db).await.unwrap());
        assert!(matching > 0);
        assert_eq!(all, count_of(fixture.model.rows.len()));
    }

    #[tokio::test]
    async fn update_and_delete_touch_the_matches_only() {
        let (fixture, query) = fixture_and_query().await;
        let db = fixture.db();
        let matching = u64::try_from(query.count(db).await.unwrap()).unwrap();
        assert!(matching > 0);
        let total = u64::try_from(fixture.model.rows.len()).unwrap();

        let mut tx = db.begin_write().await.unwrap();
        let updated = statement("UPDATE files AS f SET rating = 5 WHERE ", &query, "f", "")
            .build()
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(updated, matching);
        let deleted = statement("DELETE FROM files AS f WHERE ", &query, "f", "")
            .build()
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(deleted, matching);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(left, i64::try_from(total - matching).unwrap());
        tx.rollback().await.unwrap();

        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
            .fetch_one(db.read())
            .await
            .unwrap();
        assert_eq!(after, i64::try_from(total).unwrap());
    }

    #[tokio::test]
    async fn any_alias_works() {
        let (fixture, query) = fixture_and_query().await;
        let db = fixture.db();
        let mut ids: Vec<FileId> = statement("SELECT x.id FROM files x WHERE ", &query, "x", "")
            .build_query_scalar()
            .fetch_all(db.read())
            .await
            .unwrap();
        ids.sort_unstable();
        let mut expected = query.ids(db).await.unwrap();
        expected.sort_unstable();
        assert_eq!(ids, expected);
    }

    #[tokio::test]
    async fn a_query_that_constrains_nothing_is_a_true_expression() {
        let everything = qs::query(
            &qs::scope(None, "", true),
            &Filter::default(),
            KindFilter::All,
        );
        let mut sql = QueryBuilder::<Sqlite>::new("");
        everything.push_where(&mut sql, "f");
        assert_eq!(sql.sql().as_str(), "(1 = 1)");
    }
}

fn facet_pairs(facets: &Facets) -> FacetsExpect {
    let pairs = |list: &[opm_core::dto::FacetCount]| {
        list.iter()
            .map(|facet| (facet.value.clone(), facet.count))
            .collect()
    };
    FacetsExpect {
        cameras: pairs(&facets.cameras),
        tags: pairs(&facets.tags),
        ratings: pairs(&facets.ratings),
    }
}

fn expected(list: &[(&str, i64)]) -> Vec<(String, i64)> {
    list.iter()
        .map(|(value, count)| ((*value).to_owned(), *count))
        .collect()
}

#[tokio::test]
async fn facets_agree_with_the_reference() {
    let mut fixture = Fixture::library().await;
    let filters = qs::filter_matrix(&mut fixture).await;
    let scopes = [
        qs::scope(Some(fixture.photos), "", true),
        qs::scope(Some(fixture.photos), "2024", true),
        qs::scope(Some(fixture.photos), "a/b", false),
        qs::scope(Some(fixture.backup), "", false),
        qs::scope(None, "", true),
        qs::scope(None, "nowhere", true),
    ];
    let db = fixture.db();
    for matrix in &filters {
        for scope in &scopes {
            for kinds in KINDS {
                let what = format!("{scope:?} / {} / {kinds:?}", matrix.label);
                let facets = qs::query(scope, &matrix.filter, kinds)
                    .facets(db)
                    .await
                    .unwrap();
                assert_eq!(
                    facet_pairs(&facets),
                    fixture.model.facets(scope, &matrix.filter, kinds),
                    "{what}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_facet_counts_without_its_own_filter() {
    let (mut fixture, [_, b, c, d, _]) = five_files().await;
    let selection = fixture.select(&[b, c, d]).await;
    let db = fixture.db();
    let facets = |filter: Filter| {
        let query = qs::query(
            &qs::scope(Some(fixture.photos), "d", false),
            &filter,
            KindFilter::Assets,
        );
        async move { facet_pairs(&query.facets(db).await.unwrap()) }
    };

    let everything = facets(Filter::default()).await;
    assert_eq!(everything.cameras, expected(&[(CANON, 2), (SONY, 2)]));
    assert_eq!(
        everything.tags,
        expected(&[("family", 2), ("holiday", 2), ("work", 2)])
    );
    assert_eq!(
        everything.ratings,
        expected(&[("5", 2), ("0", 1), ("3", 1), ("4", 1)])
    );

    // Choosing a camera keeps the other cameras listed and narrows the rest.
    let canon = facets(qs::filter(&[CANON], &[], &[], &[], None)).await;
    assert_eq!(canon.cameras, everything.cameras);
    assert_eq!(canon.tags, expected(&[("holiday", 2), ("family", 1)]));
    assert_eq!(canon.ratings, expected(&[("3", 1), ("5", 1)]));

    let work = facets(qs::filter(&[], &[], &[], &["work"], None)).await;
    assert_eq!(work.cameras, expected(&[(SONY, 1)]));
    assert_eq!(work.tags, everything.tags);
    assert_eq!(work.ratings, expected(&[("4", 1), ("5", 1)]));

    let five = facets(qs::filter(&[], &[5], &[], &[], None)).await;
    assert_eq!(five.cameras, expected(&[(CANON, 1), (SONY, 1)]));
    assert_eq!(
        five.tags,
        expected(&[("family", 2), ("holiday", 1), ("work", 1)])
    );
    assert_eq!(five.ratings, everything.ratings);

    // The flag and the selection are not facets: they always narrow all three.
    let rejected = facets(qs::filter(&[], &[], &[FlagFilter::Rejected], &[], None)).await;
    assert_eq!(rejected.cameras, expected(&[(CANON, 1)]));
    assert_eq!(rejected.tags, expected(&[("holiday", 1)]));
    assert_eq!(rejected.ratings, expected(&[("3", 1)]));

    let selected = facets(qs::filter(&[], &[], &[], &[], Some(selection))).await;
    assert_eq!(selected.cameras, expected(&[(CANON, 1), (SONY, 1)]));
    assert_eq!(selected.tags, expected(&[("holiday", 1), ("work", 1)]));
    assert_eq!(selected.ratings, expected(&[("0", 1), ("3", 1), ("4", 1)]));
}

#[tokio::test]
async fn a_facet_lists_at_most_two_hundred_values() {
    let fixture = Fixture::empty().await;
    let db = fixture.db();
    let seed = Seed::new(db);
    let names: Vec<String> = (0..230).map(|n| format!("t{n:03}")).collect();
    seed.file(fixture.photos, "tags", "many.jpg")
        .tags(names.iter().map(String::as_str))
        .insert()
        .await;
    for n in 0..3 {
        seed.file(fixture.photos, "tags", &format!("again{n}.jpg"))
            .tags(["t229"])
            .insert()
            .await;
    }
    let facets = qs::query(
        &qs::scope(Some(fixture.photos), "tags", false),
        &Filter::default(),
        KindFilter::Assets,
    )
    .facets(db)
    .await
    .unwrap();
    assert_eq!(facets.tags.len(), 200);
    assert_eq!(
        (facets.tags[0].value.as_str(), facets.tags[0].count),
        ("t229", 4)
    );
    assert_eq!(facets.tags[1].value, "t000");
    assert_eq!(facets.tags[199].value, "t198");
}

#[tokio::test]
async fn the_detail_gathers_hash_mime_tags_and_sidecars() {
    let fixture = Fixture::empty().await;
    let db = fixture.db();
    let seed = Seed::new(db);
    let photo = seed
        .file(fixture.photos, "2024", "IMG_1.jpg")
        .size(4096)
        .mtime(1_700_000_500)
        .btime(1_600_000_000)
        .capture(1_700_000_000)
        .camera("Canon EOS R5")
        .rating(3)
        .hash([0xab; 20])
        .mime("image/jpeg")
        .tags(["holiday", "Family", "beach"])
        .insert()
        .await;
    let exact = seed
        .file(fixture.photos, "2024", "IMG_1.jpg.xmp")
        .insert()
        .await;
    let stem = seed
        .file(fixture.photos, "2024", "IMG_1.XMP")
        .insert()
        .await;
    seed.sidecar(photo, exact, "exact").await;
    seed.sidecar(photo, stem, "stem").await;

    let detail = file_detail(db, photo).await.unwrap();
    assert_eq!(detail.item.id, photo);
    assert_eq!(detail.item.name, "IMG_1.jpg");
    assert_eq!(detail.item.camera.as_deref(), Some("Canon EOS R5"));
    assert_eq!(detail.item.rating, 3);
    assert_eq!(detail.item.mtime, 1_700_000_500);
    assert!(!detail.item.in_immich);
    assert_eq!(detail.hash, Some("ab".repeat(20)));
    assert_eq!(detail.mime.as_deref(), Some("image/jpeg"));
    assert_eq!(detail.btime, Some(1_600_000_000));
    assert_eq!(detail.tags, ["beach", "Family", "holiday"]);
    assert_eq!(
        detail.sidecars,
        [
            SidecarDto {
                name: "IMG_1.jpg.xmp".to_owned(),
                kind: "exact".to_owned()
            },
            SidecarDto {
                name: "IMG_1.XMP".to_owned(),
                kind: "stem".to_owned()
            },
        ]
    );

    // A bare file has nothing optional.
    let bare = seed.file(fixture.photos, "2024", "bare.dat").insert().await;
    let detail = file_detail(db, bare).await.unwrap();
    assert_eq!(detail.hash, None);
    assert_eq!(detail.mime, None);
    assert_eq!(detail.btime, None);
    assert!(detail.tags.is_empty());
    assert!(detail.sidecars.is_empty());
}

#[tokio::test]
async fn an_unknown_file_has_no_detail() {
    let fixture = Fixture::empty().await;
    let error = file_detail(fixture.db(), 424_242).await.unwrap_err();
    assert!(
        matches!(error, QueryError::FileNotFound(424_242)),
        "{error:?}"
    );
    let app = AppError::from(error);
    assert_eq!(app.status(), StatusCode::NOT_FOUND);
    assert_eq!(app.code(), "not_found");
}

#[tokio::test]
async fn nothing_matches_in_a_missing_directory_or_volume() {
    let fixture = Fixture::library().await;
    let db = fixture.db();
    let scopes = [
        qs::scope(Some(fixture.photos), "does/not/exist", true),
        qs::scope(Some(fixture.photos), "does/not/exist", false),
        qs::scope(Some(99_999), "", true),
        qs::scope(Some(99_999), "2024", false),
        qs::scope(None, "does/not/exist", true),
    ];
    for scope in &scopes {
        let query = qs::query(scope, &Filter::default(), KindFilter::All);
        assert!(query.ids(db).await.unwrap().is_empty(), "{scope:?}");
        assert_eq!(query.count(db).await.unwrap(), 0, "{scope:?}");
        assert!(
            query.items(db, 0, 50).await.unwrap().is_empty(),
            "{scope:?}"
        );
        let facets = query.facets(db).await.unwrap();
        assert!(facets.cameras.is_empty() && facets.tags.is_empty() && facets.ratings.is_empty());
    }
}
