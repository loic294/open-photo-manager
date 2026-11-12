//! Connecting to Immich and disconnecting again, through `POST /api/immich/connect` and
//! `DELETE /api/immich/connection`, against the stateful fake Immich of `immich_support`.
//!
//! The fake has real sessions and API keys, so these tests check what the calls do and not only
//! that they were made: that no key is left behind in Immich when a connect fails, that the
//! password is used once, and that every credential is used the way Immich wants it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use axum::http::StatusCode;
use immich_support::{Call, EMAIL, FakeImmich, Fixture, USER_NAME, VERSION, problem_fields};
use serde_json::{Value, json};

/// What the key asks to be allowed. A literal list on purpose: a change to the permissions of the
/// key changes what the user trusts the application with, so it must show in a test.
const PERMISSIONS: [&str; 12] = [
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
];

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}

/// The requests that deleted an API key.
fn key_deletions(fake: &FakeImmich) -> Vec<Call> {
    fake.calls()
        .into_iter()
        .filter(|call| call.method == "DELETE" && call.path.starts_with("/api/api-keys/"))
        .collect()
}

#[tokio::test]
async fn an_administrator_connects_and_only_the_key_stays_in_immich() {
    let fx = Fixture::new().await;
    let password = fx.fake.password();

    let (status, connection) = fx.connect().await;
    assert_eq!(status, StatusCode::OK, "{connection}");

    assert_eq!(connection["server_url"], fx.fake.url());
    assert_eq!(connection["email"], EMAIL);
    assert_eq!(connection["user_name"], USER_NAME);
    assert_eq!(connection["is_admin"], true);
    assert_eq!(connection["api_key_name"], "Open Photo Manager");
    assert_eq!(connection["allow_insecure_tls"], false);
    assert_eq!(connection["server_version"], VERSION);
    assert!(connection["connected_at"].as_i64().unwrap() > 0);
    assert_eq!(connection["last_ok_at"], connection["connected_at"]);
    assert_eq!(connection["last_error"], Value::Null);

    assert_eq!(
        fx.fake.sequence(),
        [
            "POST /api/auth/login",
            "POST /api/api-keys",
            "GET /api/users/me",
            "GET /api/server/about",
            "POST /api/auth/logout",
        ]
    );
    let calls = fx.fake.calls();

    // The password goes to the login and nowhere else, and the login sends no credential.
    assert_eq!(
        calls[0].body,
        Some(json!({ "email": EMAIL, "password": password }))
    );
    assert!(calls[0].authorization.is_none() && calls[0].api_key.is_none());
    for call in &calls[1..] {
        assert!(
            !call
                .body
                .as_ref()
                .is_some_and(|body| body.to_string().contains(&password)),
            "the password was sent again to {}",
            call.path
        );
    }

    // The session creates the key, with exactly the permissions of the list above, and ends.
    let session = calls[1]
        .bearer()
        .expect("the key is created by the session");
    assert!(fx.fake.secrets().iter().any(|secret| secret == session));
    assert!(calls[1].api_key.is_none());
    assert_eq!(
        calls[1].body,
        Some(json!({ "name": "Open Photo Manager", "permissions": PERMISSIONS }))
    );
    assert_eq!(calls[4].bearer(), Some(session));
    assert_eq!(fx.fake.session_count(), 0);

    // The key itself proves the account: no session is used after the key exists.
    let keys = fx.fake.keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name, "Open Photo Manager");
    assert_eq!(keys[0].permissions, PERMISSIONS);
    for call in &calls[2..4] {
        assert_eq!(call.api_key.as_deref(), Some(keys[0].secret.as_str()));
        assert!(call.authorization.is_none());
    }

    let status = fx.status(false).await;
    assert_eq!(status["connected"], true);
    assert_eq!(status["mode"], "full");
    assert_eq!(status["connection"], connection);
    fx.assert_no_leak();
}

#[tokio::test]
async fn an_account_that_is_not_an_administrator_connects_in_limited_mode() {
    let fx = Fixture::limited().await;

    let connection = fx.connect_ok().await;

    assert_eq!(connection["is_admin"], false);
    let status = fx.status(false).await;
    assert_eq!(status["connected"], true);
    assert_eq!(status["mode"], "limited");
    assert_eq!(fx.fake.key_count(), 1);
    assert_eq!(fx.fake.session_count(), 0);
    fx.assert_no_leak();
}

#[tokio::test]
async fn a_pasted_address_is_cleaned_before_anything_is_sent_or_stored() {
    let fx = Fixture::new().await;
    fx.watch("pasted-secret");
    let pasted = format!(
        "{}/api/?token=pasted-secret#top",
        fx.fake
            .url()
            .replacen("http://", "http://someone:pasted-secret@", 1)
    );

    let (status, connection) = fx
        .post(
            "/api/immich/connect",
            json!({ "server_url": pasted, "email": EMAIL, "password": fx.fake.password() }),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{connection}");
    assert_eq!(connection["server_url"], fx.fake.url());
    for call in fx.fake.calls() {
        assert!(call.path.starts_with("/api/"), "{}", call.path);
        if call.path == "/api/auth/login" {
            assert!(call.authorization.is_none(), "the address carried a login");
        }
    }
    fx.assert_no_leak();
}

#[tokio::test]
async fn a_wrong_password_is_refused_before_anything_is_created() {
    let fx = Fixture::new().await;
    let wrong = "not-the-password-4711";
    fx.watch(wrong);

    let (status, body) = fx
        .post(
            "/api/immich/connect",
            json!({ "server_url": fx.fake.url(), "email": EMAIL, "password": wrong }),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "immich_auth_failed");
    assert_eq!(fx.fake.sequence(), ["POST /api/auth/login"]);
    assert_eq!(fx.fake.key_count(), 0);
    assert_eq!(fx.fake.session_count(), 0);
    assert_eq!(fx.status(false).await["connected"], false);
    assert!(
        fx.log().contains("POST /auth/login"),
        "the log must show the login was attempted: {}",
        fx.log()
    );
    fx.assert_no_leak();
}

#[tokio::test]
async fn a_failing_login_is_a_bad_gateway_and_stores_nothing() {
    let fx = Fixture::new().await;
    fx.fake.fail("POST", "/api/auth/login", 503);

    let (status, body) = fx.connect().await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(error_code(&body), "immich_error");
    assert_eq!(fx.fake.sequence(), ["POST /api/auth/login"]);
    assert_eq!(fx.status(false).await["connected"], false);
}

#[tokio::test]
async fn a_failure_after_the_key_exists_revokes_it_and_keeps_nothing() {
    let fx = Fixture::new().await;
    fx.fake.fail("GET", "/api/users/me", 500);

    let (status, body) = fx.connect().await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(error_code(&body), "immich_error");
    assert_eq!(
        fx.fake.key_count(),
        0,
        "the key that was created is revoked"
    );
    assert_eq!(fx.fake.session_count(), 0, "the session ends");
    let deletions = key_deletions(&fx.fake);
    assert_eq!(deletions.len(), 1);
    assert!(deletions[0].bearer().is_some() && deletions[0].api_key.is_none());
    assert_eq!(fx.fake.count("POST", "/api/auth/logout"), 1);
    assert_eq!(fx.status(false).await["connected"], false);
    fx.assert_no_leak();
}

#[tokio::test]
async fn the_session_ends_only_after_the_key_is_revoked() {
    let fx = Fixture::new().await;
    fx.fake.fail("GET", "/api/server/about", 500);

    let (status, _) = fx.connect().await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let sequence = fx.fake.sequence();
    let revoked = sequence
        .iter()
        .position(|call| call.starts_with("DELETE /api/api-keys/"))
        .expect("the key was revoked");
    let logged_out = sequence
        .iter()
        .position(|call| call == "POST /api/auth/logout")
        .expect("the session ended");
    assert!(revoked < logged_out, "{sequence:?}");
    assert_eq!(fx.fake.key_count(), 0);
}

#[tokio::test]
async fn a_key_that_cannot_be_revoked_is_neither_stored_nor_leaked() {
    let fx = Fixture::new().await;
    fx.fake.fail("GET", "/api/users/me", 500);
    fx.fake.fail("DELETE", "/api/api-keys/*", 500);

    let (status, body) = fx.connect().await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(fx.fake.key_count(), 1, "Immich still has the key");
    assert_eq!(fx.status(false).await["connected"], false);
    assert!(
        fx.log()
            .contains("revoking the API key of the failed connection failed"),
        "the log must say the key was left behind: {}",
        fx.log()
    );
    fx.assert_no_leak();
}

#[tokio::test]
async fn reconnecting_replaces_the_connection_and_revokes_the_old_key() {
    let fx = Fixture::connected().await;
    let first = fx.fake.keys().remove(0);
    fx.fake.clear_calls();

    let second_connection = fx.connect_ok().await;

    let keys = fx.fake.keys();
    assert_eq!(keys.len(), 1, "only the new key is left in Immich");
    assert_ne!(keys[0].id, first.id);
    assert_eq!(fx.fake.session_count(), 0);

    // The old key revokes itself; ending the session and revoking it do not wait for each other.
    let sequence = fx.fake.sequence();
    assert_eq!(
        sequence[..4],
        [
            "POST /api/auth/login",
            "POST /api/api-keys",
            "GET /api/users/me",
            "GET /api/server/about",
        ]
    );
    let mut cleanup = sequence[4..].to_vec();
    cleanup.sort();
    assert_eq!(
        cleanup,
        [
            format!("DELETE /api/api-keys/{}", first.id),
            "POST /api/auth/logout".to_owned(),
        ]
    );
    let deletions = key_deletions(&fx.fake);
    assert_eq!(deletions[0].api_key.as_deref(), Some(first.secret.as_str()));

    // The new connection is the one in use.
    fx.fake.clear_calls();
    let status = fx.status(true).await;
    assert_eq!(status["reachable"], true);
    assert_eq!(
        status["connection"]["connected_at"],
        second_connection["connected_at"]
    );
    let probes = fx.fake.calls_to("GET", "/api/server/about");
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].api_key.as_deref(), Some(keys[0].secret.as_str()));
    fx.assert_no_leak();
}

#[tokio::test]
async fn a_failed_reconnect_leaves_the_previous_connection_working() {
    let fx = Fixture::connected().await;
    let first = fx.fake.keys().remove(0);
    let connected_at = fx.status(false).await["connection"]["connected_at"].clone();

    // A wrong password.
    let wrong = "still-not-the-password-1";
    fx.watch(wrong);
    let (status, body) = fx
        .post(
            "/api/immich/connect",
            json!({ "server_url": fx.fake.url(), "email": EMAIL, "password": wrong }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A failure once the new key exists.
    fx.fake.fail("GET", "/api/users/me", 500);
    let (status, body) = fx.connect().await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    fx.fake.heal();

    let keys = fx.fake.keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].id, first.id, "the previous key was not revoked");
    let status = fx.status(true).await;
    assert_eq!(status["reachable"], true);
    assert_eq!(status["needs_reconnect"], false);
    assert_eq!(status["connection"]["connected_at"], connected_at);
    fx.assert_no_leak();
}

#[tokio::test]
async fn a_request_with_missing_fields_names_them_and_contacts_nobody() {
    let fx = Fixture::new().await;

    let (status, body) = fx
        .post(
            "/api/immich/connect",
            json!({ "server_url": "  ", "email": "not-an-email", "password": "" }),
        )
        .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error_code(&body), "validation_failed");
    assert_eq!(problem_fields(&body), ["server_url", "email", "password"]);
    assert!(fx.fake.calls().is_empty());
}

#[tokio::test]
async fn an_address_that_is_not_http_is_refused_without_repeating_it() {
    let fx = Fixture::new().await;
    fx.watch("hidden-userinfo-pass");

    let (status, body) = fx
        .post(
            "/api/immich/connect",
            json!({
                "server_url": "ftp://someone:hidden-userinfo-pass@immich.example",
                "email": EMAIL,
                "password": fx.fake.password(),
            }),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "immich_invalid_url");
    assert!(fx.fake.calls().is_empty());
    fx.assert_no_leak();
}

#[tokio::test]
async fn an_unreachable_server_is_a_bad_gateway_and_stores_nothing() {
    let mut fx = Fixture::new().await;
    let credentials = fx.credentials();
    fx.fake.stop().await;

    let (status, body) = fx.post("/api/immich/connect", credentials).await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(error_code(&body), "immich_unreachable");
    assert_eq!(fx.status(false).await["connected"], false);
}

#[tokio::test]
async fn disconnecting_revokes_the_key_and_forgets_the_connection() {
    let fx = Fixture::connected().await;
    let key = fx.fake.keys().remove(0);
    fx.fake.clear_calls();

    let (status, body) = fx.delete("/api/immich/connection").await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "revoked": true, "message": null }));
    assert_eq!(
        fx.fake.sequence(),
        [format!("DELETE /api/api-keys/{}", key.id)]
    );
    assert_eq!(
        fx.fake.calls()[0].api_key.as_deref(),
        Some(key.secret.as_str())
    );
    assert_eq!(fx.fake.key_count(), 0);
    let status = fx.status(false).await;
    assert_eq!(status["connected"], false);
    assert_eq!(status["mode"], "disconnected");

    let (status, body) = fx.delete("/api/immich/connection").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "immich_not_connected");

    // Connecting again starts from scratch.
    fx.connect_ok().await;
    assert_eq!(fx.fake.key_count(), 1);
    fx.assert_no_leak();
}

#[tokio::test]
async fn disconnecting_when_immich_refuses_still_forgets_the_connection() {
    let fx = Fixture::connected().await;
    fx.fake.fail("DELETE", "/api/api-keys/*", 500);

    let (status, body) = fx.delete("/api/immich/connection").await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], false);
    let message = body["message"].as_str().expect("the user is told why");
    assert!(message.contains("500"), "{message}");
    assert_eq!(fx.fake.key_count(), 1, "the key is still in Immich");
    assert_eq!(fx.status(false).await["connected"], false);
}

#[tokio::test]
async fn disconnecting_when_immich_is_down_still_forgets_the_connection() {
    let mut fx = Fixture::connected().await;
    fx.fake.stop().await;

    let (status, body) = fx.delete("/api/immich/connection").await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revoked"], false);
    assert!(!body["message"].as_str().unwrap_or_default().is_empty());
    assert_eq!(fx.status(false).await["connected"], false);
}

#[tokio::test]
async fn a_key_that_was_deleted_in_immich_counts_as_revoked() {
    let fx = Fixture::connected().await;
    fx.fake.revoke_all_keys();

    let (status, body) = fx.delete("/api/immich/connection").await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "revoked": true, "message": null }));
    assert_eq!(fx.status(false).await["connected"], false);
}

#[tokio::test]
async fn two_connects_at_once_leave_exactly_one_key() {
    let fx = Fixture::new().await;

    let (first, second) = tokio::join!(fx.connect(), fx.connect());

    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    assert_eq!(fx.fake.key_count(), 1);
    assert_eq!(fx.fake.session_count(), 0);
    let status = fx.status(true).await;
    assert_eq!(status["reachable"], true);
    assert_eq!(status["needs_reconnect"], false);
    fx.assert_no_leak();
}
