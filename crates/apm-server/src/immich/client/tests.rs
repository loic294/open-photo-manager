//! Tests of the Immich client against an in-process mock server.
//!
//! Nothing here talks to a real Immich: every test starts its own `wiremock` server (or a raw TCP
//! listener for answers `wiremock` cannot produce) and asserts both what the client sent and what it
//! made of the answer.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request as Received, ResponseTemplate};

use super::*;
use crate::immich::models::{AssetType, AssetVisibility, OfflineFilter};

const KEY: &str = "apm-test-key-6c1b0e8d";
const BEARER: &str = "apm-test-bearer-7d2a9f41";
const PASSWORD: &str = "apm-test-password-3e8c55";
const LIBRARY: &str = "6f1c6a1e-0d6f-4a53-9a4c-3d9c7c1a2b11";
const OTHER_LIBRARY: &str = "0b5a8f0e-72a1-4e2b-8d57-9c7a4b1d6e22";
const ASSET: &str = "3b7e1c52-92d4-4f0e-8a6b-5c1d2e3f4a50";
const KEY_ID: &str = "9a4e2c70-5b1d-4c3a-8e6f-1d2c3b4a5e60";

fn secret(text: &str) -> Redacted<String> {
    Redacted::new(text.to_owned())
}

fn with_options(server: &MockServer, options: ClientOptions) -> ImmichClient {
    ImmichClient::new(&server.uri(), Some(secret(KEY)), options).unwrap()
}

fn keyed(server: &MockServer) -> ImmichClient {
    with_options(server, ClientOptions::default())
}

fn anonymous(server: &MockServer) -> ImmichClient {
    ImmichClient::new(&server.uri(), None, ClientOptions::default()).unwrap()
}

fn knowing(version: Option<ImmichVersion>) -> ClientOptions {
    ClientOptions {
        server_version: version,
        ..ClientOptions::default()
    }
}

async fn requests(server: &MockServer) -> Vec<Received> {
    server.received_requests().await.unwrap()
}

fn body_of(request: &Received) -> Value {
    request.body_json().unwrap()
}

fn header_of(request: &Received, name: &str) -> Option<String> {
    request
        .headers
        .get(name)
        .map(|value| value.to_str().unwrap().to_owned())
}

fn asset_id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn asset_json(n: u32, library: &str, file: &str) -> Value {
    json!({
        "id": asset_id(n),
        "checksum": "c2hhMQ==",
        "originalPath": format!("/mnt/photo/{file}"),
        "originalFileName": file,
        "libraryId": library,
        "isOffline": false,
        "isTrashed": false,
        "isFavorite": false,
        "visibility": "timeline",
        "type": "IMAGE",
        "updatedAt": "2024-05-01T10:00:00.000Z",
        "fileCreatedAt": "2024-04-30T08:00:00.000Z",
        "aFutureField": { "nested": [1, 2, 3] },
    })
}

fn search_answer(items: Vec<Value>, next_cursor: Option<&str>, next_page: Option<&str>) -> Value {
    let count = items.len();
    json!({ "assets": {
        "items": items,
        "count": count,
        "total": count,
        "facets": [],
        "nextCursor": next_cursor,
        "nextPage": next_page,
    }})
}

fn ok_json(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

fn nest_error(status: u16, message: Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "message": message,
        "error": "Bad Request",
        "statusCode": status,
    }))
}

/// Mounts one answer for `method path` on `server`.
async fn mount(server: &MockServer, verb: &str, route: &str, answer: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(answer)
        .mount(server)
        .await;
}

/// Reads every page of a search, following the tokens.
async fn read_all(client: &ImmichClient, mut search: MetadataSearch) -> Vec<String> {
    let mut ids = Vec::new();
    for _ in 0..10 {
        let page = client.search_metadata(&search).await.unwrap();
        ids.extend(page.items.into_iter().map(|asset| asset.id));
        match page.next {
            Some(next) => search.after = Some(next),
            None => return ids,
        }
    }
    panic!("the search does not end");
}

/// Serves exactly one connection with the given raw bytes as the whole answer, then closes it.
/// Returns the base address.
async fn raw_server(answer: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !seen.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => seen.extend_from_slice(&chunk[..read]),
            }
        }
        let _ = stream.write_all(&answer).await;
        let _ = stream.shutdown().await;
    });
    format!("http://{address}")
}

// ----- addresses -----------------------------------------------------------------------------

#[test]
fn server_addresses_are_normalised() {
    let accepted = [
        ("http://immich.local:2283", "http://immich.local:2283"),
        ("https://h/", "https://h"),
        ("https://h/api", "https://h"),
        ("https://h/api/", "https://h"),
        ("https://h:8443/API", "https://h:8443"),
        ("https://h//api", "https://h"),
        ("https://example.com/photos/", "https://example.com/photos"),
        (
            "https://example.com/photos/api",
            "https://example.com/photos",
        ),
        ("https://u:pw@h/?token=abc#frag", "https://h"),
        ("  http://h:80  ", "http://h"),
        ("HTTP://H", "http://h"),
        ("http://[::1]:2283", "http://[::1]:2283"),
        ("http://192.168.1.20:2283/", "http://192.168.1.20:2283"),
    ];
    for (input, expected) in accepted {
        let client = ImmichClient::new(input, None, ClientOptions::default())
            .unwrap_or_else(|err| panic!("{input}: {err}"));
        assert_eq!(client.base_url(), expected, "{input}");
    }
}

#[test]
fn unusable_addresses_are_refused_without_echoing_them() {
    for input in [
        "",
        "   ",
        "immich.local",
        "immich.local:2283",
        "ftp://h",
        "file:///etc/passwd",
        "http://",
        "https://:2283",
        "http://user:hunter2@",
        "ftp://user:hunter2@h",
        "javascript:alert(1)",
    ] {
        let err = normalize_base_url(input).unwrap_err();
        assert!(
            matches!(err, ImmichError::InvalidUrl(_)),
            "{input}: {err:?}"
        );
        assert!(!err.to_string().contains("hunter2"), "{input}: {err}");
    }
}

#[test]
fn request_urls_go_under_api_and_keep_a_proxy_prefix() {
    let client = ImmichClient::new(
        "https://example.com/photos/",
        None,
        ClientOptions::default(),
    )
    .unwrap();
    let url = client.url(&["libraries", LIBRARY, "scan"], &[]).unwrap();
    assert_eq!(
        url.as_str(),
        format!("https://example.com/photos/api/libraries/{LIBRARY}/scan")
    );
    let url = client.url(&["albums"], &[("assetId", "a b&c=d")]).unwrap();
    assert_eq!(url.path(), "/photos/api/albums");
    assert_eq!(
        url.query_pairs().collect::<Vec<_>>(),
        [("assetId".into(), "a b&c=d".into())]
    );

    let root = ImmichClient::new("http://h:2283", None, ClientOptions::default()).unwrap();
    assert_eq!(
        root.url(&["server", "ping"], &[]).unwrap().as_str(),
        "http://h:2283/api/server/ping"
    );
}

#[test]
fn ids_that_go_into_urls_are_checked() {
    for good in [LIBRARY, ASSET, "abc_DEF-123", "0"] {
        assert_eq!(safe_id(good).unwrap(), good);
    }
    let too_long = "a".repeat(65);
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a b",
        "a?b",
        "a#b",
        "%2e%2e",
        "../x",
        "ä",
        too_long.as_str(),
    ] {
        let err = safe_id(bad).unwrap_err();
        assert!(matches!(err, ImmichError::Validation(_)), "{bad}: {err:?}");
    }
}

#[tokio::test]
async fn a_bad_id_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let client = keyed(&server);
    assert!(matches!(
        client.get_asset("../users/me").await,
        Err(ImmichError::Validation(_))
    ));
    assert!(matches!(
        client.library("a/b").await,
        Err(ImmichError::Validation(_))
    ));
    assert!(matches!(
        client.delete_api_key("").await,
        Err(ImmichError::Validation(_))
    ));
    assert!(matches!(
        client.albums_for_asset("x y").await,
        Err(ImmichError::Validation(_))
    ));
    assert!(requests(&server).await.is_empty());
}

// ----- credentials -----------------------------------------------------------------------------

#[test]
fn the_key_permissions_are_the_agreed_ones_and_exist_in_the_spec() {
    assert_eq!(
        KEY_PERMISSIONS,
        [
            "asset.read",
            "asset.update",
            "asset.delete",
            "library.read",
            "library.update",
            "album.read",
            "albumAsset.create",
            "tag.read",
            "tag.asset",
            "server.about",
            "user.read",
            "apiKey.delete",
        ]
    );
    let spec: Vec<&str> = include_str!("../spec-permissions.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert!(
        spec.len() > 50,
        "the permission list of the spec is missing"
    );
    for permission in KEY_PERMISSIONS {
        assert!(spec.contains(permission), "{permission} is not in the spec");
    }
}

#[test]
fn a_key_that_cannot_be_a_header_needs_a_new_connection() {
    for bad in ["bad\nkey", "bad\u{0}key", "bad\u{7f}key"] {
        let err =
            ImmichClient::new("http://h", Some(secret(bad)), ClientOptions::default()).unwrap_err();
        assert!(
            matches!(err, ImmichError::NeedsReconnect),
            "{bad:?}: {err:?}"
        );
    }
}

#[tokio::test]
async fn a_call_that_needs_a_key_without_one_is_not_connected() {
    let server = MockServer::start().await;
    let client = anonymous(&server);
    assert!(matches!(client.me().await, Err(ImmichError::NotConnected)));
    assert!(matches!(
        client.libraries().await,
        Err(ImmichError::NotConnected)
    ));
    assert!(requests(&server).await.is_empty());
    assert_eq!(client.health().reachable(), None);
}

#[tokio::test]
async fn debug_output_and_errors_never_show_the_key() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/users/me",
        nest_error(500, json!("boom")),
    )
    .await;
    let client = keyed(&server);
    assert!(!format!("{client:?}").contains(KEY));
    assert!(format!("{client:?}").contains("authenticated: true"));
    let err = client.me().await.unwrap_err();
    assert!(!format!("{err:?}").contains(KEY));
    assert!(!err.to_string().contains(KEY));
    assert!(!format!("{:?}", client.health()).contains(KEY));

    assert!(client.key_header.as_ref().unwrap().is_sensitive());
    let sent = requests(&server).await;
    assert_eq!(header_of(&sent[0], "x-api-key").as_deref(), Some(KEY));
}

#[tokio::test]
async fn login_posts_the_credentials_and_returns_the_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/auth/login"))
        .and(body_json(
            json!({ "email": "ann@example.com", "password": PASSWORD }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "accessToken": BEARER,
            "userId": ASSET,
            "userEmail": "ann@example.com",
            "name": "Ann",
            "isAdmin": true,
            "isOnboarded": true,
            "profileImagePath": "",
            "shouldChangePassword": false,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let session = anonymous(&server)
        .login("ann@example.com", &secret(PASSWORD))
        .await
        .unwrap();
    assert_eq!(session.access_token.expose(), BEARER);
    assert_eq!(session.user_id, ASSET);
    assert_eq!(session.user_name, "Ann");
    assert!(session.is_admin);
    assert!(!format!("{session:?}").contains(BEARER));

    let sent = requests(&server).await;
    assert_eq!(sent.len(), 1);
    assert!(sent[0].headers.get("x-api-key").is_none());
    assert!(sent[0].headers.get("authorization").is_none());
}

#[tokio::test]
async fn login_failures_are_typed_and_carry_no_secret() {
    let server = MockServer::start().await;
    mount(
        &server,
        "POST",
        "/api/auth/login",
        nest_error(401, json!("Incorrect email or password")),
    )
    .await;
    let err = anonymous(&server)
        .login("ann@example.com", &secret(PASSWORD))
        .await
        .unwrap_err();
    assert!(matches!(err, ImmichError::AuthFailed), "{err:?}");

    let broken = MockServer::start().await;
    mount(
        &broken,
        "POST",
        "/api/auth/login",
        ResponseTemplate::new(201).set_body_json(json!({ "accessToken": 12345, "name": 7 })),
    )
    .await;
    let err = anonymous(&broken)
        .login("ann@example.com", &secret(PASSWORD))
        .await
        .unwrap_err();
    let ImmichError::Protocol(text) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(text, "unexpected response from POST /auth/login");

    let echoing = MockServer::start().await;
    mount(
        &echoing,
        "POST",
        "/api/auth/login",
        nest_error(500, json!(format!("cannot log in {PASSWORD}"))),
    )
    .await;
    let err = anonymous(&echoing)
        .login("ann@example.com", &secret(PASSWORD))
        .await
        .unwrap_err();
    // The server's own text is kept for diagnosis, but the request never leaves this process in
    // an error of ours: the password is only in the body that was sent.
    assert!(
        matches!(err, ImmichError::Upstream { status: 500, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_api_key_is_created_with_the_bearer_token_and_the_permission_list() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/api-keys"))
        .and(header("authorization", format!("Bearer {BEARER}").as_str()))
        .and(body_json(json!({
            "name": API_KEY_NAME,
            "permissions": KEY_PERMISSIONS,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": KEY_ID,
            "name": API_KEY_NAME,
            "permissions": KEY_PERMISSIONS,
            "secret": KEY,
            "apiKey": { "id": KEY_ID },
            "createdAt": "2024-05-01T10:00:00.000Z",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let created = anonymous(&server)
        .create_api_key(&secret(BEARER), API_KEY_NAME, KEY_PERMISSIONS)
        .await
        .unwrap();
    assert_eq!(created.id, KEY_ID);
    assert_eq!(created.secret.expose(), KEY);
    assert!(!format!("{created:?}").contains(KEY));
}

#[tokio::test]
async fn logout_and_cleanup_use_the_bearer_token() {
    let server = MockServer::start().await;
    let bearer = format!("Bearer {BEARER}");
    Mock::given(method("POST"))
        .and(path("/api/auth/logout"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "successful": true,
            "redirectUri": "/auth/login?autoLaunch=0",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/api-keys/{KEY_ID}")))
        .and(header("authorization", bearer.as_str()))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = anonymous(&server);
    client.logout(&secret(BEARER)).await.unwrap();
    client
        .revoke_api_key_with_bearer(&secret(BEARER), KEY_ID)
        .await
        .unwrap();
    for request in requests(&server).await {
        assert!(request.headers.get("x-api-key").is_none());
    }
}

#[tokio::test]
async fn version_and_ping_need_no_credential() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/server/version",
        ok_json(json!({ "major": 3, "minor": 4, "patch": 0 })),
    )
    .await;
    mount(
        &server,
        "GET",
        "/api/server/ping",
        ok_json(json!({ "res": "pong" })),
    )
    .await;
    let client = keyed(&server);
    assert_eq!(client.version().await.unwrap(), ImmichVersion::new(3, 4, 0));
    client.ping().await.unwrap();
    for request in requests(&server).await {
        assert!(request.headers.get("x-api-key").is_none());
    }

    let other = MockServer::start().await;
    mount(
        &other,
        "GET",
        "/api/server/ping",
        ok_json(json!({ "res": "hello" })),
    )
    .await;
    assert!(matches!(
        anonymous(&other).ping().await,
        Err(ImmichError::Protocol(_))
    ));
}

#[tokio::test]
async fn the_probes_give_up_after_three_seconds() {
    let server = MockServer::start().await;
    let slow = ResponseTemplate::new(200)
        .set_body_json(json!({ "res": "pong", "version": "v3.4.0" }))
        .set_delay(Duration::from_secs(6));
    mount(&server, "GET", "/api/server/ping", slow.clone()).await;
    mount(&server, "GET", "/api/server/about", slow).await;
    let client = keyed(&server);

    let started = Instant::now();
    let (ping, check) = tokio::join!(client.ping(), client.check());
    assert!(started.elapsed() < Duration::from_secs(5));
    for outcome in [ping.map(drop), check.map(drop)] {
        let Err(ImmichError::Unreachable(text)) = outcome else {
            panic!("{outcome:?}")
        };
        assert!(text.contains("timed out"), "{text}");
    }
    assert_eq!(client.health().reachable(), Some(false));
}

#[tokio::test]
async fn the_api_key_calls_carry_the_key_and_parse_the_answers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/users/me"))
        .and(header("x-api-key", KEY))
        .respond_with(ok_json(json!({
            "id": ASSET,
            "name": "Ann",
            "email": "ann@example.com",
            "isAdmin": false,
            "shouldChangePassword": false,
            "oauthId": "",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/server/about"))
        .and(header("x-api-key", KEY))
        .respond_with(ok_json(json!({
            "version": "v3.4.0-rc.0",
            "versionUrl": "https://github.com/immich-app/immich/releases/tag/v3.4.0-rc.0",
            "licensed": false,
        })))
        .expect(2)
        .mount(&server)
        .await;

    let client = keyed(&server);
    let me = client.me().await.unwrap();
    assert_eq!(
        (me.id.as_str(), me.name.as_str(), me.is_admin),
        (ASSET, "Ann", false)
    );
    let about = client.about().await.unwrap();
    assert_eq!(about.parsed_version(), Some(ImmichVersion::new(3, 4, 0)));
    assert_eq!(
        client.check().await.unwrap().parsed_version(),
        Some(ImmichVersion::new(3, 4, 0))
    );
}

#[tokio::test]
async fn an_api_key_can_be_revoked_with_itself() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/api-keys/{KEY_ID}")))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    keyed(&server).delete_api_key(KEY_ID).await.unwrap();
}

// ----- libraries ------------------------------------------------------------------------------

#[tokio::test]
async fn libraries_are_listed_and_fetched_tolerantly() {
    let server = MockServer::start().await;
    let library = json!({
        "id": LIBRARY,
        "name": "Photos NAS",
        "ownerId": ASSET,
        "importPaths": ["/mnt/photo", "/mnt/video"],
        "exclusionPatterns": ["**/@eaDir/**"],
        "assetCount": 1234,
        "createdAt": "2024-01-01T00:00:00.000Z",
        "updatedAt": "2024-01-01T00:00:00.000Z",
        "refreshedAt": null,
        "aNewField": true,
    });
    mount(
        &server,
        "GET",
        "/api/libraries",
        ok_json(json!([library.clone()])),
    )
    .await;
    mount(
        &server,
        "GET",
        &format!("/api/libraries/{LIBRARY}"),
        ok_json(library),
    )
    .await;
    let client = keyed(&server);
    let listed = client.libraries().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, LIBRARY);
    assert_eq!(listed[0].import_paths, ["/mnt/photo", "/mnt/video"]);
    assert_eq!(listed[0].exclusion_patterns, ["**/@eaDir/**"]);
    assert_eq!(listed[0].asset_count, 1234);
    assert_eq!(client.library(LIBRARY).await.unwrap(), listed[0]);
}

#[tokio::test]
async fn a_non_admin_gets_forbidden_for_the_admin_only_library_calls() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/libraries",
        nest_error(403, json!("Forbidden")),
    )
    .await;
    let err = keyed(&server).libraries().await.unwrap_err();
    assert!(matches!(err, ImmichError::Forbidden), "{err:?}");
    assert_eq!(keyed(&server).health().reachable(), None);
}

#[tokio::test]
async fn exclusion_patterns_are_updated_alone_and_a_scan_is_requested() {
    let server = MockServer::start().await;
    let patterns = vec!["**/@eaDir/**".to_owned(), "**/.apm-trash/**".to_owned()];
    Mock::given(method("PUT"))
        .and(path(format!("/api/libraries/{LIBRARY}")))
        .and(header("x-api-key", KEY))
        .and(body_json(json!({ "exclusionPatterns": patterns })))
        .respond_with(ok_json(json!({ "id": LIBRARY, "name": "Photos NAS" })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/libraries/{LIBRARY}/scan")))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = keyed(&server);
    client
        .update_library_exclusions(LIBRARY, &patterns)
        .await
        .unwrap();
    client.scan_library(LIBRARY).await.unwrap();
}

// ----- assets ---------------------------------------------------------------------------------

#[tokio::test]
async fn an_asset_is_fetched_and_a_missing_one_is_none_in_both_shapes_immich_uses() {
    let server = MockServer::start().await;
    let known = asset_id(1);
    let unknown = asset_id(2);
    let hidden = asset_id(3);
    let broken = asset_id(4);
    mount(
        &server,
        "GET",
        &format!("/api/assets/{known}"),
        ok_json(asset_json(1, LIBRARY, "a.jpg")),
    )
    .await;
    mount(
        &server,
        "GET",
        &format!("/api/assets/{unknown}"),
        nest_error(404, json!("Not found")),
    )
    .await;
    mount(
        &server,
        "GET",
        &format!("/api/assets/{hidden}"),
        nest_error(400, json!("Not found or no asset.read access")),
    )
    .await;
    mount(
        &server,
        "GET",
        &format!("/api/assets/{broken}"),
        nest_error(400, json!(["id must be a UUID"])),
    )
    .await;

    let client = keyed(&server);
    let asset = client.get_asset(&known).await.unwrap().unwrap();
    assert_eq!(asset.id, known);
    assert_eq!(asset.original_path, "/mnt/photo/a.jpg");
    assert_eq!(asset.asset_type, AssetType::Image);
    assert!(client.get_asset(&unknown).await.unwrap().is_none());
    assert!(client.get_asset(&hidden).await.unwrap().is_none());
    let err = client.get_asset(&broken).await.unwrap_err();
    assert!(
        matches!(err, ImmichError::Upstream { status: 400, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn bulk_updates_go_out_in_requests_of_at_most_500_ids() {
    let server = MockServer::start().await;
    mount(&server, "PUT", "/api/assets", ResponseTemplate::new(204)).await;
    let ids: Vec<String> = (0..1201).map(asset_id).collect();
    let update = AssetUpdate {
        is_favorite: Some(true),
        rating: Some(0),
        ..AssetUpdate::default()
    };
    keyed(&server).update_assets(&ids, &update).await.unwrap();

    let sent = requests(&server).await;
    let sizes: Vec<usize> = sent
        .iter()
        .map(|request| body_of(request)["ids"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, [500, 500, 201]);
    for request in &sent {
        let body = body_of(request);
        assert_eq!(body["isFavorite"], json!(true));
        assert_eq!(body.get("rating"), Some(&Value::Null));
        assert!(body.get("visibility").is_none());
        assert!(body.get("description").is_none());
        assert_eq!(header_of(request, "x-api-key").as_deref(), Some(KEY));
    }
}

#[tokio::test]
async fn updates_send_exactly_the_fields_that_were_set() {
    let server = MockServer::start().await;
    mount(&server, "PUT", "/api/assets", ResponseTemplate::new(204)).await;
    let ids = vec![asset_id(1)];
    keyed(&server)
        .update_assets(
            &ids,
            &AssetUpdate {
                rating: Some(4),
                visibility: Some(AssetVisibility::Archive),
                description: Some("holiday".into()),
                ..AssetUpdate::default()
            },
        )
        .await
        .unwrap();
    let sent = requests(&server).await;
    assert_eq!(
        body_of(&sent[0]),
        json!({
            "ids": ids,
            "rating": 4,
            "visibility": "archive",
            "description": "holiday",
        })
    );
}

#[tokio::test]
async fn nothing_is_sent_for_no_ids_no_fields_or_an_invalid_field() {
    let server = MockServer::start().await;
    let client = keyed(&server);
    let ids = vec![asset_id(1)];
    client
        .update_assets(
            &[],
            &AssetUpdate {
                is_favorite: Some(true),
                ..AssetUpdate::default()
            },
        )
        .await
        .unwrap();
    client
        .update_assets(&ids, &AssetUpdate::default())
        .await
        .unwrap();
    let err = client
        .update_assets(
            &ids,
            &AssetUpdate {
                rating: Some(6),
                ..AssetUpdate::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ImmichError::Validation(_)), "{err:?}");
    client.delete_assets(&[], true).await.unwrap();
    assert_eq!(
        client.add_assets_to_album(ASSET, &[]).await.unwrap().len(),
        0
    );
    assert_eq!(client.tag_assets(&[], &ids).await.unwrap(), 0);
    assert!(requests(&server).await.is_empty());
}

fn offline_ids(count: u32) -> Vec<OfflineAssetId> {
    (0..count)
        .map(|n| {
            let asset = ImmichAsset {
                id: asset_id(n),
                is_offline: true,
                ..ImmichAsset::default()
            };
            OfflineAssetId::confirm(&asset).unwrap()
        })
        .collect()
}

#[tokio::test]
async fn deleting_sends_the_confirmed_offline_ids_and_the_force_flag() {
    let server = MockServer::start().await;
    mount(&server, "DELETE", "/api/assets", ResponseTemplate::new(204)).await;
    let client = keyed(&server);
    client.delete_assets(&offline_ids(2), false).await.unwrap();
    client.delete_assets(&offline_ids(501), true).await.unwrap();

    let sent = requests(&server).await;
    assert_eq!(sent.len(), 3);
    assert_eq!(
        body_of(&sent[0]),
        json!({ "ids": [asset_id(0), asset_id(1)], "force": false })
    );
    assert_eq!(body_of(&sent[1])["ids"].as_array().unwrap().len(), 500);
    assert_eq!(body_of(&sent[1])["force"], json!(true));
    assert_eq!(body_of(&sent[2])["ids"].as_array().unwrap().len(), 1);
    assert_eq!(body_of(&sent[2])["force"], json!(true));
}

#[test]
fn an_online_asset_cannot_be_turned_into_a_deletable_id() {
    let online = ImmichAsset {
        id: asset_id(1),
        is_offline: false,
        ..ImmichAsset::default()
    };
    assert!(OfflineAssetId::confirm(&online).is_none());
}

#[tokio::test]
async fn albums_of_an_asset_are_listed_by_asset_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/albums"))
        .and(query_param("assetId", ASSET))
        .respond_with(ok_json(json!([
            { "id": KEY_ID, "albumName": "Holiday", "assetCount": 3, "shared": false },
            { "id": LIBRARY, "albumName": "Best of" },
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let albums = keyed(&server).albums_for_asset(ASSET).await.unwrap();
    assert_eq!(
        albums
            .iter()
            .map(|album| album.album_name.as_str())
            .collect::<Vec<_>>(),
        ["Holiday", "Best of"]
    );
}

#[tokio::test]
async fn adding_to_an_album_reports_each_asset() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/albums/{KEY_ID}/assets")))
        .and(body_json(json!({ "ids": [asset_id(1), asset_id(2)] })))
        .respond_with(ok_json(json!([
            { "id": asset_id(1), "success": true },
            { "id": asset_id(2), "success": false, "error": "duplicate" },
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let results = keyed(&server)
        .add_assets_to_album(KEY_ID, &[asset_id(1), asset_id(2)])
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(BulkIdResult::is_ok));
}

#[tokio::test]
async fn tags_are_listed_and_attached_in_chunks() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/tags",
        ok_json(json!([
            { "id": KEY_ID, "name": "Family", "value": "People/Family", "createdAt": "x" },
        ])),
    )
    .await;
    mount(
        &server,
        "PUT",
        "/api/tags/assets",
        ok_json(json!({ "count": 3 })),
    )
    .await;
    let client = keyed(&server);
    let tags = client.tags().await.unwrap();
    assert_eq!(tags[0].value, "People/Family");

    let ids: Vec<String> = (0..600).map(asset_id).collect();
    let total = client.tag_assets(&[KEY_ID.to_owned()], &ids).await.unwrap();
    assert_eq!(total, 6);
    let sent = requests(&server).await;
    let tagging: Vec<&Received> = sent
        .iter()
        .filter(|request| request.url.path() == "/api/tags/assets")
        .collect();
    assert_eq!(tagging.len(), 2);
    assert_eq!(
        body_of(tagging[0])["assetIds"].as_array().unwrap().len(),
        500
    );
    assert_eq!(
        body_of(tagging[1])["assetIds"].as_array().unwrap().len(),
        100
    );
    assert_eq!(body_of(tagging[0])["tagIds"], json!([KEY_ID]));
}

// ----- search ---------------------------------------------------------------------------------

/// A server that follows the typed filter and pages with cursors.
fn cursor_server_answer(request: &Received) -> ResponseTemplate {
    let body = body_of(request);
    assert!(body.get("filter").is_some(), "{body}");
    match body.get("cursor").and_then(Value::as_str) {
        None => ok_json(search_answer(
            vec![
                asset_json(1, LIBRARY, "a.jpg"),
                asset_json(2, LIBRARY, "b.jpg"),
            ],
            Some("c2"),
            None,
        )),
        Some("c2") => ok_json(search_answer(
            vec![asset_json(3, LIBRARY, "c.jpg")],
            None,
            None,
        )),
        Some(_) => nest_error(400, json!("unknown cursor")),
    }
}

/// A server older than 3.2 that validates strictly: it refuses the typed fields and pages by number.
fn flat_server_answer(request: &Received) -> ResponseTemplate {
    let body = body_of(request);
    if body.get("filter").is_some() || body.get("orderBy").is_some() {
        return nest_error(
            400,
            json!([
                "property filter should not exist",
                "property orderBy should not exist"
            ]),
        );
    }
    match body["page"].as_u64() {
        Some(1) => ok_json(search_answer(
            vec![
                asset_json(1, LIBRARY, "a.jpg"),
                asset_json(2, LIBRARY, "b.jpg"),
            ],
            None,
            Some("2"),
        )),
        Some(2) => ok_json(search_answer(
            vec![asset_json(3, LIBRARY, "c.jpg")],
            None,
            None,
        )),
        _ => nest_error(400, json!("unknown page")),
    }
}

fn ids(from: u32, to: u32) -> Vec<String> {
    (from..=to).map(asset_id).collect()
}

#[tokio::test]
async fn a_known_new_server_gets_the_typed_filter_and_is_followed_through_cursors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/search/metadata"))
        .and(header("x-api-key", KEY))
        .respond_with(cursor_server_answer)
        .mount(&server)
        .await;
    let client = with_options(&server, knowing(Some(ImmichVersion::new(3, 4, 0))));
    assert!(client.supports_search_filter());
    let search = MetadataSearch {
        updated_after_ms: Some(1_714_557_599_000),
        ..MetadataSearch::in_library(LIBRARY)
    };
    assert_eq!(read_all(&client, search).await, ids(1, 3));

    let sent = requests(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(
        body_of(&sent[0])["filter"],
        json!({
            "libraryId": { "eq": LIBRARY },
            "trashedAt": { "eq": null },
            "updatedAt": { "gt": "2024-05-01T09:59:59.000Z" },
        })
    );
    assert!(body_of(&sent[0]).get("cursor").is_none());
    assert_eq!(body_of(&sent[1])["cursor"], json!("c2"));
    assert_eq!(client.health().reachable(), Some(true));
}

#[tokio::test]
async fn a_known_old_server_gets_the_flat_form_and_is_followed_through_page_numbers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/search/metadata"))
        .respond_with(flat_server_answer)
        .mount(&server)
        .await;
    let client = with_options(&server, knowing(Some(ImmichVersion::new(2, 5, 0))));
    assert!(!client.supports_search_filter());
    assert_eq!(
        read_all(&client, MetadataSearch::in_library(LIBRARY)).await,
        ids(1, 3)
    );
    let sent = requests(&server).await;
    assert_eq!(sent.len(), 2);
    let first = body_of(&sent[0]);
    assert!(first.get("filter").is_none());
    assert_eq!(first["libraryId"], json!(LIBRARY));
    assert_eq!(first["page"], json!(1));
    assert_eq!(body_of(&sent[1])["page"], json!(2));
}

#[tokio::test]
async fn an_unknown_version_tries_the_filter_first_and_keeps_it_when_accepted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/search/metadata"))
        .respond_with(cursor_server_answer)
        .mount(&server)
        .await;
    let client = keyed(&server);
    assert!(client.supports_search_filter());
    assert_eq!(
        read_all(&client, MetadataSearch::in_library(LIBRARY)).await,
        ids(1, 3)
    );
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn an_unknown_version_falls_back_once_when_the_filter_is_rejected_and_remembers_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/search/metadata"))
        .respond_with(flat_server_answer)
        .mount(&server)
        .await;
    let client = keyed(&server);
    assert_eq!(
        read_all(&client, MetadataSearch::in_library(LIBRARY)).await,
        ids(1, 3)
    );
    // One typed attempt that was refused, then the flat pages.
    let shapes: Vec<bool> = requests(&server)
        .await
        .iter()
        .map(|request| body_of(request).get("filter").is_some())
        .collect();
    assert_eq!(shapes, [true, false, false]);

    // The next search goes straight to the flat form.
    assert_eq!(
        read_all(&client, MetadataSearch::in_library(LIBRARY)).await,
        ids(1, 3)
    );
    let shapes: Vec<bool> = requests(&server)
        .await
        .iter()
        .map(|request| body_of(request).get("filter").is_some())
        .collect();
    assert_eq!(shapes, [true, false, false, false, false]);
}

#[tokio::test]
async fn a_server_that_silently_drops_the_filter_is_recognised_and_asked_again_flat() {
    let server = MockServer::start().await;
    // A lenient old server: it ignores the typed fields, so the first answer is the old paging
    // style with the assets of every library.
    Mock::given(method("POST"))
        .and(path("/api/search/metadata"))
        .respond_with(|request: &Received| {
            let body = body_of(request);
            if body.get("filter").is_some() {
                return ok_json(search_answer(
                    vec![
                        asset_json(1, LIBRARY, "a.jpg"),
                        asset_json(9, OTHER_LIBRARY, "z.jpg"),
                    ],
                    None,
                    Some("2"),
                ));
            }
            flat_server_answer(request)
        })
        .mount(&server)
        .await;
    let client = keyed(&server);
    assert_eq!(
        read_all(&client, MetadataSearch::in_library(LIBRARY)).await,
        ids(1, 3)
    );
}

#[tokio::test]
async fn a_failure_that_is_not_about_the_filter_is_not_retried_in_the_other_form() {
    for (status, answer) in [
        (500, nest_error(500, json!("Internal server error"))),
        (401, nest_error(401, json!("Invalid API key"))),
        (
            400,
            nest_error(400, json!("size must not be greater than 1000")),
        ),
    ] {
        let server = MockServer::start().await;
        mount(&server, "POST", "/api/search/metadata", answer).await;
        let client = keyed(&server);
        let err = client
            .search_metadata(&MetadataSearch::in_library(LIBRARY))
            .await
            .unwrap_err();
        match (status, &err) {
            (401, ImmichError::Unauthorized) => {}
            (500 | 400, ImmichError::Upstream { status: got, .. }) if *got == status => {}
            _ => panic!("{status}: {err:?}"),
        }
        assert_eq!(requests(&server).await.len(), 1, "{status}");
    }
}

#[tokio::test]
async fn when_both_forms_are_refused_the_typed_forms_complaint_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/search/metadata"))
        .respond_with(|request: &Received| {
            if body_of(request).get("filter").is_some() {
                nest_error(400, json!(["property filter should not exist"]))
            } else {
                nest_error(400, json!(["property whatever is invalid"]))
            }
        })
        .mount(&server)
        .await;
    let err = keyed(&server)
        .search_metadata(&MetadataSearch::in_library(LIBRARY))
        .await
        .unwrap_err();
    let ImmichError::Upstream {
        status: 400,
        message,
    } = &err
    else {
        panic!("{err:?}")
    };
    assert!(message.contains("filter should not exist"), "{message}");
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn a_page_token_decides_the_form_so_a_scan_never_switches_midway() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/search/metadata"))
        .respond_with(flat_server_answer)
        .mount(&server)
        .await;
    let client = keyed(&server);

    let flat = client
        .search_metadata(&MetadataSearch {
            after: Some(PageToken::Page(2)),
            ..MetadataSearch::in_library(LIBRARY)
        })
        .await
        .unwrap();
    assert_eq!(flat.items.len(), 1);
    assert_eq!(flat.next, None);

    let err = client
        .search_metadata(&MetadataSearch {
            after: Some(PageToken::Cursor("c2".into())),
            ..MetadataSearch::in_library(LIBRARY)
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, ImmichError::Upstream { status: 400, .. }),
        "{err:?}"
    );
    let sent = requests(&server).await;
    assert_eq!(sent.len(), 2, "no fallback after a token");
    assert!(body_of(&sent[0]).get("filter").is_none());
    assert!(body_of(&sent[1]).get("filter").is_some());
}

#[tokio::test]
async fn assets_of_another_library_are_refused_on_a_server_known_to_be_new() {
    let server = MockServer::start().await;
    mount(
        &server,
        "POST",
        "/api/search/metadata",
        ok_json(search_answer(
            vec![
                asset_json(1, LIBRARY, "a.jpg"),
                asset_json(2, OTHER_LIBRARY, "b.jpg"),
            ],
            None,
            None,
        )),
    )
    .await;
    let client = with_options(&server, knowing(Some(ImmichVersion::new(3, 4, 0))));
    let err = client
        .search_metadata(&MetadataSearch::in_library(LIBRARY))
        .await
        .unwrap_err();
    assert!(matches!(err, ImmichError::Protocol(_)), "{err:?}");
    assert_eq!(client.health().reachable(), Some(false));
}

#[tokio::test]
async fn the_search_filters_the_answer_by_its_documented_semantics() {
    let server = MockServer::start().await;
    let mut trashed = asset_json(2, LIBRARY, "t.jpg");
    trashed["deletedAt"] = json!("2024-05-02T00:00:00.000Z");
    let mut offline = asset_json(3, LIBRARY, "o.jpg");
    offline["isOffline"] = json!(true);
    offline["deletedAt"] = json!("2024-05-02T00:00:00.000Z");
    mount(
        &server,
        "POST",
        "/api/search/metadata",
        ok_json(search_answer(
            vec![asset_json(1, LIBRARY, "a.jpg"), trashed, offline],
            None,
            None,
        )),
    )
    .await;
    let client = with_options(&server, knowing(Some(ImmichVersion::new(3, 4, 0))));

    let live = client
        .search_metadata(&MetadataSearch::in_library(LIBRARY))
        .await
        .unwrap();
    assert_eq!(live.items.len(), 1);

    let gone = client
        .search_metadata(&MetadataSearch {
            include_trashed: true,
            offline: OfflineFilter::OnlyOffline,
            ..MetadataSearch::in_library(LIBRARY)
        })
        .await
        .unwrap();
    assert_eq!(gone.items.len(), 1);
    assert_eq!(gone.items[0].id, asset_id(3));
    assert!(OfflineAssetId::confirm(&gone.items[0]).is_some());
}

// ----- failures ---------------------------------------------------------------------------------

async fn failure_for(status: u16, answer: ResponseTemplate) -> ImmichError {
    let server = MockServer::start().await;
    mount(&server, "GET", "/api/users/me", answer).await;
    let _ = status;
    keyed(&server).me().await.unwrap_err()
}

#[tokio::test]
async fn statuses_map_to_typed_errors() {
    assert!(matches!(
        failure_for(401, nest_error(401, json!("Invalid API key"))).await,
        ImmichError::Unauthorized
    ));
    assert!(matches!(
        failure_for(403, nest_error(403, json!("Missing required permission"))).await,
        ImmichError::Forbidden
    ));
    assert!(matches!(
        failure_for(404, nest_error(404, json!("Not found"))).await,
        ImmichError::NotFound
    ));

    let ImmichError::Upstream { status, message } = failure_for(
        400,
        nest_error(
            400,
            json!([
                "size must not be greater than 1000",
                "page must be a positive number"
            ]),
        ),
    )
    .await
    else {
        panic!("not an upstream error")
    };
    assert_eq!(status, 400);
    assert_eq!(
        message,
        "size must not be greater than 1000; page must be a positive number"
    );

    for code in [429, 500, 502, 503] {
        let err = failure_for(code, nest_error(code, json!("try later"))).await;
        assert!(
            matches!(err, ImmichError::Upstream { status, .. } if status == code),
            "{err:?}"
        );
        assert!(err.is_retryable(), "{code}");
    }
    let err = failure_for(400, nest_error(400, json!("bad"))).await;
    assert!(!err.is_retryable());
}

#[tokio::test]
async fn upstream_texts_are_scrubbed_flattened_and_shortened() {
    let noisy = format!(
        "boom\n  at https://user:hunter2@evil.example/p?token=s3cret&x=1 {}",
        "x".repeat(2000)
    );
    let ImmichError::Upstream { message, .. } =
        failure_for(500, nest_error(500, json!(noisy))).await
    else {
        panic!("not an upstream error")
    };
    assert!(!message.contains("hunter2"), "{message}");
    assert!(!message.contains("s3cret"), "{message}");
    assert!(!message.contains('\n'));
    assert!(message.chars().count() <= MAX_UPSTREAM_MESSAGE_CHARS);
    assert!(
        message.starts_with("boom at https://evil.example/p"),
        "{message}"
    );
}

#[tokio::test]
async fn html_and_empty_error_pages_do_not_leak_into_messages() {
    let html = ResponseTemplate::new(502)
        .set_body_raw("<html><body>nginx secret-banner</body></html>", "text/html");
    let ImmichError::Upstream { message, .. } = failure_for(502, html).await else {
        panic!("not an upstream error")
    };
    assert_eq!(message, "Bad Gateway");

    let ImmichError::Upstream { message, .. } = failure_for(503, ResponseTemplate::new(503)).await
    else {
        panic!("not an upstream error")
    };
    assert_eq!(message, "Service Unavailable");

    let plain = ResponseTemplate::new(500).set_body_raw("upstream timed out", "text/plain");
    let ImmichError::Upstream { message, .. } = failure_for(500, plain).await else {
        panic!("not an upstream error")
    };
    assert_eq!(message, "upstream timed out");
}

#[tokio::test]
async fn an_answer_that_is_not_json_is_a_protocol_error_that_does_not_quote_it() {
    let page = ResponseTemplate::new(200).set_body_raw(
        "<html><body>router secret-banner</body></html>",
        "text/html",
    );
    let server = MockServer::start().await;
    mount(&server, "GET", "/api/users/me", page).await;
    let client = keyed(&server);
    let err = client.me().await.unwrap_err();
    let ImmichError::Protocol(text) = &err else {
        panic!("{err:?}")
    };
    assert!(
        text.starts_with("unexpected response from GET /users/me"),
        "{text}"
    );
    assert!(!text.contains("secret-banner"));
    assert_eq!(client.health().reachable(), Some(false));
}

async fn redirect_error(base_path: &str, location: Option<String>) -> ImmichError {
    let origin = MockServer::start().await;
    let mut answer = ResponseTemplate::new(301);
    if let Some(location) = location {
        answer = answer.insert_header("location", location.as_str());
    }
    mount(&origin, "GET", &format!("{base_path}/api/users/me"), answer).await;
    let client = ImmichClient::new(
        &format!("{}{base_path}", origin.uri()),
        Some(secret(KEY)),
        ClientOptions::default(),
    )
    .unwrap();
    client.me().await.unwrap_err()
}

#[tokio::test]
async fn a_redirect_is_never_followed_and_names_the_address_to_use() {
    let target = MockServer::start().await;
    let origin = MockServer::start().await;
    mount(
        &origin,
        "GET",
        "/api/users/me",
        ResponseTemplate::new(302).insert_header(
            "location",
            format!("{}/api/users/me?token=zzz#frag", target.uri()).as_str(),
        ),
    )
    .await;
    let err = keyed(&origin).me().await.unwrap_err();
    let ImmichError::Redirected { to } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(to, &target.uri());
    assert!(requests(&target).await.is_empty(), "the key was sent on");
    assert_eq!(requests(&origin).await.len(), 1);

    for (location, expected) in [
        (
            "https://immich.example.com/api/users/me",
            "https://immich.example.com",
        ),
        (
            "https://u:pw@immich.example.com:8443/api/users/me?a=b",
            "https://immich.example.com:8443",
        ),
        (
            "https://immich.example.com/login",
            "https://immich.example.com/login",
        ),
    ] {
        let err = redirect_error("", Some(location.to_owned())).await;
        let ImmichError::Redirected { to } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(to, expected, "{location}");
    }

    let err = redirect_error(
        "/photos",
        Some("https://x.example/immich/api/users/me".into()),
    )
    .await;
    assert!(
        matches!(&err, ImmichError::Redirected { to } if to == "https://x.example/immich"),
        "{err:?}"
    );

    let err = redirect_error("", None).await;
    assert!(
        matches!(&err, ImmichError::Redirected { to } if to == "another address"),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_closed_port_is_unreachable_without_leaking_the_address_or_userinfo() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let client = ImmichClient::new(
        &format!("http://user:pw-secret@127.0.0.1:{port}"),
        Some(secret(KEY)),
        ClientOptions::default(),
    )
    .unwrap();
    let err = client.me().await.unwrap_err();
    let ImmichError::Unreachable(text) = &err else {
        panic!("{err:?}")
    };
    assert!(!text.contains("pw-secret"), "{text}");
    assert!(!text.contains("http://"), "{text}");
    assert!(err.is_retryable());
    assert_eq!(client.health().reachable(), Some(false));
    assert!(client.health().last_error.is_some());
}

#[tokio::test]
async fn a_slow_server_hits_the_request_timeout() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/api/users/me",
        ok_json(json!({ "id": ASSET })).set_delay(Duration::from_secs(5)),
    )
    .await;
    let client = with_options(
        &server,
        ClientOptions {
            request_timeout: Duration::from_millis(200),
            ..ClientOptions::default()
        },
    );
    let started = Instant::now();
    let err = client.me().await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        matches!(&err, ImmichError::Unreachable(text) if text.contains("timed out")),
        "{err:?}"
    );
    assert!(err.is_retryable());
}

#[tokio::test]
async fn tls_against_a_plain_http_server_is_unreachable_and_nameless() {
    let server = MockServer::start().await;
    let address = server.address();
    let client = ImmichClient::new(
        &format!("https://{address}"),
        Some(secret(KEY)),
        ClientOptions {
            allow_insecure_tls: true,
            request_timeout: Duration::from_secs(5),
            ..ClientOptions::default()
        },
    )
    .unwrap();
    let err = client.me().await.unwrap_err();
    let ImmichError::Unreachable(text) = &err else {
        panic!("{err:?}")
    };
    assert!(!text.contains("https://"), "{text}");
}

#[tokio::test]
async fn a_server_that_does_not_speak_http_is_an_error_not_a_hang() {
    let base = raw_server(b"SSH-2.0-OpenSSH_9.6\r\n".to_vec()).await;
    let client = ImmichClient::new(&base, Some(secret(KEY)), ClientOptions::default()).unwrap();
    let err = client.me().await.unwrap_err();
    assert!(matches!(err, ImmichError::Unreachable(_)), "{err:?}");
}

#[tokio::test]
async fn answers_are_bounded() {
    let server = MockServer::start().await;
    mount(
        &server,
        "GET",
        "/big",
        ResponseTemplate::new(200).set_body_bytes(vec![b'a'; 2048]),
    )
    .await;
    let get = || reqwest::get(format!("{}/big", server.uri()));
    let err = read_body(get().await.unwrap(), 1024).await.unwrap_err();
    assert!(matches!(err, ImmichError::Protocol(_)), "{err:?}");
    assert_eq!(
        read_body(get().await.unwrap(), 4096).await.unwrap().len(),
        2048
    );
    assert_eq!(read_capped(get().await.unwrap(), 10).await.len(), 10);
    assert_eq!(read_capped(get().await.unwrap(), 5000).await.len(), 2048);

    // No content length to check up front: the limit applies while reading.
    let chunk = "258\r\n".to_owned() + &"b".repeat(600) + "\r\n";
    let chunked = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n{chunk}{chunk}0\r\n\r\n"
    );
    let base = raw_server(chunked.clone().into_bytes()).await;
    let response = reqwest::get(format!("{base}/x")).await.unwrap();
    assert!(response.content_length().is_none());
    let err = read_body(response, 1024).await.unwrap_err();
    assert!(matches!(err, ImmichError::Protocol(_)), "{err:?}");
    let base = raw_server(chunked.into_bytes()).await;
    let response = reqwest::get(format!("{base}/x")).await.unwrap();
    assert_eq!(read_body(response, 2048).await.unwrap().len(), 1200);
}

#[tokio::test]
async fn health_follows_the_latest_outcome() {
    let server = MockServer::start().await;
    let client = keyed(&server);
    assert_eq!(client.health(), Health::default());
    assert_eq!(client.health().reachable(), None);

    mount(
        &server,
        "GET",
        "/api/users/me",
        ok_json(json!({ "id": ASSET })),
    )
    .await;
    client.me().await.unwrap();
    let health = client.health();
    assert_eq!(health.reachable(), Some(true));
    assert!(health.last_ok_at.is_some());
    assert_eq!(health.last_error, None);

    // A server that answers a refusal is still a working connection.
    assert!(client.about().await.is_err());
    assert_eq!(client.health().reachable(), Some(true));

    server.reset().await;
    mount(
        &server,
        "GET",
        "/api/users/me",
        nest_error(500, json!("down")),
    )
    .await;
    client.me().await.unwrap_err();
    let health = client.health();
    assert_eq!(health.reachable(), Some(false));
    assert!(health.last_error.as_deref().unwrap().contains("500"));
    assert!(health.last_ok_at.is_some(), "the last success stays known");

    server.reset().await;
    mount(
        &server,
        "GET",
        "/api/users/me",
        nest_error(401, json!("revoked")),
    )
    .await;
    assert!(matches!(client.me().await, Err(ImmichError::Unauthorized)));
    assert_eq!(client.health().reachable(), Some(false));
    assert!(client.health().key_rejected);

    // Another kind of trouble does not make the revoked key valid again.
    server.reset().await;
    mount(
        &server,
        "GET",
        "/api/users/me",
        nest_error(500, json!("down")),
    )
    .await;
    client.me().await.unwrap_err();
    assert!(client.health().key_rejected);

    server.reset().await;
    mount(
        &server,
        "GET",
        "/api/users/me",
        ok_json(json!({ "id": ASSET })),
    )
    .await;
    client.me().await.unwrap();
    assert_eq!(client.health().reachable(), Some(true));
    assert_eq!(client.health().last_error, None);
    assert!(!client.health().key_rejected);
}

#[tokio::test]
async fn a_rejected_login_is_not_a_rejected_key() {
    let server = MockServer::start().await;
    mount(
        &server,
        "POST",
        "/api/auth/login",
        nest_error(401, json!("Incorrect email or password")),
    )
    .await;
    let client = anonymous(&server);
    let err = client.login("a@b.c", &secret(PASSWORD)).await.unwrap_err();
    assert!(matches!(err, ImmichError::AuthFailed), "{err:?}");
    assert!(!client.health().key_rejected);
}

#[test]
fn defaults_are_bounded_and_secure() {
    let options = ClientOptions::default();
    assert_eq!(options.connect_timeout, Duration::from_secs(10));
    assert_eq!(options.request_timeout, Duration::from_secs(60));
    assert!(!options.allow_insecure_tls);
    assert_eq!(options.server_version, None);
    assert!(ImmichClient::new("http://h", None, options).is_ok());
}
