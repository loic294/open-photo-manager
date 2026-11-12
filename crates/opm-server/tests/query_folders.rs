//! `FileQuery::folders` and `footer_counts` against the in-memory reference of `query_support`,
//! plus hand-written cases for the rules the reference shares with the engine: empty and nested
//! folders, counts with and without the filter, the sort of the names.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod query_support;

use axum::http::StatusCode;
use opm_core::dto::{Filter, FlagFilter, FolderEntry, FooterCounts, Overlay};
use opm_server::db::Db;
use opm_server::error::AppError;
use opm_server::query::{FileQuery, FolderRow, KindFilter, QueryError};
use query_support as qs;
use query_support::{Fixture, FolderExpect, MatrixFilter, Spec};

const KINDS: [KindFilter; 2] = [KindFilter::Assets, KindFilter::All];

fn expect(row: FolderRow) -> FolderExpect {
    FolderExpect {
        name: row.name,
        path: row.path,
        volume_id: row.volume_id,
        count: row.count,
        total: row.total,
    }
}

async fn folders(db: &Db, query: &FileQuery) -> Vec<FolderExpect> {
    query
        .folders(db)
        .await
        .unwrap()
        .into_iter()
        .map(expect)
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

/// `(name, count, total)` of each folder, in order.
fn summary(list: &[FolderExpect]) -> Vec<(&str, i64, i64)> {
    list.iter()
        .map(|folder| (folder.name.as_str(), folder.count, folder.total))
        .collect()
}

#[tokio::test]
async fn folders_and_footers_agree_with_the_reference() {
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
            "empty selection",
        ],
    );
    let db = fixture.db();
    let mut listed = 0;
    for scope in &qs::scope_matrix(&fixture) {
        for matrix in &probe {
            for kinds in KINDS {
                let what = format!("{scope:?} / {} / {kinds:?}", matrix.label);
                let query = qs::query(scope, &matrix.filter, kinds);
                if scope.volume_id.is_some() {
                    let expected = fixture.model.folders(scope, &matrix.filter, kinds);
                    listed += expected.len();
                    assert_eq!(folders(db, &query).await, expected, "folders: {what}");
                }
                let (in_folder, in_subfolders) = fixture.model.footer(scope, &matrix.filter, kinds);
                assert_eq!(
                    query.footer_counts(db).await.unwrap(),
                    FooterCounts {
                        in_folder,
                        in_subfolders
                    },
                    "footer: {what}"
                );
            }
        }
    }
    assert!(listed > 500, "the matrix listed only {listed} folders");
}

/// ```text
/// (root)   root.jpg
/// a        a1.jpg (5 stars)  a2.jpg (1 star)
/// a/b      b1.jpg (5 stars)
/// a/b/c    c1.jpg (1 star)   c2.xmp (a sidecar)
/// d/e      nothing
/// f        f1.xmp (a sidecar)
/// ```
async fn nested() -> Fixture {
    let mut fixture = Fixture::empty().await;
    let photos = fixture.photos;
    for (dir, name, rating) in [
        ("", "root.jpg", 0),
        ("a", "a1.jpg", 5),
        ("a", "a2.jpg", 1),
        ("a/b", "b1.jpg", 5),
        ("a/b/c", "c1.jpg", 1),
        ("a/b/c", "c2.xmp", 0),
        ("f", "f1.xmp", 0),
    ] {
        let spec = Spec {
            rating,
            ..Spec::default()
        };
        fixture.add(photos, dir, name, spec).await;
    }
    fixture.add_dir(photos, "d/e").await;
    fixture
}

fn rated(ratings: &[u8]) -> Filter {
    qs::filter(&[], ratings, &[], &[], None)
}

fn here(fixture: &Fixture, dir: &str, filter: &Filter, kinds: KindFilter) -> FileQuery {
    qs::query(&qs::scope(Some(fixture.photos), dir, false), filter, kinds)
}

#[tokio::test]
async fn a_folder_counts_its_whole_subtree() {
    let fixture = nested().await;
    let db = fixture.db();
    let none = Filter::default();

    let root = folders(db, &here(&fixture, "", &none, KindFilter::Assets)).await;
    // The sidecars are not assets: `f` and the sidecar of `a/b/c` count for nothing.
    assert_eq!(summary(&root), [("a", 4, 4), ("d", 0, 0), ("f", 0, 0)]);
    assert_eq!(root[0].path, "a");
    assert_eq!(root[0].volume_id, fixture.photos);

    let below = folders(db, &here(&fixture, "a", &none, KindFilter::Assets)).await;
    assert_eq!(summary(&below), [("b", 2, 2)]);
    assert_eq!(below[0].path, "a/b");

    let deepest = folders(db, &here(&fixture, "a/b", &none, KindFilter::Assets)).await;
    assert_eq!(summary(&deepest), [("c", 1, 1)]);
    assert_eq!(deepest[0].path, "a/b/c");

    // Empty directories are listed, and so are the ones holding only empty directories.
    let empty = folders(db, &here(&fixture, "d", &none, KindFilter::Assets)).await;
    assert_eq!(summary(&empty), [("e", 0, 0)]);
    assert_eq!(empty[0].path, "d/e");

    // A directory without subdirectories, and one that does not exist, list nothing.
    assert!(
        folders(db, &here(&fixture, "a/b/c", &none, KindFilter::Assets))
            .await
            .is_empty()
    );
    assert!(
        folders(db, &here(&fixture, "nowhere", &none, KindFilter::Assets))
            .await
            .is_empty()
    );

    // With the sidecars counted, `f` and `a/b/c` hold one more.
    let all = folders(db, &here(&fixture, "", &none, KindFilter::All)).await;
    assert_eq!(summary(&all), [("a", 5, 5), ("d", 0, 0), ("f", 1, 1)]);
}

#[tokio::test]
async fn the_filter_lowers_the_count_and_not_the_total() {
    let fixture = nested().await;
    let db = fixture.db();
    let five = rated(&[5]);

    let root = folders(db, &here(&fixture, "", &five, KindFilter::Assets)).await;
    assert_eq!(summary(&root), [("a", 2, 4), ("d", 0, 0), ("f", 0, 0)]);
    let below = folders(db, &here(&fixture, "a", &five, KindFilter::Assets)).await;
    assert_eq!(summary(&below), [("b", 1, 2)]);
    let deepest = folders(db, &here(&fixture, "a/b", &five, KindFilter::Assets)).await;
    assert_eq!(summary(&deepest), [("c", 0, 1)]);

    // A filter nothing passes zeroes every count and keeps the totals.
    let flagged = qs::filter(&[], &[], &[FlagFilter::Selected], &[], None);
    let root = folders(db, &here(&fixture, "", &flagged, KindFilter::Assets)).await;
    assert_eq!(summary(&root), [("a", 0, 4), ("d", 0, 0), ("f", 0, 0)]);
}

#[tokio::test]
async fn the_footer_splits_the_folder_from_what_is_below() {
    let fixture = nested().await;
    let db = fixture.db();
    let counts = |in_folder, in_subfolders| FooterCounts {
        in_folder,
        in_subfolders,
    };
    let footer = |dir: &'static str, recursive: bool, filter: Filter, kinds: KindFilter| {
        let scope = qs::scope(Some(fixture.photos), dir, recursive);
        let query = qs::query(&scope, &filter, kinds);
        async move { query.footer_counts(db).await.unwrap() }
    };

    // The same numbers whether or not the listing itself is recursive.
    for recursive in [false, true] {
        let none = Filter::default;
        assert_eq!(
            footer("", recursive, none(), KindFilter::Assets).await,
            counts(1, 4)
        );
        assert_eq!(
            footer("a", recursive, none(), KindFilter::Assets).await,
            counts(2, 2)
        );
        assert_eq!(
            footer("a/b", recursive, none(), KindFilter::Assets).await,
            counts(1, 1)
        );
        assert_eq!(
            footer("a/b/c", recursive, none(), KindFilter::Assets).await,
            counts(1, 0)
        );
        assert_eq!(
            footer("a/b/c", recursive, none(), KindFilter::All).await,
            counts(2, 0)
        );
        assert_eq!(
            footer("d", recursive, none(), KindFilter::Assets).await,
            counts(0, 0)
        );
        assert_eq!(
            footer("nowhere", recursive, none(), KindFilter::Assets).await,
            counts(0, 0)
        );
        assert_eq!(
            footer("", recursive, rated(&[5]), KindFilter::Assets).await,
            counts(0, 2)
        );
        assert_eq!(
            footer("a", recursive, rated(&[5]), KindFilter::Assets).await,
            counts(1, 1)
        );
    }
}

#[tokio::test]
async fn the_footer_of_every_volume_adds_the_volumes_up() {
    let mut fixture = nested().await;
    let backup = fixture.backup;
    fixture
        .add(backup, "", "backup-root.jpg", Spec::default())
        .await;
    fixture
        .add(backup, "a", "backup-a.jpg", Spec::default())
        .await;
    fixture
        .add(backup, "a/z", "backup-z.jpg", Spec::default())
        .await;
    let db = fixture.db();
    let footer = |dir: &str, volume_id| {
        qs::query(
            &qs::scope(volume_id, dir, false),
            &Filter::default(),
            KindFilter::Assets,
        )
    };
    assert_eq!(
        footer("", None).footer_counts(db).await.unwrap(),
        FooterCounts {
            in_folder: 2,
            in_subfolders: 6
        }
    );
    assert_eq!(
        footer("a", None).footer_counts(db).await.unwrap(),
        FooterCounts {
            in_folder: 3,
            in_subfolders: 3
        }
    );
    assert_eq!(
        footer("a", Some(backup)).footer_counts(db).await.unwrap(),
        FooterCounts {
            in_folder: 1,
            in_subfolders: 1
        }
    );
}

#[tokio::test]
async fn folders_are_sorted_by_normalised_name_then_name() {
    let mut fixture = Fixture::empty().await;
    let photos = fixture.photos;
    let nfc = "\u{e9}t\u{e9}";
    let nfd = "e\u{301}te\u{301}";
    for dir in [
        "zed", "Alpha", "alpha", "beta", "Zed2", nfc, nfd, "10", "9", "_under",
    ] {
        fixture
            .add(photos, &format!("top/{dir}"), "x.jpg", Spec::default())
            .await;
    }
    let db = fixture.db();
    let query = here(&fixture, "top", &Filter::default(), KindFilter::Assets);
    let names: Vec<String> = folders(db, &query)
        .await
        .into_iter()
        .map(|folder| folder.name)
        .collect();
    // The case-folded name decides; equal ones fall back to the name itself, bytewise.
    assert_eq!(
        names,
        [
            "10", "9", "_under", "Alpha", "alpha", "beta", "zed", "Zed2", nfd, nfc
        ]
    );
}

#[tokio::test]
async fn listing_the_folders_of_every_volume_is_refused() {
    let fixture = nested().await;
    let query = qs::query(
        &qs::scope(None, "", false),
        &Filter::default(),
        KindFilter::Assets,
    );
    let error = query.folders(fixture.db()).await.unwrap_err();
    assert!(matches!(error, QueryError::VolumeRequired), "{error:?}");
    let app = AppError::from(error);
    assert_eq!(app.status(), StatusCode::BAD_REQUEST);
    assert_eq!(app.code(), "bad_request");
}

#[tokio::test]
async fn a_folder_row_becomes_a_plain_folder_entry() {
    let row = FolderRow {
        name: "b".to_owned(),
        path: "a/b".to_owned(),
        volume_id: 7,
        count: 3,
        total: 9,
    };
    assert_eq!(
        FolderEntry::from(row),
        FolderEntry {
            name: "b".to_owned(),
            path: "a/b".to_owned(),
            volume_id: 7,
            count: 3,
            total: 9,
            matches: None,
            overlay: Overlay::default(),
        }
    );
}

/// The folder list and the item list of one scope tell the same story.
#[tokio::test]
async fn folder_counts_match_the_items_below_each_folder() {
    let mut fixture = Fixture::library().await;
    let filters = qs::filter_matrix(&mut fixture).await;
    let db = fixture.db();
    let scope = qs::scope(Some(fixture.photos), "", false);
    for matrix in pick(&filters, &["none", "tags, other case", "everything"]) {
        let query = qs::query(&scope, &matrix.filter, KindFilter::Assets);
        for folder in query.folders(db).await.unwrap() {
            let below = qs::scope(Some(folder.volume_id), &folder.path, true);
            let count = qs::query(&below, &matrix.filter, KindFilter::Assets)
                .count(db)
                .await
                .unwrap();
            let total = qs::query(&below, &Filter::default(), KindFilter::Assets)
                .count(db)
                .await
                .unwrap();
            assert_eq!(
                (folder.count, folder.total),
                (count, total),
                "{} / {}",
                matrix.label,
                folder.path
            );
        }
    }
}
