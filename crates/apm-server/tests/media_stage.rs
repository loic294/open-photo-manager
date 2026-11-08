//! The thumbnail stage of the indexer (stage 4), and an ignored benchmark of the renderers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Instant;

use common::TestApp;
use common::media_fixtures::{file_id, index, jpeg, png_with_alpha, thumb_state};

#[tokio::test]
async fn the_stage_marks_done_failed_and_not_applicable_and_is_resumable() {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    tree.write("a.jpg", &jpeg(200, 100));
    tree.write("bad.jpg", b"not a jpeg");
    tree.write("notes.txt", b"text");
    tree.write("song.mp3", b"audio");
    index(&app).await;

    let state = |name: &'static str| {
        let app = &app;
        async move {
            let id = file_id(app, volume, "", name).await;
            thumb_state(app, id).await
        }
    };
    assert_eq!(state("a.jpg").await, 1);
    assert_eq!(state("bad.jpg").await, 2);
    assert_eq!(state("notes.txt").await, 3);
    assert_eq!(state("song.mp3").await, 3);

    // A changed file becomes pending again and is rendered on the next run.
    tree.write_at("a.jpg", &jpeg(300, 200), 1_700_000_500);
    index(&app).await;
    assert_eq!(state("a.jpg").await, 1);
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE thumb_state = 0")
        .fetch_one(app.state.db.read())
        .await
        .unwrap();
    assert_eq!(pending, 0);
}

#[tokio::test]
#[ignore = "benchmark: prints renders per second"]
async fn bench_thumbnails() {
    let app = TestApp::new().await;
    let (_volume, tree) = app.add_volume("photos").await;
    let sample = jpeg(6000, 4000);
    for i in 0..40 {
        tree.write(&format!("p{i}.jpg"), &sample);
    }
    tree.write("a.png", &png_with_alpha(4000, 3000));
    let started = Instant::now();
    index(&app).await;
    let elapsed = started.elapsed();
    println!(
        "41 files (24 MP) in {elapsed:?}: {:.1} renders/s",
        41.0 / elapsed.as_secs_f64()
    );
}
