//! What `GET /api/immich/status` says about the connection, and how the connection holds up when
//! things go wrong: a key Immich revoked, a server that is down, a key file that is gone, a
//! restart. Also that nothing secret is kept in the clear. All against the stateful fake Immich of
//! `immich_support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod immich_support;

use std::sync::Arc;

use apm_core::dto::ImmichMode;
use apm_server::immich::crypto::key_path;
use apm_server::immich::{ImmichError, ImmichService};
use axum::http::StatusCode;
use immich_support::{EMAIL, Fixture, VERSION, files_containing};
use serde_json::{Value, json};

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn without_a_connection_the_status_says_so_and_asks_nobody() {
    let fx = Fixture::new().await;
    let expected = json!({
        "connected": false,
        "mode": "disconnected",
        "connection": null,
        "reachable": null,
        "needs_reconnect": false,
        "server_version": null,
        "version_warning": null,
        "mappings": [],
        "last_sync_at": null,
        "sync": null,
        "outbox_pending": 0,
        "needs_attention": 0,
    });

    assert_eq!(fx.status(false).await, expected);
    assert_eq!(fx.status(true).await, expected);
    assert!(fx.fake.calls().is_empty());
}

#[tokio::test]
async fn a_status_without_a_check_makes_no_request() {
    let fx = Fixture::connected().await;
    fx.fake.clear_calls();

    let status = fx.status(false).await;

    assert!(fx.fake.calls().is_empty());
    assert_eq!(status["connected"], true);
    assert_eq!(status["mode"], "full");
    assert_eq!(status["reachable"], true);
    assert_eq!(status["needs_reconnect"], false);
    assert_eq!(status["server_version"], VERSION);
    assert_eq!(status["version_warning"], Value::Null);
    assert_eq!(status["connection"]["server_url"], fx.fake.url());
    assert_eq!(status["connection"]["last_error"], Value::Null);
    assert_eq!(status["mappings"], json!([]));
    assert_eq!(status["last_sync_at"], Value::Null);
    assert_eq!(status["sync"], Value::Null);
    assert_eq!(status["outbox_pending"], 0);
    assert_eq!(status["needs_attention"], 0);
}

#[tokio::test]
async fn a_checked_status_asks_immich_once_and_with_the_key_alone() {
    let fx = Fixture::connected().await;
    let key = fx.fake.keys().remove(0).secret;
    fx.fake.clear_calls();

    let status = fx.status(true).await;

    let calls = fx.fake.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].is("GET", "/api/server/about"));
    assert_eq!(calls[0].api_key.as_deref(), Some(key.as_str()));
    assert!(calls[0].authorization.is_none());
    assert_eq!(status["reachable"], true);
    assert_eq!(status["needs_reconnect"], false);
    assert!(
        status["connection"]["last_ok_at"].as_i64().unwrap()
            >= status["connection"]["connected_at"].as_i64().unwrap()
    );
}

#[tokio::test]
async fn the_version_of_the_server_is_followed_and_judged() {
    let fx = Fixture::connected().await;
    // What Immich reports and the word its warning has to contain, if any.
    let cases = [
        ("v3.5.1", None),
        ("v2.0.0", None),
        ("v1.130.2", Some("older")),
        ("v4.0.0", Some("newer")),
        (VERSION, None),
    ];

    for (version, warning) in cases {
        fx.fake.state().version = version.to_owned();

        let status = fx.status(true).await;

        assert_eq!(status["server_version"], version);
        assert_eq!(status["connection"]["server_version"], version);
        match warning {
            None => assert_eq!(status["version_warning"], Value::Null, "{version}"),
            Some(word) => {
                let text = status["version_warning"].as_str().unwrap_or_default();
                assert!(text.contains(word), "{version}: {text:?}");
            }
        }

        // The version is remembered: the next status needs no question to know it.
        let remembered = fx.status(false).await;
        assert_eq!(remembered["server_version"], version);
        assert_eq!(remembered["version_warning"], status["version_warning"]);
    }
}

#[tokio::test]
async fn a_revoked_key_asks_for_a_new_connection_and_connecting_again_heals_it() {
    let fx = Fixture::connected().await;
    fx.fake.revoke_all_keys();

    let status = fx.status(true).await;

    assert_eq!(status["connected"], true);
    assert_eq!(status["needs_reconnect"], true);
    // Immich answered, it is the key it refuses.
    assert_eq!(status["reachable"], true);
    assert!(status["connection"]["last_error"].is_string());

    // That is remembered and costs no new question.
    fx.fake.clear_calls();
    let remembered = fx.status(false).await;
    assert_eq!(remembered["needs_reconnect"], true);
    assert!(fx.fake.calls().is_empty());

    let (code, body) = fx.get("/api/immich/libraries").await;
    assert_eq!(code, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(error_code(&body), "immich_unauthorized");

    // A key that is gone already is not a problem when it is replaced.
    fx.connect_ok().await;
    let healed = fx.status(true).await;
    assert_eq!(healed["needs_reconnect"], false);
    assert_eq!(healed["reachable"], true);
    assert_eq!(healed["connection"]["last_error"], Value::Null);
    let (code, body) = fx.get("/api/immich/libraries").await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(fx.fake.key_count(), 1);
    assert!(!fx.log().contains("replaced Immich connection failed"));
    fx.assert_no_leak();
}

#[tokio::test]
async fn a_server_that_is_down_is_reported_and_remembered_not_failed() {
    let mut fx = Fixture::connected().await;
    fx.fake.stop().await;

    let status = fx.status(true).await;

    assert_eq!(status["connected"], true);
    assert_eq!(status["reachable"], false);
    assert_eq!(status["needs_reconnect"], false);
    let reason = status["connection"]["last_error"].as_str().unwrap();
    assert!(!reason.is_empty());

    // The next status knows without asking again.
    let remembered = fx.status(false).await;
    assert_eq!(remembered["reachable"], false);
    assert_eq!(
        remembered["connection"]["last_error"],
        status["connection"]["last_error"]
    );
}

#[tokio::test]
async fn a_server_that_recovers_is_reachable_again() {
    let fx = Fixture::connected().await;
    fx.fake.fail("GET", "/api/server/about", 500);

    let failing = fx.status(true).await;

    assert_eq!(failing["reachable"], false);
    assert_eq!(failing["needs_reconnect"], false);
    assert!(
        failing["connection"]["last_error"]
            .as_str()
            .unwrap()
            .contains("500")
    );

    fx.fake.heal();
    let healed = fx.status(true).await;
    assert_eq!(healed["reachable"], true);
    assert_eq!(healed["connection"]["last_error"], Value::Null);
}

#[tokio::test]
async fn a_lost_key_file_asks_for_a_new_connection_and_connecting_again_makes_a_new_one() {
    let fx = Fixture::connected().await;
    let old_key = fx.fake.keys().remove(0).secret;
    std::fs::remove_file(key_path(&fx.data_dir())).unwrap();
    fx.fake.clear_calls();

    let status = fx.status(true).await;

    assert_eq!(status["connected"], true);
    assert_eq!(status["needs_reconnect"], true);
    assert_eq!(status["reachable"], Value::Null);
    assert!(
        fx.fake.calls().is_empty(),
        "nothing can be asked without the key"
    );
    assert!(fx.log().contains("cannot be read"));

    let (code, body) = fx.get("/api/immich/libraries").await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "immich_needs_reconnect");

    fx.connect_ok().await;

    assert!(key_path(&fx.data_dir()).exists());
    let healed = fx.status(true).await;
    assert_eq!(healed["needs_reconnect"], false);
    assert_eq!(healed["reachable"], true);
    // The old key cannot be revoked without being read: it stays in Immich for the user to delete.
    let keys = fx.fake.keys();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().any(|key| key.secret == old_key));
    fx.assert_no_leak();
}

#[tokio::test]
async fn disconnecting_with_a_lost_key_file_still_forgets_the_connection() {
    let fx = Fixture::connected().await;
    std::fs::remove_file(key_path(&fx.data_dir())).unwrap();
    fx.fake.clear_calls();

    let (code, result) = fx.delete("/api/immich/connection").await;

    assert_eq!(code, StatusCode::OK, "{result}");
    assert_eq!(result["revoked"], false);
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .contains("cannot be read")
    );
    assert!(fx.fake.calls().is_empty());
    assert_eq!(fx.status(false).await["connected"], false);
}

#[tokio::test]
async fn the_key_is_encrypted_at_rest_and_no_secret_is_kept_in_the_clear() {
    let fx = Fixture::connected().await;
    let key = fx.fake.keys().remove(0).secret;
    // Use the connection a little, so that whatever it might write has been written.
    fx.status(true).await;
    fx.get("/api/immich/libraries").await;

    let stored: String =
        sqlx::query_scalar("SELECT api_key_enc FROM immich_connection WHERE id = 1")
            .fetch_one(fx.app.state.db.read())
            .await
            .unwrap();
    assert!(!stored.is_empty());
    assert!(!stored.contains(&key));
    fx.assert_no_leak();

    // The scan above proves something only if it does find what is there in the clear.
    assert!(!files_containing(&fx.data_dir(), EMAIL.as_bytes()).is_empty());
    assert!(key_path(&fx.data_dir()).is_file());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(key_path(&fx.data_dir()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the key file is open to others: {mode:o}");
    }
}

#[tokio::test]
async fn the_connection_survives_a_restart() {
    let fx = Fixture::connected().await;
    // A new service over the same database and data directory is what a restart makes.
    let restarted = ImmichService::new(fx.app.state.db.clone(), fx.data_dir());

    let remembered = restarted.status(false).await.unwrap();
    assert!(remembered.connected);
    assert_eq!(remembered.reachable, Some(true));
    assert!(!remembered.needs_reconnect);

    let checked = restarted.status(true).await.unwrap();
    assert!(matches!(checked.mode, ImmichMode::Full));
    assert_eq!(checked.reachable, Some(true));
    assert_eq!(checked.server_version.as_deref(), Some(VERSION));
    assert_eq!(fx.fake.count("GET", "/api/server/about"), 2);
}

#[tokio::test]
async fn a_rejected_key_is_still_known_after_a_restart() {
    let fx = Fixture::connected().await;
    fx.fake.revoke_all_keys();
    fx.status(true).await;
    let restarted = ImmichService::new(fx.app.state.db.clone(), fx.data_dir());
    fx.fake.clear_calls();

    let status = restarted.status(false).await.unwrap();

    assert!(status.connected);
    assert!(status.needs_reconnect);
    assert_eq!(status.reachable, Some(true));
    assert!(fx.fake.calls().is_empty());
}

#[tokio::test]
async fn the_service_hands_out_a_client_that_follows_the_connection() {
    let fx = Fixture::new().await;
    assert!(matches!(
        fx.app.state.immich.client().await,
        Err(ImmichError::NotConnected)
    ));

    fx.connect_ok().await;
    let first = fx.app.state.immich.client().await.unwrap();
    let again = fx.app.state.immich.client().await.unwrap();
    assert!(Arc::ptr_eq(&first, &again), "one client, not one per call");
    let first_key = fx.fake.keys().remove(0).secret;
    fx.fake.clear_calls();
    assert_eq!(first.about().await.unwrap().version, VERSION);
    assert_eq!(
        fx.fake.calls()[0].api_key.as_deref(),
        Some(first_key.as_str())
    );

    // A new connection brings a new key, and the next client is made with it.
    fx.connect_ok().await;
    let second = fx.app.state.immich.client().await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    let second_key = fx.fake.keys().remove(0).secret;
    assert_ne!(first_key, second_key);
    fx.fake.clear_calls();
    second.about().await.unwrap();
    assert_eq!(
        fx.fake.calls()[0].api_key.as_deref(),
        Some(second_key.as_str())
    );

    fx.delete("/api/immich/connection").await;
    assert!(matches!(
        fx.app.state.immich.client().await,
        Err(ImmichError::NotConnected)
    ));
}
