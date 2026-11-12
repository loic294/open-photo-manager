//! Explicit gate against a disposable database initialized by the pinned Immich
//! server, never a hand-written approximation of its schema.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

async fn asset(
    pool: &PgPool,
    owner: &str,
    library: &str,
    path: &str,
    checksum: &[u8],
    algorithm: &str,
) -> String {
    sqlx::query_scalar(
        r#"INSERT INTO public.asset ("ownerId", "libraryId", "isExternal", type,
        "originalPath", "originalFileName", checksum, "checksumAlgorithm",
        "fileCreatedAt", "fileModifiedAt", "localDateTime")
        VALUES ($1::uuid, $2::uuid, true, 'IMAGE', $3, 'photo.jpg', $4,
        $5::public.asset_checksum_algorithm_enum, now(), now(), now()) RETURNING id::text"#,
    )
    .bind(owner)
    .bind(library)
    .bind(path)
    .bind(checksum)
    .bind(algorithm)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "Explicit real-v3.3.3 gate: requires OPM_IMMICH_TEST_DATABASE_URL_FILE and a marked disposable database"]
async fn real_v333_postgres_gate() {
    let path = std::env::var_os("OPM_IMMICH_TEST_DATABASE_URL_FILE").expect(
        "GATE FAILED: OPM_IMMICH_TEST_DATABASE_URL_FILE is required; no PostgreSQL checks ran",
    );
    let database = PostgresMoves::new(Some(PathBuf::from(path)));
    let pool = database
        .pool()
        .await
        .expect("GATE FAILED: disposable PostgreSQL unavailable or TLS invalid");
    let (name, marker): (String, Option<String>) = sqlx::query_as(
        "SELECT current_database(), shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = current_database()")
        .fetch_one(&pool).await.unwrap();
    assert!(
        name.starts_with("opm_immich_test_"),
        "GATE FAILED: refusing an unmarked database"
    );
    assert_eq!(
        marker.as_deref(),
        Some("opm-immich-v3.3.3-disposable"),
        "GATE FAILED: disposable marker absent"
    );
    database
        .probe("v3.3.3")
        .await
        .expect("GATE FAILED: exact upstream migrations/catalog/triggers do not match");
    assert_eq!(
        database.probe("v3.4.0").await,
        Err(DbMoveError::Incompatible)
    );

    let group: String =
        sqlx::query_scalar("INSERT INTO public.cluster_group DEFAULT VALUES RETURNING id::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    let owner: String = sqlx::query_scalar(
        r#"INSERT INTO public."user" (email, "clusterGroupId") VALUES ($1, $2::uuid) RETURNING id::text"#)
        .bind(format!("{group}@example.test")).bind(&group).fetch_one(&pool).await.unwrap();
    let library: String = sqlx::query_scalar(
        r#"INSERT INTO public.library (name, "ownerId", "importPaths", "exclusionPatterns")
        VALUES ('OPM disposable test', $1::uuid, ARRAY['/photos'], ARRAY['**/@eaDir/**', '**/.opm-trash/**']) RETURNING id::text"#)
        .bind(&owner).fetch_one(&pool).await.unwrap();
    let old = "/photos/old.jpg";
    let new = "/photos/new.jpg";
    let id = asset(
        &pool,
        &owner,
        &library,
        old,
        &path_checksum(old).unwrap(),
        "sha1-path",
    )
    .await;
    let album: String =
        sqlx::query_scalar("INSERT INTO public.album DEFAULT VALUES RETURNING id::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        r#"INSERT INTO public.album_asset ("albumId", "assetId") VALUES ($1::uuid, $2::uuid)"#,
    )
    .bind(&album)
    .bind(&id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO public.asset_file ("assetId", type, path) VALUES ($1::uuid, 'sidecar', '/photos/old.xmp')"#)
        .bind(&id).execute(&pool).await.unwrap();
    // Official discovery sees the old path as free; its plain INSERT then fails
    // if a path-only writer retained the old digest. Reproduce that regression.
    sqlx::query(r#"UPDATE public.asset SET "originalPath" = $1 WHERE id = $2::uuid"#)
        .bind(new)
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let collision = sqlx::query(
        r#"INSERT INTO public.asset ("ownerId", "libraryId", "isExternal", type,
        "originalPath", "originalFileName", checksum, "checksumAlgorithm",
        "fileCreatedAt", "fileModifiedAt", "localDateTime")
        VALUES ($1::uuid, $2::uuid, true, 'IMAGE', $3, 'unrelated.jpg', $4,
        'sha1-path', now(), now(), now())"#,
    )
    .bind(&owner)
    .bind(&library)
    .bind(old)
    .bind(path_checksum(old).unwrap())
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(collision.as_database_error().unwrap().is_unique_violation());
    sqlx::query(r#"UPDATE public.asset SET "originalPath" = $1 WHERE id = $2::uuid"#)
        .bind(old)
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();

    let spec = MoveSpec {
        asset_id: id.clone(),
        owner_id: owner.clone(),
        library_id: library.clone(),
        old: old.into(),
        new: new.into(),
        new_name: "new.jpg".into(),
        sidecars: vec![SidecarMove {
            old: "/photos/old.xmp".into(),
            new: "/photos/new.xmp".into(),
        }],
    };
    let prepared = database.prepare(spec.clone()).await.unwrap();
    let asset_updated_at: String =
        sqlx::query_scalar(r#"SELECT "updatedAt"::text FROM public.asset WHERE id = $1::uuid"#)
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let sidecar_updated_at: String = sqlx::query_scalar(
        r#"SELECT "updatedAt"::text FROM public.asset_file
        WHERE "assetId" = $1::uuid AND type = 'sidecar' AND "isEdited" = false"#,
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(database.apply(&prepared).await, Ok(MoveApplied::Updated));
    assert_eq!(
        database.apply(&prepared).await,
        Ok(MoveApplied::AlreadyApplied)
    );
    let (path, filename, checksum, cursor): (String, String, Vec<u8>, String) = sqlx::query_as(
        r#"SELECT "originalPath", "originalFileName", checksum, "updateId"::text FROM public.asset WHERE id = $1::uuid"#)
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!((path.as_str(), filename.as_str()), (new, "new.jpg"));
    assert_eq!(checksum, path_checksum(new).unwrap());
    assert_ne!(cursor, prepared.update_id);
    let (sidecar, sidecar_cursor): (String, String) = sqlx::query_as(
        r#"SELECT path, "updateId"::text FROM public.asset_file WHERE "assetId" = $1::uuid AND type = 'sidecar'"#)
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(sidecar, "/photos/new.xmp");
    assert_ne!(Some(sidecar_cursor), prepared.sidecar_update_id);
    let asset_changed: bool = sqlx::query_scalar(
        r#"SELECT "updatedAt"::text <> $2 FROM public.asset WHERE id = $1::uuid"#,
    )
    .bind(&id)
    .bind(asset_updated_at)
    .fetch_one(&pool)
    .await
    .unwrap();
    let sidecar_changed: bool = sqlx::query_scalar(
        r#"SELECT "updatedAt"::text <> $2 FROM public.asset_file
        WHERE "assetId" = $1::uuid AND type = 'sidecar' AND "isEdited" = false"#,
    )
    .bind(&id)
    .bind(sidecar_updated_at)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(asset_changed, "the real asset timestamp trigger must fire");
    assert!(
        sidecar_changed,
        "the real sidecar timestamp trigger must fire"
    );
    let associated: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM public.album_asset WHERE "albumId" = $1::uuid AND "assetId" = $2::uuid)"#)
        .bind(&album).bind(&id).fetch_one(&pool).await.unwrap();
    assert!(associated);
    let reused = asset(
        &pool,
        &owner,
        &library,
        old,
        &path_checksum(old).unwrap(),
        "sha1-path",
    )
    .await;
    assert_ne!(reused, id);

    let bytes = vec![17; 20];
    let content_id = asset(
        &pool,
        &owner,
        &library,
        "/photos/content.jpg",
        &bytes,
        "sha1",
    )
    .await;
    let mut content = MoveSpec {
        asset_id: content_id.clone(),
        old: "/photos/content.jpg".into(),
        new: "/photos/content-new.jpg".into(),
        new_name: "content-new.jpg".into(),
        sidecars: vec![],
        ..spec.clone()
    };
    let prepared = database.prepare(content.clone()).await.unwrap();
    database.apply(&prepared).await.unwrap();
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM public.asset WHERE id = $1::uuid")
            .bind(&content_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, bytes);

    // A destination digest already held at a DIFFERENT path must also refuse.
    let target = "/photos/blocked.jpg";
    asset(
        &pool,
        &owner,
        &library,
        "/photos/different.jpg",
        &path_checksum(target).unwrap(),
        "sha1-path",
    )
    .await;
    let mut blocked = spec.clone();
    blocked.asset_id = reused.clone();
    blocked.sidecars.clear();
    blocked.new = target.into();
    blocked.new_name = "blocked.jpg".into();
    assert_eq!(database.prepare(blocked).await, Err(DbMoveError::Collision));
    let mut late_collision = spec.clone();
    late_collision.asset_id = reused.clone();
    late_collision.sidecars.clear();
    late_collision.new = "/photos/late.jpg".into();
    late_collision.new_name = "late.jpg".into();
    let before_collision = database.prepare(late_collision).await.unwrap();
    asset(
        &pool,
        &owner,
        &library,
        "/photos/scanner-race.jpg",
        &path_checksum("/photos/late.jpg").unwrap(),
        "sha1-path",
    )
    .await;
    assert_eq!(
        database.apply(&before_collision).await,
        Err(DbMoveError::Collision)
    );
    let unchanged: (String, Vec<u8>) =
        sqlx::query_as(r#"SELECT "originalPath", checksum FROM public.asset WHERE id = $1::uuid"#)
            .bind(&reused)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unchanged, (old.into(), path_checksum(old).unwrap()));

    let mut excluded_spec = before_collision.spec.clone();
    excluded_spec.new = "/photos/@eaDir/late.jpg".into();
    assert_eq!(
        database.prepare(excluded_spec).await,
        Err(DbMoveError::Destination)
    );
    let mut identity = before_collision.spec.clone();
    identity.owner_id = group.clone();
    assert_eq!(database.prepare(identity).await, Err(DbMoveError::Identity));
    let mut wrong_library = before_collision.spec.clone();
    wrong_library.library_id = group.clone();
    assert_eq!(
        database.prepare(wrong_library).await,
        Err(DbMoveError::Identity)
    );
    let mut outside_library = before_collision.spec.clone();
    outside_library.new = "/other-library/late.jpg".into();
    assert_eq!(
        database.prepare(outside_library).await,
        Err(DbMoveError::Destination)
    );
    let mut stale_path = before_collision.spec.clone();
    stale_path.old = "/photos/not-the-original.jpg".into();
    stale_path.new = "/photos/stale-source-check.jpg".into();
    stale_path.new_name = "stale-source-check.jpg".into();
    assert_eq!(database.prepare(stale_path).await, Err(DbMoveError::Stale));
    sqlx::query(r#"UPDATE public.asset SET "isExternal" = false WHERE id = $1::uuid"#)
        .bind(&reused)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        database.prepare(before_collision.spec.clone()).await,
        Err(DbMoveError::Identity)
    );
    sqlx::query(r#"UPDATE public.asset SET "isExternal" = true WHERE id = $1::uuid"#)
        .bind(&reused)
        .execute(&pool)
        .await
        .unwrap();
    content.old = content.new.clone();
    content.new = "/photos/stale.jpg".into();
    let stale = database.prepare(content.clone()).await.unwrap();
    sqlx::query(
        r#"UPDATE public.asset SET "originalFileName" = 'changed.jpg' WHERE id = $1::uuid"#,
    )
    .bind(&content_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(database.apply(&stale).await, Err(DbMoveError::Stale));

    let mut conn = pool.acquire().await.unwrap();
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("DELETE FROM public.kysely_migrations WHERE name = $1")
        .bind(contract::MIGRATIONS.lines().last().unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        contract::verify(&mut tx).await,
        Err(DbMoveError::Incompatible)
    );
    tx.rollback().await.unwrap();
    // Synthetic rows are intentionally confined to a disposable DB. The setup
    // tears down its named containers/data even if an assertion fails.
    drop(conn);
    pool.close().await;
}
