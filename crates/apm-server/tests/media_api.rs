//! The media endpoints end to end: thumbnails, previews and `content` with `Range`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, header};
use common::media_fixtures::{
    decoded_size, file_id, index, jpeg, jpeg_with_orientation, png_with_alpha, thumb_state,
};
use common::{TestApp, Tree};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fetch(
    app: &TestApp,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> (Response<Body>, Vec<u8>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "localhost");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes().to_vec();
    (Response::from_parts(parts, Body::empty()), bytes)
}

async fn get(app: &TestApp, path: &str) -> (Response<Body>, Vec<u8>) {
    fetch(app, Method::GET, path, &[]).await
}

fn hdr(response: &Response<Body>, name: &str) -> String {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default()
}

fn error_code(bytes: &[u8]) -> String {
    let json: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    json["error"]["code"].as_str().unwrap().to_owned()
}

async fn setup(files: &[(&str, Vec<u8>)]) -> (TestApp, i64, Tree) {
    let app = TestApp::new().await;
    let (volume, tree) = app.add_volume("photos").await;
    for (name, bytes) in files {
        tree.write(name, bytes);
    }
    index(&app).await;
    (app, volume, tree)
}

#[tokio::test]
async fn thumbnails_are_bounded_jpegs_that_never_upscale() {
    let (app, volume, _tree) =
        setup(&[("big.jpg", jpeg(1600, 1200)), ("tiny.jpg", jpeg(64, 48))]).await;
    let big = file_id(&app, volume, "", "big.jpg").await;
    let tiny = file_id(&app, volume, "", "tiny.jpg").await;

    let (response, bytes) = get(&app, &format!("/api/files/{big}/thumb?size=sm")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hdr(&response, "content-type"), "image/jpeg");
    assert_eq!(
        hdr(&response, "cache-control"),
        "private, max-age=31536000, immutable"
    );
    assert_eq!(decoded_size(&bytes), (320, 240));

    let (_, default) = get(&app, &format!("/api/files/{big}/thumb")).await;
    assert_eq!(default, bytes, "sm is the default size");

    let (_, lg) = get(&app, &format!("/api/files/{big}/thumb?size=lg&v=1")).await;
    assert_eq!(decoded_size(&lg), (800, 600));

    let (_, small) = get(&app, &format!("/api/files/{tiny}/thumb?size=lg")).await;
    assert_eq!(decoded_size(&small), (64, 48));
}

#[tokio::test]
async fn a_matching_etag_answers_304_and_the_row_is_marked_done() {
    let (app, volume, _tree) = setup(&[("a.jpg", jpeg(800, 600))]).await;
    let id = file_id(&app, volume, "", "a.jpg").await;
    assert_eq!(thumb_state(&app, id).await, 1, "the stage pre-generated it");

    let (response, _) = get(&app, &format!("/api/files/{id}/thumb")).await;
    let etag = hdr(&response, "etag");
    assert!(etag.starts_with("\"thumb-sm-"), "{etag}");
    let (response, body) = fetch(
        &app,
        Method::GET,
        &format!("/api/files/{id}/thumb"),
        &[("if-none-match", &etag)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
}

#[tokio::test]
async fn exif_orientation_is_applied_for_every_value() {
    let files: Vec<(String, Vec<u8>)> = (1..=8)
        .map(|o| (format!("o{o}.jpg"), jpeg_with_orientation(60, 40, o)))
        .collect();
    let borrowed: Vec<(&str, Vec<u8>)> =
        files.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    let (app, volume, _tree) = setup(&borrowed).await;
    for orientation in 1..=8_u16 {
        let id = file_id(&app, volume, "", &format!("o{orientation}.jpg")).await;
        let (response, bytes) = get(&app, &format!("/api/files/{id}/thumb?size=lg")).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "orientation {orientation}"
        );
        let (w, h) = decoded_size(&bytes);
        let expected = if orientation >= 5 { (40, 60) } else { (60, 40) };
        assert_eq!((w, h), expected, "orientation {orientation}");
    }
}

#[tokio::test]
async fn transparent_pixels_are_flattened_onto_light_grey() {
    let (app, volume, _tree) = setup(&[("a.png", png_with_alpha(300, 300))]).await;
    let id = file_id(&app, volume, "", "a.png").await;
    let (response, bytes) = get(&app, &format!("/api/files/{id}/thumb")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let img = image::load_from_memory(&bytes).unwrap().to_rgb8();
    let corner = img.get_pixel(2, 2);
    assert!(corner.0.iter().all(|&c| c > 200), "{corner:?}");
}

#[tokio::test]
async fn error_statuses() {
    let (app, volume, tree) = setup(&[
        ("a.jpg", jpeg(100, 100)),
        ("bad.jpg", b"this is not a jpeg".to_vec()),
        ("notes.txt", b"hello".to_vec()),
    ])
    .await;
    let a = file_id(&app, volume, "", "a.jpg").await;
    let bad = file_id(&app, volume, "", "bad.jpg").await;
    let txt = file_id(&app, volume, "", "notes.txt").await;

    let (response, body) = get(&app, &format!("/api/files/{a}/thumb?size=huge")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "invalid_query");

    let (response, body) = get(&app, "/api/files/999999/thumb").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");

    let (response, body) = get(&app, &format!("/api/files/{txt}/thumb")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "no_thumbnail");
    let (response, body) = get(&app, &format!("/api/files/{txt}/preview")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "no_preview");

    let (response, body) = get(&app, &format!("/api/files/{bad}/thumb")).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(&body), "thumbnail_failed");
    assert_eq!(hdr(&response, "cache-control"), "private, max-age=600");
    assert_eq!(thumb_state(&app, bad).await, 2);

    tree.remove("a.jpg");
    let (response, body) = get(&app, &format!("/api/files/{a}/content")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "file_missing");
}

#[tokio::test]
async fn a_symlink_put_in_place_of_a_file_is_refused() {
    let (app, volume, tree) = setup(&[("a.jpg", jpeg(100, 100)), ("b.jpg", jpeg(50, 50))]).await;
    let a = file_id(&app, volume, "", "a.jpg").await;
    tree.remove("a.jpg");
    std::os::unix::fs::symlink(tree.path("b.jpg"), tree.path("a.jpg")).unwrap();
    let (response, body) = get(&app, &format!("/api/files/{a}/content")).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "symlink_rejected");
}

#[tokio::test]
async fn content_range_matrix() {
    let data: Vec<u8> = (0..=255_u8).cycle().take(1000).collect();
    let (app, volume, _tree) = setup(&[("clip.bin", data.clone())]).await;
    let id = file_id(&app, volume, "", "clip.bin").await;
    let path = format!("/api/files/{id}/content");

    let (response, body) = get(&app, &path).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body, data);
    assert_eq!(hdr(&response, "accept-ranges"), "bytes");
    assert_eq!(hdr(&response, "x-content-type-options"), "nosniff");
    assert_eq!(hdr(&response, "content-security-policy"), "sandbox");
    assert_eq!(hdr(&response, "cache-control"), "private, no-cache");

    let cases: [(&str, StatusCode, std::ops::Range<usize>, &str); 4] = [
        (
            "bytes=0-99",
            StatusCode::PARTIAL_CONTENT,
            0..100,
            "bytes 0-99/1000",
        ),
        (
            "bytes=900-",
            StatusCode::PARTIAL_CONTENT,
            900..1000,
            "bytes 900-999/1000",
        ),
        (
            "bytes=-10",
            StatusCode::PARTIAL_CONTENT,
            990..1000,
            "bytes 990-999/1000",
        ),
        (
            "bytes=990-5000",
            StatusCode::PARTIAL_CONTENT,
            990..1000,
            "bytes 990-999/1000",
        ),
    ];
    for (range, status, slice, content_range) in cases {
        let (response, body) = fetch(&app, Method::GET, &path, &[("range", range)]).await;
        assert_eq!(response.status(), status, "{range}");
        assert_eq!(body, data[slice], "{range}");
        assert_eq!(hdr(&response, "content-range"), content_range, "{range}");
    }

    let (response, body) = fetch(&app, Method::GET, &path, &[("range", "bytes=1000-")]).await;
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(hdr(&response, "content-range"), "bytes */1000");
    assert!(body.is_empty());

    let (response, body) = fetch(&app, Method::GET, &path, &[("range", "bytes=0-1,5-9")]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body.len(), 1000);

    let last_modified = hdr(&get(&app, &path).await.0, "last-modified");
    let (response, body) = fetch(
        &app,
        Method::GET,
        &path,
        &[("range", "bytes=0-9"), ("if-range", &last_modified)],
    )
    .await;
    assert_eq!(
        (response.status(), body.len()),
        (StatusCode::PARTIAL_CONTENT, 10)
    );
    let (response, body) = fetch(
        &app,
        Method::GET,
        &path,
        &[
            ("range", "bytes=0-9"),
            ("if-range", "Wed, 01 Jan 2020 00:00:00 GMT"),
        ],
    )
    .await;
    assert_eq!((response.status(), body.len()), (StatusCode::OK, 1000));

    let (response, body) = fetch(&app, Method::HEAD, &path, &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hdr(&response, "content-length"), "1000");
    assert!(body.is_empty());

    let etag = hdr(&get(&app, &path).await.0, "etag");
    let (response, _) = fetch(&app, Method::GET, &path, &[("if-none-match", &etag)]).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    let (response, _) = fetch(
        &app,
        Method::GET,
        &path,
        &[("if-modified-since", &last_modified)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn a_huge_sparse_file_streams_a_tiny_range() {
    let (app, volume, tree) = setup(&[("big.bin", b"x".to_vec())]).await;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(tree.path("big.bin"))
        .unwrap();
    file.set_len(200 * 1024 * 1024).unwrap();
    drop(file);
    tree.set_mtime("big.bin", 1_700_000_100);
    index(&app).await;
    let id = file_id(&app, volume, "", "big.bin").await;
    let started = std::time::Instant::now();
    let (response, body) = fetch(
        &app,
        Method::GET,
        &format!("/api/files/{id}/content"),
        &[("range", "bytes=104857600-104857609")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, vec![0; 10]);
    assert!(started.elapsed().as_secs() < 5);
}

#[tokio::test]
async fn hostile_names_and_active_types_are_safe() {
    let (app, volume, _tree) = setup(&[
        ("a\"b\u{e9}.txt", b"hi".to_vec()),
        ("page.html", b"<script>alert(1)</script>".to_vec()),
    ])
    .await;
    let id = file_id(&app, volume, "", "a\"b\u{e9}.txt").await;
    let (response, _) = get(&app, &format!("/api/files/{id}/content")).await;
    let disposition = hdr(&response, "content-disposition");
    assert!(
        disposition.starts_with("inline; filename=\"a_b_.txt\""),
        "{disposition}"
    );
    assert!(
        disposition.contains("filename*=UTF-8''a%22b%C3%A9.txt"),
        "{disposition}"
    );

    let html = file_id(&app, volume, "", "page.html").await;
    let (response, _) = get(&app, &format!("/api/files/{html}/content")).await;
    assert!(hdr(&response, "content-disposition").starts_with("attachment;"));
    let (response, _) = get(&app, &format!("/api/files/{id}/content?download=1")).await;
    assert!(hdr(&response, "content-disposition").starts_with("attachment;"));
}

#[tokio::test]
async fn previews_serve_native_images_and_render_the_rest() {
    let tiff = {
        let img = image::RgbImage::from_pixel(3000, 2000, image::Rgb([1, 2, 3]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Tiff).unwrap();
        out.into_inner()
    };
    let original = jpeg(300, 200);
    let (app, volume, _tree) = setup(&[("a.jpg", original.clone()), ("b.tif", tiff)]).await;
    let a = file_id(&app, volume, "", "a.jpg").await;
    let (response, body) = get(&app, &format!("/api/files/{a}/preview")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body, original);

    let b = file_id(&app, volume, "", "b.tif").await;
    let (response, body) = get(&app, &format!("/api/files/{b}/preview")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(hdr(&response, "content-type"), "image/jpeg");
    let (w, h) = decoded_size(&body);
    assert_eq!(w, 2560);
    assert!((1706..=1707).contains(&h), "{h}");
    assert_eq!(hdr(&response, "cache-control"), "private, no-cache");
}
