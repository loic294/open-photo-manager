//! Tests of `migrations/0001_core.sql` itself: the tables, every constraint, the cascades, and the
//! query plans that the scanner, the query engine and the file operations count on.

use std::collections::BTreeMap;

use opm_core::consts::TRASH_DIR;
use opm_core::dto::{CaptureSource, FileKind, FlagFilter};
use opm_core::paths::{FileId, VolumeId};
use sqlx::error::ErrorKind;

use super::models::{
    capture_source_from_db, capture_source_to_db, file_kind_from_db, file_kind_to_db, flag_from_db,
    flag_to_db,
};
use super::testing::{insert_dir, insert_file, insert_volume};
use super::*;

const CORE_TABLES: [&str; 13] = [
    "cameras",
    "dirs",
    "file_sidecars",
    "file_tags",
    "files",
    "fs_journal",
    "selection_items",
    "selections",
    "settings",
    "tags",
    "tasks",
    "trash_items",
    "volumes",
];

/// Row counts of [`populate`]. Every test that deletes something compares against a variation.
const POPULATED: [(&str, i64); 13] = [
    ("cameras", 1),
    ("dirs", 3),
    ("file_sidecars", 1),
    ("file_tags", 2),
    ("files", 4),
    ("fs_journal", 1),
    ("selection_items", 2),
    ("selections", 1),
    ("settings", 1),
    ("tags", 1),
    ("tasks", 1),
    ("trash_items", 2),
    ("volumes", 2),
];

// ---------------------------------------------------------------------------------------------
// helpers

fn kind_of(err: &sqlx::Error) -> ErrorKind {
    err.as_database_error()
        .unwrap_or_else(|| panic!("not a database error: {err}"))
        .kind()
}

/// The extended SQLite result code, as sqlx reports it.
fn code_of(err: &sqlx::Error) -> String {
    err.as_database_error()
        .and_then(|err| err.code())
        .map(|code| code.into_owned())
        .unwrap_or_default()
}

async fn run(db: &Db, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(db.write())
        .await?;
    Ok(())
}

async fn must_run(db: &Db, sql: &str) {
    if let Err(err) = run(db, sql).await {
        panic!("rejected:\n  {sql}\n  {err}");
    }
}

async fn must_fail(db: &Db, sql: &str, expected: ErrorKind) {
    match run(db, sql).await {
        Ok(()) => panic!("accepted, but {expected:?} was expected:\n  {sql}"),
        Err(err) => assert_eq!(kind_of(&err), expected, "{sql}\n  {err}"),
    }
}

/// `SQLITE_CONSTRAINT_DATATYPE`: what a STRICT table answers to a value it cannot store.
async fn must_fail_datatype(db: &Db, sql: &str) {
    match run(db, sql).await {
        Ok(()) => panic!("a value of the wrong type was accepted:\n  {sql}"),
        Err(err) => assert_eq!(code_of(&err), "3091", "{sql}\n  {err}"),
    }
}

/// Proves that `sql` is accepted without keeping its row, so a later "rejected" case can only be
/// rejected for the one column it changes.
async fn must_be_valid(db: &Db, sql: &str) {
    let mut tx = db.begin_write().await.unwrap();
    let outcome = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(&mut *tx)
        .await;
    tx.rollback().await.unwrap();
    if let Err(err) = outcome {
        panic!("rejected:\n  {sql}\n  {err}");
    }
}

/// An `INSERT` of the valid row `base` in which some columns are replaced (or added) by
/// `overrides`. Values are SQL literals.
fn insert_sql(table: &str, base: &[(&str, &str)], overrides: &[(&str, &str)]) -> String {
    let mut row = base.to_vec();
    for &(column, value) in overrides {
        match row.iter_mut().find(|(name, _)| *name == column) {
            Some(slot) => slot.1 = value,
            None => row.push((column, value)),
        }
    }
    let columns: Vec<&str> = row.iter().map(|(column, _)| *column).collect();
    let values: Vec<&str> = row.iter().map(|(_, value)| *value).collect();
    format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        columns.join(", "),
        values.join(", ")
    )
}

fn volume_sql(overrides: &[(&str, &str)]) -> String {
    insert_sql(
        "volumes",
        &[("name", "'v'"), ("root", "'/v'"), ("created_at", "1")],
        overrides,
    )
}

fn dir_sql(volume: VolumeId, overrides: &[(&str, &str)]) -> String {
    let volume = volume.to_string();
    insert_sql(
        "dirs",
        &[
            ("volume_id", volume.as_str()),
            ("path", "'a'"),
            ("parent_path", "''"),
            ("name", "'a'"),
        ],
        overrides,
    )
}

fn file_sql(volume: VolumeId, overrides: &[(&str, &str)]) -> String {
    let volume = volume.to_string();
    insert_sql(
        "files",
        &[
            ("volume_id", volume.as_str()),
            ("dir_path", "'d'"),
            ("name", "'f.jpg'"),
            ("name_norm", "'f.jpg'"),
            ("stem_norm", "'f'"),
            ("kind", "'image'"),
            ("size", "1"),
            ("mtime", "1"),
        ],
        overrides,
    )
}

fn trash_sql(volume: VolumeId, overrides: &[(&str, &str)]) -> String {
    let volume = volume.to_string();
    insert_sql(
        "trash_items",
        &[
            ("volume_id", volume.as_str()),
            ("batch", "'b'"),
            ("orig_path", "'x'"),
            ("trash_path", "'.opm-trash/b/x'"),
            ("trashed_at", "1"),
        ],
        overrides,
    )
}

fn task_sql(overrides: &[(&str, &str)]) -> String {
    insert_sql(
        "tasks",
        &[
            ("kind", "'scan'"),
            ("title", "'Scan'"),
            ("lane", "'index'"),
            ("state", "'queued'"),
            ("created_at", "1"),
        ],
        overrides,
    )
}

fn journal_sql(overrides: &[(&str, &str)]) -> String {
    insert_sql(
        "fs_journal",
        &[
            ("op", "'move'"),
            ("state", "'pending'"),
            ("created_at", "1"),
        ],
        overrides,
    )
}

fn quoted(value: &str) -> String {
    format!("'{value}'")
}

async fn count(db: &Db, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(db.read())
        .await
        .unwrap()
}

async fn counts(db: &Db) -> BTreeMap<&'static str, i64> {
    let mut counts = BTreeMap::new();
    for table in CORE_TABLES {
        counts.insert(table, count(db, table).await);
    }
    counts
}

/// [`POPULATED`] with some tables changed.
fn expected(changes: &[(&'static str, i64)]) -> BTreeMap<&'static str, i64> {
    let mut counts: BTreeMap<_, _> = POPULATED.into_iter().collect();
    counts.extend(changes.iter().copied());
    counts
}

/// A small library that touches every table.
struct Library {
    photos: VolumeId,
    archive: VolumeId,
    /// `photos:2019/a.cr2`, tagged, with a camera, a sidecar and a selection.
    raw: FileId,
    /// `photos:2019/a.xmp`, the sidecar of `raw`.
    sidecar: FileId,
    /// `photos:2019/b.jpg`, with the camera.
    jpg: FileId,
    /// `archive:c.jpg`, tagged and selected.
    elsewhere: FileId,
    camera: i64,
    tag: i64,
    selection: i64,
}

async fn populate(db: &Db) -> Library {
    let photos = insert_volume(db, "photos").await;
    let archive = insert_volume(db, "archive").await;
    for (volume, path) in [(photos, ""), (photos, "2019"), (archive, "")] {
        insert_dir(db, volume, path).await;
    }
    let raw = insert_file(db, photos, "2019", "a.cr2").await;
    let sidecar = insert_file(db, photos, "2019", "a.xmp").await;
    let jpg = insert_file(db, photos, "2019", "b.jpg").await;
    let elsewhere = insert_file(db, archive, "", "c.jpg").await;

    let camera: i64 =
        sqlx::query_scalar("INSERT INTO cameras (name) VALUES ('Canon EOS R5') RETURNING id")
            .fetch_one(db.write())
            .await
            .unwrap();
    sqlx::query("UPDATE files SET camera_id = ? WHERE id IN (?, ?)")
        .bind(camera)
        .bind(raw)
        .bind(jpg)
        .execute(db.write())
        .await
        .unwrap();

    let tag: i64 = sqlx::query_scalar("INSERT INTO tags (name) VALUES ('holiday') RETURNING id")
        .fetch_one(db.write())
        .await
        .unwrap();
    for file in [raw, elsewhere] {
        sqlx::query("INSERT INTO file_tags (file_id, tag_id) VALUES (?, ?)")
            .bind(file)
            .bind(tag)
            .execute(db.write())
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO file_sidecars (file_id, sidecar_id, kind) VALUES (?, ?, 'exact')")
        .bind(raw)
        .bind(sidecar)
        .execute(db.write())
        .await
        .unwrap();

    let selection: i64 = sqlx::query_scalar(
        "INSERT INTO selections (created_at, expires_at, item_count) VALUES (1, 2, 2) RETURNING id",
    )
    .fetch_one(db.write())
    .await
    .unwrap();
    for file in [raw, elsewhere] {
        sqlx::query("INSERT INTO selection_items (selection_id, file_id) VALUES (?, ?)")
            .bind(selection)
            .bind(file)
            .execute(db.write())
            .await
            .unwrap();
    }

    must_run(
        db,
        &trash_sql(
            photos,
            &[("trash_path", "'.opm-trash/b1/a.cr2'"), ("task_id", "7")],
        ),
    )
    .await;
    must_run(
        db,
        &trash_sql(archive, &[("trash_path", "'.opm-trash/b2/c.jpg'")]),
    )
    .await;
    must_run(
        db,
        &journal_sql(&[
            ("state", "'done'"),
            ("src_volume_id", &photos.to_string()),
            ("src_path", "'2019/a.cr2'"),
        ]),
    )
    .await;
    must_run(db, &task_sql(&[("state", "'succeeded'")])).await;
    must_run(
        db,
        "INSERT INTO settings (key, value, updated_at) VALUES ('app', '{}', 1)",
    )
    .await;

    Library {
        photos,
        archive,
        raw,
        sidecar,
        jpg,
        elsewhere,
        camera,
        tag,
        selection,
    }
}

async fn delete(db: &Db, table: &str, id: i64) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM {table} WHERE id = ?"
    )))
    .bind(id)
    .execute(db.write())
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------------------------
// tables

#[tokio::test]
async fn every_core_table_exists_and_is_strict() {
    let db = Db::open_temp().await.unwrap();
    for table in CORE_TABLES {
        let strict: Option<bool> = sqlx::query_scalar(
            "SELECT strict FROM pragma_table_list WHERE schema = 'main' AND name = ?",
        )
        .bind(table)
        .fetch_optional(db.read())
        .await
        .unwrap();
        assert_eq!(strict, Some(true), "{table} must exist and be STRICT");
    }
}

#[tokio::test]
async fn a_populated_library_has_no_dangling_references() {
    let db = Db::open_temp().await.unwrap();
    populate(&db).await;
    assert_eq!(counts(&db).await, expected(&[]));

    let dangling: Vec<(String, i64, String, i64)> =
        sqlx::query_as("SELECT * FROM pragma_foreign_key_check")
            .fetch_all(db.write())
            .await
            .unwrap();
    assert!(dangling.is_empty(), "{dangling:?}");
    let broken: Vec<String> = sqlx::query_scalar("SELECT * FROM pragma_integrity_check")
        .fetch_all(db.write())
        .await
        .unwrap();
    assert_eq!(broken, ["ok"]);
}

// ---------------------------------------------------------------------------------------------
// foreign keys

#[tokio::test]
async fn deleting_a_volume_removes_everything_it_owns_and_nothing_else() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;

    delete(&db, "volumes", lib.photos).await;

    assert_eq!(
        counts(&db).await,
        expected(&[
            ("volumes", 1),
            ("dirs", 1),
            ("files", 1),
            ("file_tags", 1),
            ("file_sidecars", 0),
            ("selection_items", 1),
            ("trash_items", 1),
        ])
    );
    let left: Vec<i64> = sqlx::query_scalar("SELECT id FROM files")
        .fetch_all(db.read())
        .await
        .unwrap();
    assert_eq!(left, [lib.elsewhere]);
}

#[tokio::test]
async fn deleting_a_file_removes_its_links() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;

    delete(&db, "files", lib.raw).await;

    assert_eq!(
        counts(&db).await,
        expected(&[
            ("files", 3),
            ("file_tags", 1),
            ("file_sidecars", 0),
            ("selection_items", 1),
        ])
    );
}

#[tokio::test]
async fn deleting_a_sidecar_file_removes_only_its_link() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;

    delete(&db, "files", lib.sidecar).await;

    assert_eq!(
        counts(&db).await,
        expected(&[("files", 3), ("file_sidecars", 0)])
    );
}

#[tokio::test]
async fn deleting_a_camera_clears_camera_id_and_keeps_the_files() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;
    let with_camera = |db: &Db| {
        let db = db.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM files WHERE camera_id IS NOT NULL")
                .fetch_one(db.read())
                .await
                .unwrap()
        }
    };
    assert_eq!(with_camera(&db).await, 2);

    delete(&db, "cameras", lib.camera).await;

    assert_eq!(with_camera(&db).await, 0);
    assert_eq!(
        counts(&db).await,
        expected(&[("cameras", 0)]),
        "no file is deleted with its camera"
    );
}

#[tokio::test]
async fn deleting_a_tag_or_a_selection_removes_the_links_and_keeps_the_files() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;

    delete(&db, "tags", lib.tag).await;
    assert_eq!(
        counts(&db).await,
        expected(&[("tags", 0), ("file_tags", 0)])
    );

    delete(&db, "selections", lib.selection).await;
    assert_eq!(
        counts(&db).await,
        expected(&[
            ("tags", 0),
            ("file_tags", 0),
            ("selections", 0),
            ("selection_items", 0)
        ])
    );
}

#[tokio::test]
async fn the_journal_and_task_references_are_deliberately_not_foreign_keys() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;

    // The crash-recovery journal outlives the volume it mentions ...
    delete(&db, "volumes", lib.photos).await;
    let journaled: i64 = sqlx::query_scalar("SELECT src_volume_id FROM fs_journal")
        .fetch_one(db.read())
        .await
        .unwrap();
    assert_eq!(journaled, lib.photos);

    // ... and trash items (and the journal) may mention a task that was purged from the history.
    must_run(
        &db,
        &trash_sql(
            lib.archive,
            &[("trash_path", "'.opm-trash/b3/x'"), ("task_id", "123456")],
        ),
    )
    .await;
    must_run(&db, &journal_sql(&[("task_id", "123456")])).await;
}

#[tokio::test]
async fn rows_cannot_reference_parents_that_do_not_exist() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;
    let cases = [
        dir_sql(999, &[]),
        file_sql(999, &[]),
        file_sql(lib.photos, &[("name", "'x.jpg'"), ("camera_id", "999")]),
        trash_sql(999, &[]),
        format!(
            "INSERT INTO file_tags (file_id, tag_id) VALUES (999, {})",
            lib.tag
        ),
        format!(
            "INSERT INTO file_tags (file_id, tag_id) VALUES ({}, 999)",
            lib.jpg
        ),
        format!(
            "INSERT INTO file_sidecars (file_id, sidecar_id, kind) VALUES ({}, 999, 'exact')",
            lib.jpg
        ),
        format!(
            "INSERT INTO file_sidecars (file_id, sidecar_id, kind) VALUES (999, {}, 'exact')",
            lib.jpg
        ),
        format!(
            "INSERT INTO selection_items (selection_id, file_id) VALUES (999, {})",
            lib.jpg
        ),
        format!(
            "INSERT INTO selection_items (selection_id, file_id) VALUES ({}, 999)",
            lib.selection
        ),
        format!("UPDATE files SET camera_id = 999 WHERE id = {}", lib.jpg),
        format!("UPDATE files SET volume_id = 999 WHERE id = {}", lib.jpg),
    ];
    for sql in cases {
        must_fail(&db, &sql, ErrorKind::ForeignKeyViolation).await;
    }
    assert_eq!(counts(&db).await, expected(&[]), "nothing was written");
}

// ---------------------------------------------------------------------------------------------
// unique indexes

#[tokio::test]
async fn unique_indexes_reject_duplicates() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;
    let cases = [
        volume_sql(&[("name", "'photos'")]),
        dir_sql(lib.photos, &[("path", "'2019'"), ("name", "'2019'")]),
        file_sql(lib.photos, &[("dir_path", "'2019'"), ("name", "'a.cr2'")]),
        trash_sql(lib.photos, &[("trash_path", "'.opm-trash/b1/a.cr2'")]),
        format!(
            "INSERT INTO file_tags (file_id, tag_id) VALUES ({}, {})",
            lib.raw, lib.tag
        ),
        format!(
            "INSERT INTO file_sidecars (file_id, sidecar_id, kind) VALUES ({}, {}, 'stem')",
            lib.raw, lib.sidecar
        ),
        format!(
            "INSERT INTO selection_items (selection_id, file_id) VALUES ({}, {})",
            lib.selection, lib.raw
        ),
        "INSERT INTO settings (key, value, updated_at) VALUES ('app', '{}', 2)".to_owned(),
    ];
    for sql in &cases {
        must_fail(&db, sql, ErrorKind::UniqueViolation).await;
    }
    assert_eq!(counts(&db).await, expected(&[]), "nothing was written");
}

#[tokio::test]
async fn uniqueness_is_scoped_to_the_volume_and_the_exact_name() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;
    // The same directory and file names on another volume are different rows.
    must_run(
        &db,
        &dir_sql(lib.archive, &[("path", "'2019'"), ("name", "'2019'")]),
    )
    .await;
    must_run(
        &db,
        &file_sql(lib.archive, &[("dir_path", "'2019'"), ("name", "'a.cr2'")]),
    )
    .await;
    // Names differing only by case are different on the file systems the NAS uses.
    must_run(
        &db,
        &file_sql(lib.photos, &[("dir_path", "'2019'"), ("name", "'A.CR2'")]),
    )
    .await;
    must_run(&db, &volume_sql(&[("name", "'Photos'")])).await;
    // A trash path is unique per volume.
    must_run(
        &db,
        &trash_sql(lib.archive, &[("trash_path", "'.opm-trash/b1/a.cr2'")]),
    )
    .await;
}

#[tokio::test]
async fn camera_and_tag_names_are_unique_ignoring_ascii_case() {
    let db = Db::open_temp().await.unwrap();
    for table in ["cameras", "tags"] {
        must_run(
            &db,
            &format!("INSERT INTO {table} (name) VALUES ('Canon EOS R5')"),
        )
        .await;
        must_fail(
            &db,
            &format!("INSERT INTO {table} (name) VALUES ('CANON eos r5')"),
            ErrorKind::UniqueViolation,
        )
        .await;
        let found: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT id FROM {table} WHERE name = 'canon EOS R5'"
        )))
        .fetch_optional(db.read())
        .await
        .unwrap();
        assert!(found.is_some(), "{table}: lookups ignore case as well");

        // NOCASE folds ASCII only: these two are different names.
        must_run(&db, &format!("INSERT INTO {table} (name) VALUES ('É')")).await;
        must_run(&db, &format!("INSERT INTO {table} (name) VALUES ('é')")).await;
    }
}

// ---------------------------------------------------------------------------------------------
// CHECK, NOT NULL and STRICT

#[tokio::test]
async fn volume_flags_are_booleans() {
    let db = Db::open_temp().await.unwrap();
    must_be_valid(&db, &volume_sql(&[])).await;
    must_be_valid(&db, &volume_sql(&[("writable", "1"), ("online", "0")])).await;
    for overrides in [[("writable", "2")], [("writable", "-1")], [("online", "2")]] {
        must_fail(&db, &volume_sql(&overrides), ErrorKind::CheckViolation).await;
    }
}

#[tokio::test]
async fn dirs_must_be_consistent_relative_paths() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;

    let valid: [&[(&str, &str)]; 3] = [
        &[],
        &[("path", "'a/b'"), ("parent_path", "'a'"), ("name", "'b'")],
        &[("path", "''"), ("parent_path", "NULL"), ("name", "''")],
    ];
    for overrides in valid {
        must_be_valid(&db, &dir_sql(volume, overrides)).await;
    }

    let invalid: [&[(&str, &str)]; 9] = [
        &[("path", "'/a'")],
        &[("path", "'a/'")],
        &[("path", "''"), ("parent_path", "NULL"), ("name", "'x'")],
        &[("path", "''"), ("parent_path", "''"), ("name", "''")],
        &[("parent_path", "NULL")],
        &[("name", "''")],
        &[("path", "'a/b'"), ("parent_path", "'a'"), ("name", "'b/c'")],
        &[("path", "'a/b'")],
        &[("parent_path", "'x'")],
    ];
    for overrides in invalid {
        must_fail(&db, &dir_sql(volume, overrides), ErrorKind::CheckViolation).await;
    }
}

#[tokio::test]
async fn files_accept_only_their_documented_values() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;

    let valid: [&[(&str, &str)]; 17] = [
        &[],
        &[("kind", "'video'")],
        &[("kind", "'audio'")],
        &[("kind", "'sidecar'")],
        &[("kind", "'other'")],
        &[("capture_src", "'exif'")],
        &[("capture_src", "'fs'")],
        &[("capture_src", "NULL")],
        &[("rating", "0")],
        &[("rating", "5")],
        &[("flag", "-1")],
        &[("flag", "1")],
        &[("hash", "zeroblob(20)")],
        &[("hash", "NULL")],
        &[("dir_path", "''")],
        &[("dir_path", "'a/b'")],
        &[("name", "'été 1.jpg'")],
    ];
    for overrides in valid {
        must_be_valid(&db, &file_sql(volume, overrides)).await;
    }

    let invalid: [&[(&str, &str)]; 16] = [
        &[("kind", "'movie'")],
        &[("kind", "''")],
        &[("capture_src", "'gps'")],
        &[("rating", "6")],
        &[("rating", "-1")],
        &[("flag", "2")],
        &[("flag", "-2")],
        &[("hash", "zeroblob(0)")],
        &[("hash", "zeroblob(19)")],
        &[("hash", "zeroblob(21)")],
        &[("name", "''")],
        &[("name", "'a/b'")],
        &[("dir_path", "'/a'")],
        &[("dir_path", "'a/'")],
        &[("dir_path", "'/'")],
        &[("capture_src", "''")],
    ];
    for overrides in invalid {
        must_fail(&db, &file_sql(volume, overrides), ErrorKind::CheckViolation).await;
    }
}

#[tokio::test]
async fn the_hash_is_exactly_twenty_bytes() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    let file = insert_file(&db, volume, "d", "f.jpg").await;

    for (length, accepted) in [
        (0, false),
        (19, false),
        (20, true),
        (21, false),
        (32, false),
    ] {
        let outcome = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE files SET hash = zeroblob({length}) WHERE id = {file}"
        )))
        .execute(db.write())
        .await;
        assert_eq!(outcome.is_ok(), accepted, "{length} bytes: {outcome:?}");
        if let Err(err) = outcome {
            assert_eq!(kind_of(&err), ErrorKind::CheckViolation);
        }
    }
    let stored: Vec<u8> = sqlx::query_scalar("SELECT hash FROM files WHERE id = ?")
        .bind(file)
        .fetch_one(db.read())
        .await
        .unwrap();
    assert_eq!(stored.len(), 20);
}

#[tokio::test]
async fn sidecar_links_have_a_kind_and_never_point_at_themselves() {
    let db = Db::open_temp().await.unwrap();
    let lib = populate(&db).await;
    let link = |file: FileId, sidecar: FileId, kind: &str| {
        format!(
            "INSERT INTO file_sidecars (file_id, sidecar_id, kind) VALUES ({file}, {sidecar}, '{kind}')"
        )
    };
    must_be_valid(&db, &link(lib.jpg, lib.sidecar, "stem")).await;
    must_fail(
        &db,
        &link(lib.jpg, lib.sidecar, "other"),
        ErrorKind::CheckViolation,
    )
    .await;
    must_fail(
        &db,
        &link(lib.jpg, lib.jpg, "exact"),
        ErrorKind::CheckViolation,
    )
    .await;
}

#[tokio::test]
async fn tasks_have_a_known_state_and_valid_json() {
    let db = Db::open_temp().await.unwrap();
    must_be_valid(&db, &task_sql(&[])).await;
    for state in [
        "queued",
        "running",
        "succeeded",
        "failed",
        "cancelled",
        "interrupted",
    ] {
        must_be_valid(&db, &task_sql(&[("state", &quoted(state))])).await;
    }
    must_be_valid(
        &db,
        &task_sql(&[
            ("params_json", "'{\"a\":[1,2]}'"),
            ("result_json", "'null'"),
        ]),
    )
    .await;

    for overrides in [
        [("state", "'paused'")],
        [("state", "''")],
        [("params_json", "'{broken'")],
        [("result_json", "'[1,'")],
    ] {
        must_fail(&db, &task_sql(&overrides), ErrorKind::CheckViolation).await;
    }
}

#[tokio::test]
async fn trash_items_live_under_the_trash_directory() {
    // The CHECK of 0001_core.sql spells the directory out: a rename of the constant needs a migration.
    assert_eq!(
        TRASH_DIR, ".opm-trash",
        "the trash_path CHECK in 0001_core.sql hard-codes the trash directory name"
    );

    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    must_be_valid(
        &db,
        &trash_sql(
            volume,
            &[("trash_path", &quoted(&format!("{TRASH_DIR}/x")))],
        ),
    )
    .await;
    must_be_valid(&db, &trash_sql(volume, &[("is_dir", "1")])).await;
    must_be_valid(
        &db,
        &trash_sql(volume, &[("meta_json", "'{\"reason\":\"dup\"}'")]),
    )
    .await;

    let invalid = [
        TRASH_DIR.to_owned(),
        format!("{TRASH_DIR}2/x"),
        format!("x/{TRASH_DIR}/y"),
        format!("/{TRASH_DIR}/y"),
        String::new(),
    ];
    for trash_path in invalid {
        must_fail(
            &db,
            &trash_sql(volume, &[("trash_path", &quoted(&trash_path))]),
            ErrorKind::CheckViolation,
        )
        .await;
    }
    for overrides in [[("is_dir", "2")], [("meta_json", "'not json'")]] {
        must_fail(
            &db,
            &trash_sql(volume, &overrides),
            ErrorKind::CheckViolation,
        )
        .await;
    }
}

#[tokio::test]
async fn journal_entries_have_a_known_operation_and_state() {
    let db = Db::open_temp().await.unwrap();
    for op in [
        "move", "copy", "trash", "restore", "rmdir", "mkdir", "delete",
    ] {
        must_be_valid(&db, &journal_sql(&[("op", &quoted(op))])).await;
    }
    for state in ["pending", "done", "failed", "abandoned"] {
        must_be_valid(&db, &journal_sql(&[("state", &quoted(state))])).await;
    }
    for overrides in [[("op", "'teleport'")], [("state", "'running'")]] {
        must_fail(&db, &journal_sql(&overrides), ErrorKind::CheckViolation).await;
    }
}

#[tokio::test]
async fn settings_values_are_json_documents() {
    let db = Db::open_temp().await.unwrap();
    for value in ["{}", "[]", "null", "42", r#"{"a":{"b":[1]}}"#] {
        must_be_valid(
            &db,
            &format!("INSERT INTO settings (key, value, updated_at) VALUES ('k', '{value}', 0)"),
        )
        .await;
    }
    for value in ["", "not json", "{\"a\":", "{\"a\" 1}", "[1, 2"] {
        must_fail(
            &db,
            &format!("INSERT INTO settings (key, value, updated_at) VALUES ('k', '{value}', 0)"),
            ErrorKind::CheckViolation,
        )
        .await;
    }
}

#[tokio::test]
async fn required_columns_refuse_null() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    let cases = [
        volume_sql(&[("name", "NULL")]),
        volume_sql(&[("root", "NULL")]),
        volume_sql(&[("created_at", "NULL")]),
        dir_sql(volume, &[("name", "NULL")]),
        file_sql(volume, &[("name_norm", "NULL")]),
        file_sql(volume, &[("stem_norm", "NULL")]),
        file_sql(volume, &[("kind", "NULL")]),
        file_sql(volume, &[("size", "NULL")]),
        file_sql(volume, &[("mtime", "NULL")]),
        file_sql(volume, &[("rating", "NULL")]),
        file_sql(volume, &[("meta_state", "NULL")]),
        task_sql(&[("state", "NULL")]),
        task_sql(&[("params_json", "NULL")]),
        trash_sql(volume, &[("trash_path", "NULL")]),
        journal_sql(&[("op", "NULL")]),
        "INSERT INTO selections (created_at) VALUES (1)".to_owned(),
        "INSERT INTO settings (key, value, updated_at) VALUES ('k', NULL, 0)".to_owned(),
    ];
    for sql in &cases {
        must_fail(&db, sql, ErrorKind::NotNullViolation).await;
    }
}

#[tokio::test]
async fn strict_tables_refuse_values_of_the_wrong_type() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    let cases = [
        volume_sql(&[("created_at", "'yesterday'")]),
        volume_sql(&[("writable", "'maybe'")]),
        file_sql(volume, &[("size", "'big'")]),
        file_sql(volume, &[("mtime", "1.5")]),
        file_sql(volume, &[("flag", "0.5")]),
        file_sql(volume, &[("width", "x'00'")]),
        file_sql(volume, &[("name", "x'00'")]),
        file_sql(volume, &[("hash", "'twenty characters....'")]),
        task_sql(&[("items_done", "'many'")]),
        "INSERT INTO settings (key, value, updated_at) VALUES ('k', '{}', 'later')".to_owned(),
    ];
    for sql in &cases {
        must_fail_datatype(&db, sql).await;
    }
}

// ---------------------------------------------------------------------------------------------
// the enums that are stored as text

/// The quoted values of `<column> IN ('a', 'b', ...)` in the `CREATE TABLE` statement of `table`.
async fn in_list(db: &Db, table: &str, column: &str) -> Vec<String> {
    let ddl: String = sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE name = ?")
        .bind(table)
        .fetch_one(db.read())
        .await
        .unwrap();
    let marker = format!("{column} IN (");
    let start = ddl
        .find(&marker)
        .unwrap_or_else(|| panic!("no `{marker}` in {ddl}"))
        + marker.len();
    let end = start + ddl[start..].find(')').expect("an unterminated IN list");
    ddl[start..end]
        .split(',')
        .map(|value| value.trim().trim_matches('\'').to_owned())
        .collect()
}

#[tokio::test]
async fn the_kind_check_and_the_file_kind_enum_agree() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    let kinds = [
        FileKind::Image,
        FileKind::Video,
        FileKind::Audio,
        FileKind::Sidecar,
        FileKind::Other,
    ];
    // Exhaustive on purpose: a new variant stops compiling here, next to the list above.
    for kind in kinds {
        match kind {
            FileKind::Image
            | FileKind::Video
            | FileKind::Audio
            | FileKind::Sidecar
            | FileKind::Other => {}
        }
    }

    for kind in kinds {
        let value = file_kind_to_db(kind);
        must_be_valid(&db, &file_sql(volume, &[("kind", &quoted(value))])).await;
    }
    let allowed = in_list(&db, "files", "kind").await;
    assert_eq!(allowed.len(), kinds.len(), "{allowed:?}");
    for value in &allowed {
        let kind = file_kind_from_db(value)
            .unwrap_or_else(|err| panic!("the CHECK allows a kind the code does not know: {err}"));
        assert_eq!(file_kind_to_db(kind), value);
    }
}

#[tokio::test]
async fn the_capture_src_check_and_the_capture_source_enum_agree() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    let sources = [
        CaptureSource::Exif,
        CaptureSource::Xmp,
        CaptureSource::Video,
        CaptureSource::Fs,
    ];
    // Exhaustive on purpose, see the kind test.
    for source in sources {
        match source {
            CaptureSource::Exif | CaptureSource::Xmp | CaptureSource::Video | CaptureSource::Fs => {
            }
        }
    }

    for source in sources {
        let value = capture_source_to_db(source);
        must_be_valid(&db, &file_sql(volume, &[("capture_src", &quoted(value))])).await;
    }
    let allowed = in_list(&db, "files", "capture_src").await;
    assert_eq!(allowed.len(), sources.len(), "{allowed:?}");
    for value in &allowed {
        let source = capture_source_from_db(value).unwrap_or_else(|err| {
            panic!("the CHECK allows a capture source the code does not know: {err}")
        });
        assert_eq!(capture_source_to_db(source), value);
    }
}

#[tokio::test]
async fn every_flag_value_is_accepted_and_maps_back() {
    let db = Db::open_temp().await.unwrap();
    let volume = insert_volume(&db, "photos").await;
    let flags = [None, Some(FlagFilter::Selected), Some(FlagFilter::Rejected)];
    // Exhaustive on purpose, see the kind test.
    for flag in flags.into_iter().flatten() {
        match flag {
            FlagFilter::Selected | FlagFilter::Rejected => {}
        }
    }

    for flag in flags {
        let value = flag_to_db(flag);
        must_be_valid(&db, &file_sql(volume, &[("flag", &value.to_string())])).await;
        assert_eq!(flag_from_db(value).unwrap(), flag);
    }
    let allowed = in_list(&db, "files", "flag").await;
    assert_eq!(allowed.len(), flags.len(), "{allowed:?}");
}

// ---------------------------------------------------------------------------------------------
// query plans

/// The plan of `sql` (bind parameters are filled with `1`), one line per step.
async fn plan(db: &Db, sql: &str) -> String {
    let mut query = sqlx::query_as::<_, (i64, i64, i64, String)>(sqlx::AssertSqlSafe(format!(
        "EXPLAIN QUERY PLAN {sql}"
    )));
    for _ in 0..sql.matches('?').count() {
        query = query.bind(1_i64);
    }
    // The writer connection: it is the one that sees the statistics of an `ANALYZE` it ran.
    let rows = query.fetch_all(db.write()).await.unwrap();
    rows.into_iter()
        .map(|(_, _, _, detail)| detail)
        .collect::<Vec<_>>()
        .join("\n")
}

async fn assert_plans(db: &Db, cases: &[(&str, &str)]) {
    for (sql, wanted) in cases {
        let plan = plan(db, sql).await;
        assert!(
            plan.contains(wanted),
            "expected `{wanted}` in the plan of\n  {sql}\nbut it is\n  {plan}"
        );
        assert!(
            !plan.contains("SCAN files"),
            "full scan of files for {sql}: {plan}"
        );
    }
}

/// The lookups the other modules are built on, with the index step each must plan as.
const INDEXED_LOOKUPS: [(&str, &str); 14] = [
    (
        "SELECT id FROM files WHERE volume_id = ? AND dir_path = ? AND name = ?",
        "INDEX files_volume_dir_name (volume_id=? AND dir_path=? AND name=?)",
    ),
    (
        "SELECT id FROM files WHERE volume_id = ? AND dir_path = ?",
        "INDEX files_volume_dir_name (volume_id=? AND dir_path=?)",
    ),
    (
        "SELECT id FROM files WHERE hash = ?",
        "INDEX files_hash (hash=?)",
    ),
    (
        "SELECT id FROM files WHERE name_norm = ?",
        "INDEX files_name_capture (name_norm=?)",
    ),
    (
        "SELECT id FROM files WHERE name_norm = ? AND capture_ts = ?",
        "INDEX files_name_capture (name_norm=? AND capture_ts=?)",
    ),
    (
        "SELECT id FROM files WHERE name_norm = ? AND capture_ts >= ? AND capture_ts < ?",
        "INDEX files_name_capture (name_norm=? AND capture_ts>? AND capture_ts<?)",
    ),
    (
        "SELECT id FROM files WHERE stem_norm = ?",
        "INDEX files_stem (stem_norm=?)",
    ),
    (
        "SELECT id FROM files WHERE volume_id = ? AND meta_state = ?",
        "INDEX files_meta_state (volume_id=? AND meta_state=?)",
    ),
    (
        "SELECT id FROM files WHERE volume_id = ? AND hash_state = ?",
        "INDEX files_hash_state (volume_id=? AND hash_state=?)",
    ),
    (
        "SELECT id FROM files WHERE volume_id = ? AND thumb_state = ?",
        "INDEX files_thumb_state (volume_id=? AND thumb_state=?)",
    ),
    (
        "SELECT id FROM files WHERE volume_id = ? AND seen_gen < ?",
        "INDEX files_seen_gen (volume_id=? AND seen_gen<?)",
    ),
    (
        "SELECT id FROM files WHERE camera_id = ?",
        "INDEX files_camera (camera_id=?)",
    ),
    (
        "SELECT id FROM files WHERE size = ?",
        "INDEX files_size (size=?)",
    ),
    (
        "SELECT id FROM files WHERE rating = ?",
        "INDEX files_rating (rating=?)",
    ),
];

const OTHER_INDEXED_LOOKUPS: [(&str, &str); 11] = [
    (
        "SELECT id FROM dirs WHERE volume_id = ? AND path = ?",
        "INDEX dirs_volume_path (volume_id=? AND path=?)",
    ),
    (
        "SELECT id FROM dirs WHERE volume_id = ? AND parent_path = ?",
        "INDEX dirs_parent (volume_id=? AND parent_path=?)",
    ),
    (
        "SELECT file_id FROM file_tags WHERE tag_id = ?",
        "INDEX file_tags_tag (tag_id=?)",
    ),
    (
        "SELECT file_id FROM file_sidecars WHERE sidecar_id = ?",
        "INDEX file_sidecars_sidecar (sidecar_id=?)",
    ),
    (
        "SELECT selection_id FROM selection_items WHERE file_id = ?",
        "INDEX selection_items_file (file_id=?)",
    ),
    (
        "SELECT id FROM selections WHERE expires_at < ?",
        "INDEX selections_expires (expires_at<?)",
    ),
    (
        "SELECT id FROM tasks WHERE state = ?",
        "INDEX tasks_state (state=?)",
    ),
    (
        "SELECT id FROM tasks ORDER BY created_at DESC LIMIT ?",
        "INDEX tasks_created",
    ),
    (
        "SELECT id FROM trash_items WHERE volume_id = ? AND trash_path = ?",
        "INDEX trash_items_volume_trash_path (volume_id=? AND trash_path=?)",
    ),
    (
        "SELECT id FROM fs_journal WHERE state = ?",
        "INDEX fs_journal_state (state=?)",
    ),
    (
        "SELECT id FROM files WHERE volume_id = ? AND dir_path >= ? AND dir_path < ?",
        "INDEX files_volume_dir_name (volume_id=? AND dir_path>? AND dir_path<?)",
    ),
];

#[tokio::test]
async fn lookups_use_their_indexes_on_a_new_database() {
    let db = Db::open_temp().await.unwrap();
    assert_plans(&db, &INDEXED_LOOKUPS).await;
    assert_plans(&db, &OTHER_INDEXED_LOOKUPS).await;
}

#[tokio::test]
async fn walking_a_volume_in_path_order_needs_no_sort() {
    let db = Db::open_temp().await.unwrap();
    let sql = "SELECT id FROM files WHERE volume_id = ? ORDER BY dir_path, name";
    let plan = plan(&db, sql).await;
    assert!(
        plan.contains("INDEX files_volume_dir_name (volume_id=?)"),
        "{plan}"
    );
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

#[tokio::test]
async fn the_important_lookups_keep_their_plans_when_the_tables_have_statistics() {
    let db = Db::open_temp().await.unwrap();
    let photos = insert_volume(&db, "photos").await;
    let archive = insert_volume(&db, "archive").await;
    must_run(
        &db,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 59) \
             INSERT INTO dirs (volume_id, path, parent_path, name) \
             SELECT {photos}, 'd' || i, '', 'd' || i FROM n"
        ),
    )
    .await;
    must_run(
        &db,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
             INSERT INTO files (volume_id, dir_path, name, name_norm, stem_norm, ext, kind, size, \
                                mtime, capture_ts, rating, flag, hash, meta_state, hash_state, \
                                thumb_state, seen_gen) \
             SELECT {photos}, 'd' || (i % 60), 'img' || i || '.jpg', 'img' || i || '.jpg', \
                    'img' || i, 'jpg', 'image', 1000 + i, 1, 1500000000 + i * 3600, i % 6, \
                    (i % 3) - 1, randomblob(20), i % 4, i % 3, i % 2, i % 5 \
             FROM n"
        ),
    )
    .await;
    must_run(&db, &file_sql(archive, &[])).await;
    must_run(&db, "ANALYZE").await;
    let analysed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_stat1")
        .fetch_one(db.write())
        .await
        .unwrap();
    assert!(analysed > 0, "ANALYZE produced no statistics");

    let important = [
        INDEXED_LOOKUPS[0],
        INDEXED_LOOKUPS[1],
        INDEXED_LOOKUPS[2],
        INDEXED_LOOKUPS[3],
        INDEXED_LOOKUPS[4],
        INDEXED_LOOKUPS[5],
        OTHER_INDEXED_LOOKUPS[0],
        OTHER_INDEXED_LOOKUPS[1],
        OTHER_INDEXED_LOOKUPS[10],
    ];
    assert_plans(&db, &important).await;
}

// ---------------------------------------------------------------------------------------------
// index policy

/// Deleting a parent row looks up its children by the foreign key columns, so each foreign key
/// needs an index that starts with them. Without it a delete scans the whole child table.
#[tokio::test]
async fn every_foreign_key_is_backed_by_an_index() {
    let db = Db::open_temp().await.unwrap();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )
    .fetch_all(db.read())
    .await
    .unwrap();
    assert!(tables.len() >= CORE_TABLES.len());

    let mut unindexed = Vec::new();
    for table in &tables {
        let columns: Vec<(i64, String)> =
            sqlx::query_as(r#"SELECT id, "from" FROM pragma_foreign_key_list(?) ORDER BY id, seq"#)
                .bind(table)
                .fetch_all(db.read())
                .await
                .unwrap();
        let mut keys: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        for (id, column) in columns {
            keys.entry(id).or_default().push(column);
        }
        if keys.is_empty() {
            continue;
        }

        let index_names: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_index_list(?) WHERE partial = 0")
                .bind(table)
                .fetch_all(db.read())
                .await
                .unwrap();
        let mut indexed: Vec<Vec<String>> = Vec::new();
        for index in index_names {
            let columns =
                sqlx::query_scalar("SELECT name FROM pragma_index_info(?) ORDER BY seqno")
                    .bind(index)
                    .fetch_all(db.read())
                    .await
                    .unwrap();
            indexed.push(columns);
        }
        // `id INTEGER PRIMARY KEY` of an ordinary table is the rowid: a lookup by it needs no index.
        let rowid_alias: Option<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info(?) WHERE pk = 1 AND type = 'INTEGER' \
             AND (SELECT COUNT(*) FROM pragma_table_info(?) WHERE pk > 0) = 1 \
             AND (SELECT wr FROM pragma_table_list WHERE name = ? AND schema = 'main') = 0",
        )
        .bind(table)
        .bind(table)
        .bind(table)
        .fetch_optional(db.read())
        .await
        .unwrap();

        for key in keys.into_values() {
            let by_index = indexed.iter().any(|columns| {
                columns.len() >= key.len()
                    && columns[..key.len()]
                        .iter()
                        .all(|column| key.contains(column))
            });
            let by_rowid = key.len() == 1 && rowid_alias.as_deref() == Some(key[0].as_str());
            if !by_index && !by_rowid {
                unindexed.push(format!(
                    "{table}({}): add `CREATE INDEX {table}_{} ON {table} ({})` to the migration \
                     that creates the foreign key",
                    key.join(", "),
                    key.join("_"),
                    key.join(", ")
                ));
            }
        }
    }
    assert!(
        unindexed.is_empty(),
        "foreign keys without an index (a parent delete would scan the child table):\n  {}",
        unindexed.join("\n  ")
    );
}
