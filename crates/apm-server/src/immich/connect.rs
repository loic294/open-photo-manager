//! Connecting to Immich and disconnecting again.
//!
//! # Connecting
//!
//! The application never keeps the account password. It uses it once to ask Immich for an API key
//! and keeps only that key (encrypted, see `crypto`):
//!
//! 1. `POST /auth/login` with the email and the password.
//! 2. `POST /api-keys` with the bearer token of that login: a key with exactly the permissions of
//!    [`KEY_PERMISSIONS`]. Immich shows the key's secret only in this answer.
//! 3. The new key reads `/users/me` and `/server/about`: this proves that the key works and tells
//!    the account, whether it is an administrator (full mode) or not (limited mode) and the server
//!    version.
//! 4. The key is encrypted and the connection stored, replacing a previous one.
//! 5. The login session is ended (`POST /auth/logout`), so no session stays behind in Immich, and
//!    the key of a replaced connection is revoked.
//!
//! It is all or nothing. A failure after step 2 revokes the new key with the bearer token (and
//! only then ends the session, which is what authorises the revocation) and stores nothing; the
//! previous connection stays as it was. Steps 1 to 4 share one time limit that ends before the
//! HTTP layer's own, and the flow runs in a task of its own, so a browser that goes away cannot
//! leave a half-done connection or a key nobody knows about. The clean-up calls are best effort:
//! if Immich cannot be reached to revoke a key, the key stays in Immich under the name
//! [`API_KEY_NAME`], where the user can delete it.
//!
//! # Disconnecting
//!
//! Revokes the key in Immich, removes the connection and forgets the client. The connection is
//! removed even when the key could not be revoked (Immich is down, the key cannot be read any more);
//! the result then says so, and the user is told which key to remove by hand. The mappings stay, so
//! connecting again does not mean entering them again.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use apm_core::dto::{
    FieldError, ImmichConnectRequest, ImmichConnectionDto, ImmichDisconnectResult,
};

use crate::db::now_secs;
use crate::security::Redacted;

use super::client::{API_KEY_NAME, ClientOptions, ImmichClient, KEY_PERMISSIONS};
use super::crypto::SecretBox;
use super::error::ImmichError;
use super::store::{self, NewConnection, StoredConnection};
use super::{ImmichService, connection_dto};

/// The time limits of the connect and disconnect flows. The longest possible request (the connect
/// flow, then the two clean-up calls of a failed one one after the other) must stay below the 30
/// seconds the HTTP layer allows.
#[derive(Clone, Copy, Debug)]
pub(super) struct Timing {
    /// Login, key creation, verification and storing together.
    pub connect: Duration,
    /// Each clean-up call: ending the session, revoking a key.
    pub cleanup: Duration,
    /// Revoking the key of the connection that is being removed.
    pub revoke: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(18),
            cleanup: Duration::from_secs(4),
            revoke: Duration::from_secs(8),
        }
    }
}

/// What the user typed to connect. The password is a secret from the moment it is read.
pub struct ConnectInput {
    pub server_url: String,
    pub email: String,
    pub password: Redacted<String>,
    pub allow_insecure_tls: bool,
}

impl From<ImmichConnectRequest> for ConnectInput {
    fn from(request: ImmichConnectRequest) -> Self {
        Self {
            server_url: request.server_url,
            email: request.email.trim().to_owned(),
            password: Redacted::new(request.password),
            allow_insecure_tls: request.allow_insecure_tls,
        }
    }
}

impl fmt::Debug for ConnectInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectInput")
            .field("server_url", &"[redacted]")
            .field("email", &self.email)
            .field("password", &self.password)
            .field("allow_insecure_tls", &self.allow_insecure_tls)
            .finish()
    }
}

impl ConnectInput {
    /// What can be checked without asking Immich: nothing is sent for an input that fails here.
    /// (The address itself is checked when the client is built.)
    fn validate(&self) -> Result<(), ImmichError> {
        let mut problems = Vec::new();
        if self.server_url.trim().is_empty() {
            problems.push(FieldError::new(
                "server_url",
                "is required: the address of the Immich server, such as http://192.168.1.10:2283",
            ));
        }
        match self.email.split_once('@') {
            _ if self.email.is_empty() => {
                problems.push(FieldError::new("email", "is required"));
            }
            Some((name, domain)) if !name.is_empty() && !domain.is_empty() => {}
            _ => problems.push(FieldError::new(
                "email",
                "must be the email address of the Immich account",
            )),
        }
        if self.password.expose().is_empty() {
            problems.push(FieldError::new("password", "is required"));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(ImmichError::Validation(problems))
        }
    }
}

/// What a connect flow created in Immich, which a failure has to take away again.
#[derive(Default)]
struct Created {
    bearer: Option<Redacted<String>>,
    key_id: Option<String>,
}

impl ImmichService {
    /// Connects to Immich and stores the connection, replacing a previous one; see the module
    /// documentation. The answer describes the stored connection.
    pub async fn connect(&self, input: ConnectInput) -> Result<ImmichConnectionDto, ImmichError> {
        input.validate()?;
        let service = self.clone();
        tokio::spawn(async move { service.connect_flow(input).await })
            .await
            .map_err(|err| ImmichError::Internal(format!("the connect task failed: {err}")))?
    }

    /// Revokes the API key in Immich and removes the connection; see the module documentation.
    /// [`ImmichError::NotConnected`] when there is none.
    pub async fn disconnect(&self) -> Result<ImmichDisconnectResult, ImmichError> {
        let service = self.clone();
        tokio::spawn(async move { service.disconnect_flow().await })
            .await
            .map_err(|err| ImmichError::Internal(format!("the disconnect task failed: {err}")))?
    }

    async fn connect_flow(&self, input: ConnectInput) -> Result<ImmichConnectionDto, ImmichError> {
        let _gate = self.inner.gate.lock().await;
        let timing = self.inner.timing;
        let previous = store::load(&self.inner.db).await?;
        let options = ClientOptions {
            allow_insecure_tls: input.allow_insecure_tls,
            ..ClientOptions::default()
        };
        let anonymous = ImmichClient::new(&input.server_url, None, options.clone())?;

        let mut created = Created::default();
        let outcome = tokio::time::timeout(
            timing.connect,
            self.establish(&anonymous, &options, &input, &mut created),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ImmichError::Unreachable(
                "Immich did not answer in time while connecting".to_owned(),
            ))
        });

        let session_end = async {
            if let Some(bearer) = &created.bearer {
                best_effort(
                    "ending the Immich login session",
                    timing.cleanup,
                    anonymous.logout(bearer),
                )
                .await;
            }
        };
        if outcome.is_ok() {
            tokio::join!(session_end, self.revoke_replaced(previous.as_ref()));
        } else {
            // The login session is what authorises the revocation, so it ends only afterwards.
            if let (Some(bearer), Some(key_id)) = (&created.bearer, &created.key_id) {
                best_effort(
                    "revoking the API key of the failed connection",
                    timing.cleanup,
                    anonymous.revoke_api_key_with_bearer(bearer, key_id),
                )
                .await;
            }
            session_end.await;
        }
        outcome
    }

    /// Steps 1 to 4 of the module documentation. What is created in Immich on the way is recorded
    /// in `created` the moment it exists, so the caller can clean up whatever happens here, a
    /// timeout included.
    async fn establish(
        &self,
        anonymous: &ImmichClient,
        options: &ClientOptions,
        input: &ConnectInput,
        created: &mut Created,
    ) -> Result<ImmichConnectionDto, ImmichError> {
        let session = anonymous.login(&input.email, &input.password).await?;
        created.bearer = Some(session.access_token.clone());

        let key = anonymous
            .create_api_key(&session.access_token, API_KEY_NAME, KEY_PERMISSIONS)
            .await
            .map_err(|err| match err {
                ImmichError::Forbidden => ImmichError::Upstream {
                    status: 403,
                    message: "this account is not allowed to create API keys".to_owned(),
                },
                other => other,
            })?;
        created.key_id = Some(key.id.clone());

        let keyed = ImmichClient::new(
            &anonymous.base_url(),
            Some(key.secret.clone()),
            options.clone(),
        )?;
        let me = keyed.me().await.map_err(key_not_accepted)?;
        let about = keyed.about().await.map_err(key_not_accepted)?;

        let encrypted = self.encrypt_key(key.secret).await?;
        let user_name = if me.name.is_empty() {
            session.user_name
        } else {
            me.name
        };
        let email = if me.email.is_empty() {
            input.email.as_str()
        } else {
            me.email.as_str()
        };
        let server_version = (!about.version.is_empty()).then_some(about.version.as_str());
        store::replace(
            &self.inner.db,
            &NewConnection {
                server_url: &keyed.base_url(),
                email,
                user_id: &me.id,
                user_name: &user_name,
                is_admin: me.is_admin,
                api_key_id: &key.id,
                api_key_enc: &encrypted,
                allow_insecure_tls: input.allow_insecure_tls,
                server_version,
                connected_at: now_secs(),
            },
        )
        .await?;

        let stored = self.stored().await?;
        Ok(connection_dto(&stored))
    }

    /// Encrypts the API key with the key file of the data directory, creating that file on the
    /// first connection.
    async fn encrypt_key(&self, key: Redacted<String>) -> Result<String, ImmichError> {
        let data_dir = self.inner.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            SecretBox::load_or_create(&data_dir).and_then(|secrets| secrets.encrypt(key.expose()))
        })
        .await
        .map_err(|err| ImmichError::Internal(format!("encrypting the API key failed: {err}")))?
        .map_err(|err| ImmichError::Internal(format!("cannot store the API key: {err}")))
    }

    /// Revokes the key of the connection that a new one replaced, with a client of its own: the
    /// old connection may be to another server.
    async fn revoke_replaced(&self, previous: Option<&StoredConnection>) {
        let Some(previous) = previous else { return };
        best_effort(
            "revoking the API key of the replaced Immich connection",
            self.inner.timing.cleanup,
            async {
                let client = self.build_client(previous).await?;
                match client.delete_api_key(&previous.api_key_id).await {
                    Err(err) if key_is_gone(&err) => Ok(()),
                    other => other,
                }
            },
        )
        .await;
    }

    async fn disconnect_flow(&self) -> Result<ImmichDisconnectResult, ImmichError> {
        let _gate = self.inner.gate.lock().await;
        let stored = self.stored().await?;
        let revoked = tokio::time::timeout(self.inner.timing.revoke, async {
            let client = self.build_client(&stored).await?;
            client.delete_api_key(&stored.api_key_id).await
        })
        .await;
        let result = match revoked {
            Ok(Ok(())) => ImmichDisconnectResult {
                revoked: true,
                message: None,
            },
            Ok(Err(err)) if key_is_gone(&err) => ImmichDisconnectResult {
                revoked: true,
                message: None,
            },
            Ok(Err(ImmichError::NeedsReconnect)) => ImmichDisconnectResult {
                revoked: false,
                message: Some(
                    "The stored API key cannot be read any more, so it could not be revoked."
                        .to_owned(),
                ),
            },
            Ok(Err(err)) => ImmichDisconnectResult {
                revoked: false,
                message: Some(err.summary()),
            },
            Err(_) => ImmichDisconnectResult {
                revoked: false,
                message: Some("Immich did not answer in time.".to_owned()),
            },
        };

        store::delete(&self.inner.db).await?;
        self.inner.cache.lock().await.take();
        // Seam for `syncing-immich-assets`: remove here what the sync keeps about the connection
        // (its cursor, the mirrored asset ids). The mappings stay on purpose.
        Ok(result)
    }
}

/// Runs a clean-up call whose failure must not fail the flow it belongs to; says in the log what
/// did not work. The texts of `ImmichError` carry no secret.
async fn best_effort(
    what: &str,
    limit: Duration,
    work: impl Future<Output = Result<(), ImmichError>>,
) {
    match tokio::time::timeout(limit, work).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!(error = %err, "{what} failed"),
        Err(_) => tracing::warn!("{what} timed out"),
    }
}

/// An answer that shows the key is not there any more (Immich forgot or rejects it): nothing is
/// left to revoke.
fn key_is_gone(err: &ImmichError) -> bool {
    match err {
        ImmichError::Unauthorized | ImmichError::NotFound => true,
        ImmichError::Upstream {
            status: 400,
            message,
        } => message.to_ascii_lowercase().contains("not found"),
        _ => false,
    }
}

/// What it means that a key Immich just made does not work.
fn key_not_accepted(err: ImmichError) -> ImmichError {
    match err {
        ImmichError::Unauthorized => {
            ImmichError::Protocol("Immich did not accept the API key it just created".to_owned())
        }
        ImmichError::Forbidden => ImmichError::Protocol(
            "the API key Immich created lacks a permission the application needs".to_owned(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::db::Db;

    fn input(url: &str) -> ConnectInput {
        ConnectInput {
            server_url: url.to_owned(),
            email: "me@example.org".to_owned(),
            password: Redacted::new("unit-test-password".to_owned()),
            allow_insecure_tls: false,
        }
    }

    #[test]
    fn missing_fields_are_reported_together() {
        let empty = ConnectInput {
            server_url: " ".to_owned(),
            email: String::new(),
            password: Redacted::new(String::new()),
            allow_insecure_tls: false,
        };
        let Err(ImmichError::Validation(problems)) = empty.validate() else {
            panic!("an empty input must be refused");
        };
        let fields: Vec<_> = problems.iter().map(|p| p.field.as_str()).collect();
        assert_eq!(fields, ["server_url", "email", "password"]);

        for bad in ["me", "@example.org", "me@"] {
            let mut input = input("http://immich");
            input.email = bad.to_owned();
            let Err(ImmichError::Validation(problems)) = input.validate() else {
                panic!("{bad} is not an email address");
            };
            assert_eq!(problems.len(), 1, "{bad}");
            assert_eq!(problems[0].field, "email");
        }
        assert!(input("http://immich").validate().is_ok());
    }

    #[test]
    fn the_request_is_converted_with_the_password_hidden() {
        let request = ImmichConnectRequest {
            server_url: "http://immich:2283".to_owned(),
            email: "  me@example.org ".to_owned(),
            password: "converted-password".to_owned(),
            allow_insecure_tls: true,
        };
        let input = ConnectInput::from(request);
        assert_eq!(input.email, "me@example.org");
        assert!(input.allow_insecure_tls);
        assert_eq!(input.password.expose(), "converted-password");
        let shown = format!("{input:?} {input:#?}");
        assert!(!shown.contains("converted-password"), "{shown}");
        assert!(!shown.contains("immich:2283"), "{shown}");
    }

    #[test]
    fn only_answers_that_mean_a_gone_key_count_as_revoked() {
        assert!(key_is_gone(&ImmichError::Unauthorized));
        assert!(key_is_gone(&ImmichError::NotFound));
        assert!(key_is_gone(&ImmichError::Upstream {
            status: 400,
            message: "API Key not found".to_owned(),
        }));
        assert!(!key_is_gone(&ImmichError::Upstream {
            status: 400,
            message: "bad id".to_owned(),
        }));
        assert!(!key_is_gone(&ImmichError::Forbidden));
        assert!(!key_is_gone(&ImmichError::Unreachable("down".to_owned())));
    }

    fn quick() -> Timing {
        Timing {
            connect: Duration::from_millis(300),
            cleanup: Duration::from_secs(2),
            revoke: Duration::from_secs(2),
        }
    }

    async fn service(timing: Timing) -> (ImmichService, crate::db::TempDb, tempfile::TempDir) {
        let temp = Db::open_temp().await.unwrap();
        let dir = crate::immich::test_support::scratch_dir();
        let service = ImmichService::with_timing(temp.handle(), dir.path().to_owned(), timing);
        (service, temp, dir)
    }

    #[tokio::test]
    async fn a_silent_server_ends_the_connect_in_time_and_leaves_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let (service, temp, _dir) = service(quick()).await;

        let started = std::time::Instant::now();
        let err = service.connect(input(&server.uri())).await.unwrap_err();
        assert!(matches!(err, ImmichError::Unreachable(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(store::load(&temp.handle()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_timeout_after_the_key_exists_still_revokes_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accessToken": "unit-bearer-token",
                "userId": "user-1",
                "userEmail": "me@example.org",
                "name": "Me",
                "isAdmin": true,
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/api-keys"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "secret": "unit-api-key-secret",
                "apiKey": { "id": "key-77", "name": API_KEY_NAME },
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/users/me"))
            .and(header("x-api-key", "unit-api-key-secret"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/api-keys/key-77"))
            .and(header("authorization", "Bearer unit-bearer-token"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/auth/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "successful": true,
                "redirectUri": "/auth/login",
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (service, temp, _dir) = service(quick()).await;

        let err = service.connect(input(&server.uri())).await.unwrap_err();
        assert!(matches!(err, ImmichError::Unreachable(_)), "{err:?}");
        assert!(store::load(&temp.handle()).await.unwrap().is_none());
        server.verify().await;
    }
}
