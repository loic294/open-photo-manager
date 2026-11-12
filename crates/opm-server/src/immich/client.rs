//! Immich HTTP client (reqwest): authentication, libraries, assets, albums and tags.
//!
//! Owned by todo `building-immich-client`. Every request goes through ONE private path
//! (`ImmichClient::execute`) that builds the URL, adds the credential, applies the timeouts, reads
//! the answer within a size limit and maps the status to an [`ImmichError`]; the typed methods only
//! say which endpoint they call and what comes back. The endpoints and shapes are those of the
//! OpenAPI spec of Immich v3.4.0-rc.0 (`models::SPEC_VERSION`); every response model is read
//! tolerantly (see `models`).
//!
//! # Rules that hold for every call
//!
//! - **Secrets.** The API key lives only in a header value marked sensitive; the bearer token and
//!   the password of the connect flow only in the request that needs them. Nothing here logs a body,
//!   a header or a query (the debug log has method, path, status and duration), and every text that
//!   comes from the network is scrubbed with `redact_urls_in` and shortened before it is kept.
//! - **No redirects.** A redirect is never followed, the credential would be sent where the user did
//!   not point it. It becomes [`ImmichError::Redirected`] that names the address to use instead.
//! - **Bounded.** Connecting, a whole request and the size of an answer are limited; the probes
//!   ([`ImmichClient::ping`], [`ImmichClient::check`]) give up after three seconds.
//! - **Ids** that go into a URL are checked first: `url` silently drops `.` and `..` segments.
//! - **Deleting** takes an [`OfflineAssetId`]: see there for why.
//!
//! # Search
//!
//! [`ImmichClient::search_metadata`] hides the two request generations of Immich and applies the
//! documented [`MetadataSearch`] semantics to what comes back (see `search`).
//!
//! # Health
//!
//! The client remembers how its latest call went ([`ImmichClient::health`]), so the status endpoint
//! can say whether Immich is reachable without making a request of its own.

mod search;
#[cfg(test)]
mod tests;

use std::error::Error as _;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderName, HeaderValue, LOCATION};
use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::db::now_secs;
use crate::security::{Redacted, redact_url, redact_urls_in};

use super::error::{ImmichError, MAX_UPSTREAM_MESSAGE_CHARS, truncate_chars};
use super::models::{
    AssetUpdate, BulkIdResult, CreatedApiKey, ImmichAlbum, ImmichAsset, ImmichLibrary, ImmichTag,
    ImmichUser, ImmichVersion, LoginSession, MetadataSearch, OfflineAssetId, PageToken, SearchPage,
    ServerAbout,
};
use search::{Reading, Style};

/// The name of the API key the application creates in Immich.
pub const API_KEY_NAME: &str = "Open Photo Manager";

/// The permissions of that key: what the sync and the outbox use, plus `apiKey.delete` so that
/// disconnecting can revoke the key. A unit test checks every entry against the spec's enum.
pub const KEY_PERMISSIONS: &[&str] = &[
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

const API_KEY_HEADER: HeaderName = HeaderName::from_static("x-api-key");
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// The largest answer read: far above a page of 1000 assets with their EXIF data.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
/// Ids per request of the bulk calls, so a request stays small however many assets are meant.
const BULK_CHUNK: usize = 500;

/// How a client connects.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub connect_timeout: Duration,
    /// The longest a whole request may take, answer included.
    pub request_timeout: Duration,
    /// Accept a certificate that does not verify (a self-signed one on a NAS). The user decides,
    /// per connection.
    pub allow_insecure_tls: bool,
    /// The server version recorded when connecting; it decides the search request shape. `None`
    /// means unknown: the client then tries the newer shape and falls back once.
    pub server_version: Option<ImmichVersion>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            allow_insecure_tls: false,
            server_version: None,
        }
    }
}

/// How the latest calls went (see [`ImmichClient::health`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    /// Epoch seconds of the last call that showed a working connection.
    pub last_ok_at: Option<i64>,
    /// What went wrong in the latest call; cleared by the next one that works.
    pub last_error: Option<String>,
    /// Immich answered 401 to the API key and no call has worked since: the key was revoked or has
    /// expired, so the user has to connect again.
    pub key_rejected: bool,
}

impl Health {
    /// `Some(false)` while the latest call failed, `Some(true)` after one that worked, `None`
    /// before any call.
    pub fn reachable(&self) -> Option<bool> {
        if self.last_error.is_some() {
            Some(false)
        } else {
            self.last_ok_at.map(|_| true)
        }
    }
}

/// A client for one Immich server and (usually) one API key.
///
/// Shared as `Arc<ImmichClient>`; every method takes `&self`. Obtain the application's client only
/// from `ImmichService::client`, so the credential handling stays in one place.
pub struct ImmichClient {
    http: reqwest::Client,
    base: Url,
    key_header: Option<HeaderValue>,
    server_version: Option<ImmichVersion>,
    /// Set once an unknown-version server proved to speak the flat search form.
    flat_search: AtomicBool,
    health: Mutex<Health>,
}

impl fmt::Debug for ImmichClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImmichClient")
            .field("base_url", &redact_url(&self.base_url()))
            .field("authenticated", &self.key_header.is_some())
            .field("server_version", &self.server_version)
            .finish_non_exhaustive()
    }
}

/// Which credential a request carries.
#[derive(Clone, Copy)]
enum Auth<'a> {
    ApiKey,
    Bearer(&'a Redacted<String>),
    Anonymous,
}

/// One request, described before it is sent.
struct Request<'a> {
    method: Method,
    /// The path after `/api`.
    path: Vec<&'a str>,
    query: Vec<(&'a str, &'a str)>,
    body: Option<Value>,
    auth: Auth<'a>,
    timeout: Option<Duration>,
    /// The body or the answer carries a secret: decode failures say nothing about the content.
    sensitive: bool,
}

impl<'a> Request<'a> {
    fn new(method: Method, path: &[&'a str]) -> Self {
        Self {
            method,
            path: path.to_vec(),
            query: Vec::new(),
            body: None,
            auth: Auth::ApiKey,
            timeout: None,
            sensitive: false,
        }
    }

    fn query(mut self, name: &'a str, value: &'a str) -> Self {
        self.query.push((name, value));
        self
    }

    fn json(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    fn auth(mut self, auth: Auth<'a>) -> Self {
        self.auth = auth;
        self
    }

    fn within(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }

    /// `POST /search/metadata`, for logs and error texts (ids are not secret, queries are left out).
    fn describe(&self) -> String {
        format!("{} /{}", self.method, self.path.join("/"))
    }
}

impl ImmichClient {
    /// Builds a client for `server_url` (see [`normalize_base_url`] for what is accepted).
    ///
    /// `api_key` is `None` for the calls of the connect flow that need none. A key that cannot be
    /// sent as a header is reported as [`ImmichError::NeedsReconnect`]: it is not one Immich made.
    pub fn new(
        server_url: &str,
        api_key: Option<Redacted<String>>,
        options: ClientOptions,
    ) -> Result<Self, ImmichError> {
        let base = normalize_base_url(server_url)?;
        let key_header = api_key
            .map(|key| {
                let mut value =
                    HeaderValue::from_str(key.expose()).map_err(|_| ImmichError::NeedsReconnect)?;
                value.set_sensitive(true);
                Ok::<_, ImmichError>(value)
            })
            .transpose()?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(options.connect_timeout)
            .timeout(options.request_timeout)
            .user_agent(concat!("open-photo-manager/", env!("CARGO_PKG_VERSION")))
            .danger_accept_invalid_certs(options.allow_insecure_tls)
            .build()
            .map_err(|err| {
                ImmichError::Internal(format!(
                    "cannot create the HTTP client: {}",
                    err.without_url()
                ))
            })?;
        Ok(Self {
            http,
            base,
            key_header,
            server_version: options.server_version,
            flat_search: AtomicBool::new(false),
            health: Mutex::new(Health::default()),
        })
    }

    /// The normalised server address, without a trailing slash (`http://nas:2283`). This is what
    /// the connection stores.
    pub fn base_url(&self) -> String {
        self.base.as_str().trim_end_matches('/').to_owned()
    }

    pub fn server_version(&self) -> Option<ImmichVersion> {
        self.server_version
    }

    /// Whether this server takes the typed search filter (Immich 3.2 and later). An unknown version
    /// counts as new, the same assumption [`ImmichClient::search_metadata`] starts from.
    pub fn supports_search_filter(&self) -> bool {
        self.server_version
            .is_none_or(ImmichVersion::supports_search_filter)
    }

    /// How the latest calls of this client went.
    pub fn health(&self) -> Health {
        self.lock_health().clone()
    }

    // ----- connect flow: no API key yet -------------------------------------------------------

    /// `POST /auth/login`. A refused email or password is [`ImmichError::AuthFailed`].
    pub async fn login(
        &self,
        email: &str,
        password: &Redacted<String>,
    ) -> Result<LoginSession, ImmichError> {
        let request = Request::new(Method::POST, &["auth", "login"])
            .auth(Auth::Anonymous)
            .json(json!({ "email": email, "password": password.expose() }))
            .sensitive();
        match self.call_json(request).await {
            Err(ImmichError::Unauthorized) => Err(ImmichError::AuthFailed),
            other => other,
        }
    }

    /// `POST /api-keys` with the bearer token of [`ImmichClient::login`]. The secret is only
    /// returned this once.
    pub async fn create_api_key(
        &self,
        bearer: &Redacted<String>,
        name: &str,
        permissions: &[&str],
    ) -> Result<CreatedApiKey, ImmichError> {
        let request = Request::new(Method::POST, &["api-keys"])
            .auth(Auth::Bearer(bearer))
            .json(json!({ "name": name, "permissions": permissions }))
            .sensitive();
        self.call_json(request).await
    }

    /// `POST /auth/logout`: ends the session of the bearer token.
    pub async fn logout(&self, bearer: &Redacted<String>) -> Result<(), ImmichError> {
        let request = Request::new(Method::POST, &["auth", "logout"]).auth(Auth::Bearer(bearer));
        self.call_empty(request).await
    }

    /// `DELETE /api-keys/{id}` with a bearer token: cleans up a key whose connection failed.
    pub async fn revoke_api_key_with_bearer(
        &self,
        bearer: &Redacted<String>,
        key_id: &str,
    ) -> Result<(), ImmichError> {
        let key_id = safe_id(key_id)?;
        let request =
            Request::new(Method::DELETE, &["api-keys", key_id]).auth(Auth::Bearer(bearer));
        self.call_empty(request).await
    }

    /// `GET /server/version`, no credential needed.
    pub async fn version(&self) -> Result<ImmichVersion, ImmichError> {
        let request = Request::new(Method::GET, &["server", "version"]).auth(Auth::Anonymous);
        self.call_json(request).await
    }

    /// `GET /server/ping`, no credential needed; gives up after three seconds.
    pub async fn ping(&self) -> Result<(), ImmichError> {
        #[derive(Deserialize)]
        struct Pong {
            res: String,
        }
        let request = Request::new(Method::GET, &["server", "ping"])
            .auth(Auth::Anonymous)
            .within(PROBE_TIMEOUT);
        let pong: Pong = self.call_json(request).await?;
        if pong.res == "pong" {
            Ok(())
        } else {
            Err(ImmichError::Protocol(
                "the server did not answer the ping like Immich".into(),
            ))
        }
    }

    // ----- with the API key -------------------------------------------------------------------

    /// `GET /users/me`: the account the key belongs to.
    pub async fn me(&self) -> Result<ImmichUser, ImmichError> {
        self.call_json(Request::new(Method::GET, &["users", "me"]))
            .await
    }

    /// `GET /server/about`: carries the server version.
    pub async fn about(&self) -> Result<ServerAbout, ImmichError> {
        self.call_json(Request::new(Method::GET, &["server", "about"]))
            .await
    }

    /// The status probe: an authenticated `GET /server/about` that gives up after three seconds.
    /// Unlike [`ImmichClient::ping`] it also shows that the API key is still accepted.
    pub async fn check(&self) -> Result<ServerAbout, ImmichError> {
        let request = Request::new(Method::GET, &["server", "about"]).within(PROBE_TIMEOUT);
        self.call_json(request).await
    }

    /// `DELETE /api-keys/{id}`: revokes a key, normally the one this client uses.
    pub async fn delete_api_key(&self, key_id: &str) -> Result<(), ImmichError> {
        let key_id = safe_id(key_id)?;
        self.call_empty(Request::new(Method::DELETE, &["api-keys", key_id]))
            .await
    }

    /// `GET /libraries`. Immich answers 403 to an account that is not an administrator.
    pub async fn libraries(&self) -> Result<Vec<ImmichLibrary>, ImmichError> {
        self.call_json(Request::new(Method::GET, &["libraries"]))
            .await
    }

    /// `GET /libraries/{id}` (administrators only).
    pub async fn library(&self, id: &str) -> Result<ImmichLibrary, ImmichError> {
        let id = safe_id(id)?;
        self.call_json(Request::new(Method::GET, &["libraries", id]))
            .await
    }

    /// `PUT /libraries/{id}` with only `exclusionPatterns`, which REPLACES the list: send the
    /// complete list (administrators only).
    pub async fn update_library_exclusions(
        &self,
        id: &str,
        patterns: &[String],
    ) -> Result<(), ImmichError> {
        let id = safe_id(id)?;
        let request = Request::new(Method::PUT, &["libraries", id])
            .json(json!({ "exclusionPatterns": patterns }));
        self.call_empty(request).await
    }

    /// `POST /libraries/{id}/scan`: asks Immich to scan the library for changes (administrators
    /// only). It returns at once, the scan runs in Immich.
    pub async fn scan_library(&self, id: &str) -> Result<(), ImmichError> {
        let id = safe_id(id)?;
        self.call_empty(Request::new(Method::POST, &["libraries", id, "scan"]))
            .await
    }

    /// `POST /search/metadata`, one page. Works for accounts that are not administrators.
    ///
    /// Which request generation is used follows the recorded server version (see
    /// [`MetadataSearch`]): from 3.2.0 the typed filter and a cursor, before that the flat fields
    /// and a page number. With no known version the first page is tried in the new form and, when
    /// the server rejects it or answers in the old paging style, once in the old form; the
    /// outcome is remembered. A token of `after` always continues in the style that issued it.
    ///
    /// The returned items follow the documented semantics of the search whatever the server did,
    /// so a page can be shorter than `size`, even empty: continue until `next` is `None`. A server
    /// that answers with assets of another library than asked for is refused.
    pub async fn search_metadata(
        &self,
        search: &MetadataSearch,
    ) -> Result<SearchPage, ImmichError> {
        let style = match &search.after {
            Some(PageToken::Cursor(_)) => Some(Style::Filter),
            Some(PageToken::Page(_)) => Some(Style::Flat),
            None if self.flat_search.load(Ordering::Relaxed) => Some(Style::Flat),
            None => match self.server_version {
                Some(version) if version.supports_search_filter() => Some(Style::Filter),
                Some(_) => Some(Style::Flat),
                None => None,
            },
        };
        if let Some(style) = style {
            return match self.search_once(style, search, false).await? {
                Attempt::Page(page) => Ok(page),
                Attempt::WrongStyle(err) => Err(err),
            };
        }

        match self.search_once(Style::Filter, search, true).await? {
            Attempt::Page(page) => Ok(page),
            Attempt::WrongStyle(original) => {
                match self.search_once(Style::Flat, search, false).await {
                    Ok(Attempt::Page(page)) => {
                        self.flat_search.store(true, Ordering::Relaxed);
                        Ok(page)
                    }
                    Ok(Attempt::WrongStyle(_)) => Err(original),
                    // Both forms refused the same way: what the typed form said is the clearer
                    // report. Any other failure is news about the connection itself.
                    Err(ImmichError::Upstream { status: 400, .. })
                        if matches!(original, ImmichError::Upstream { .. }) =>
                    {
                        Err(original)
                    }
                    Err(flat) => Err(flat),
                }
            }
        }
    }

    async fn search_once(
        &self,
        style: Style,
        search: &MetadataSearch,
        probing: bool,
    ) -> Result<Attempt, ImmichError> {
        let request =
            Request::new(Method::POST, &["search", "metadata"]).json(search::body(style, search));
        let bytes = match self.execute(request).await {
            Ok(bytes) => bytes,
            Err(ImmichError::Upstream {
                status: 400,
                message,
            }) if probing && search::rejects_the_filter_form(&message) => {
                return Ok(Attempt::WrongStyle(ImmichError::Upstream {
                    status: 400,
                    message,
                }));
            }
            Err(err) => return Err(err),
        };
        match search::read_page(style, search, &bytes, probing).inspect_err(|err| {
            self.remember_error(err);
        })? {
            Reading::Page(page) => Ok(Attempt::Page(page)),
            Reading::OldServer => Ok(Attempt::WrongStyle(ImmichError::Protocol(
                "the server does not know the typed search filter of Immich 3.2".into(),
            ))),
        }
    }

    /// `GET /assets/{id}`. `None` when Immich does not know the asset (or the key cannot see it):
    /// Immich answers that with 404 or with a 400 "Not found or no asset.read access".
    pub async fn get_asset(&self, id: &str) -> Result<Option<ImmichAsset>, ImmichError> {
        let id = safe_id(id)?;
        match self
            .call_json(Request::new(Method::GET, &["assets", id]))
            .await
        {
            Ok(asset) => Ok(Some(asset)),
            Err(ImmichError::NotFound) => Ok(None),
            Err(ImmichError::Upstream {
                status: 400,
                message,
            }) if message.to_ascii_lowercase().contains("not found") => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// `PUT /assets`: sets the given fields on all `ids` (in requests of at most 500). Nothing is
    /// sent for no ids or no fields; an invalid field is refused before the first request.
    pub async fn update_assets(
        &self,
        ids: &[String],
        update: &AssetUpdate,
    ) -> Result<(), ImmichError> {
        if ids.is_empty() || update.is_empty() {
            return Ok(());
        }
        for chunk in ids.chunks(BULK_CHUNK) {
            let body = update.body(chunk)?;
            self.call_empty(Request::new(Method::PUT, &["assets"]).json(body))
                .await?;
        }
        Ok(())
    }

    /// `DELETE /assets`: removes the records of assets Immich reports as OFFLINE (in requests of at
    /// most 500). With `force` they are deleted for good, without they only go to Immich's trash.
    ///
    /// Only [`OfflineAssetId`]s are accepted, because deleting a record whose file is still on disk
    /// removes the original file; see there for the rule and how the later todos meet it.
    pub async fn delete_assets(
        &self,
        ids: &[OfflineAssetId],
        force: bool,
    ) -> Result<(), ImmichError> {
        for chunk in ids.chunks(BULK_CHUNK) {
            let ids: Vec<&str> = chunk.iter().map(OfflineAssetId::as_str).collect();
            let request = Request::new(Method::DELETE, &["assets"])
                .json(json!({ "ids": ids, "force": force }));
            self.call_empty(request).await?;
        }
        Ok(())
    }

    /// `GET /albums?assetId=`: the albums that hold the asset.
    pub async fn albums_for_asset(&self, asset_id: &str) -> Result<Vec<ImmichAlbum>, ImmichError> {
        let asset_id = safe_id(asset_id)?;
        self.call_json(Request::new(Method::GET, &["albums"]).query("assetId", asset_id))
            .await
    }

    /// `PUT /albums/{id}/assets`: one result per asset. An asset that is already in the album
    /// reports `duplicate`, which [`BulkIdResult::is_ok`] counts as done.
    pub async fn add_assets_to_album(
        &self,
        album_id: &str,
        asset_ids: &[String],
    ) -> Result<Vec<BulkIdResult>, ImmichError> {
        let album_id = safe_id(album_id)?;
        let mut results = Vec::with_capacity(asset_ids.len());
        for chunk in asset_ids.chunks(BULK_CHUNK) {
            let request = Request::new(Method::PUT, &["albums", album_id, "assets"])
                .json(json!({ "ids": chunk }));
            results.extend(self.call_json::<Vec<BulkIdResult>>(request).await?);
        }
        Ok(results)
    }

    /// `GET /tags`: every tag of the account. The key may not create tags (it lacks `tag.create`).
    pub async fn tags(&self) -> Result<Vec<ImmichTag>, ImmichError> {
        self.call_json(Request::new(Method::GET, &["tags"])).await
    }

    /// `PUT /tags/assets`: puts every tag on every asset (assets in requests of at most 500) and
    /// returns how many assets Immich says it tagged.
    pub async fn tag_assets(
        &self,
        tag_ids: &[String],
        asset_ids: &[String],
    ) -> Result<u64, ImmichError> {
        #[derive(Deserialize)]
        struct Tagged {
            #[serde(default)]
            count: i64,
        }
        if tag_ids.is_empty() {
            return Ok(0);
        }
        let mut total = 0_u64;
        for chunk in asset_ids.chunks(BULK_CHUNK) {
            let request = Request::new(Method::PUT, &["tags", "assets"])
                .json(json!({ "assetIds": chunk, "tagIds": tag_ids }));
            let tagged: Tagged = self.call_json(request).await?;
            total = total.saturating_add(u64::try_from(tagged.count).unwrap_or(0));
        }
        Ok(total)
    }

    // ----- the one request path ---------------------------------------------------------------

    async fn call_json<T: DeserializeOwned>(&self, request: Request<'_>) -> Result<T, ImmichError> {
        let what = request.describe();
        let sensitive = request.sensitive;
        let bytes = self.execute(request).await?;
        serde_json::from_slice(&bytes).map_err(|err| {
            let error = ImmichError::Protocol(if sensitive {
                format!("unexpected response from {what}")
            } else {
                format!(
                    "unexpected response from {what}: {}",
                    truncate_chars(
                        &redact_urls_in(&err.to_string()),
                        MAX_UPSTREAM_MESSAGE_CHARS
                    )
                )
            });
            self.remember_error(&error);
            error
        })
    }

    async fn call_empty(&self, request: Request<'_>) -> Result<(), ImmichError> {
        self.execute(request).await.map(drop)
    }

    /// Sends the request and returns the body of a 2xx answer; records the outcome in the health.
    /// A problem that arose before anything was sent says nothing about the connection.
    async fn execute(&self, request: Request<'_>) -> Result<Vec<u8>, ImmichError> {
        let outcome = self.exchange(&request).await;
        match &outcome {
            Ok(_) => self.remember_ok(),
            Err(err) if shows_trouble(err) => self.remember_error(err),
            Err(err) if shows_contact(err) => self.remember_ok(),
            Err(_) => {}
        }
        outcome
    }

    async fn exchange(&self, request: &Request<'_>) -> Result<Vec<u8>, ImmichError> {
        let url = self.url(&request.path, &request.query)?;
        let mut builder = self
            .http
            .request(request.method.clone(), url.clone())
            .header(ACCEPT, "application/json");
        match request.auth {
            Auth::Anonymous => {}
            Auth::ApiKey => {
                let key = self.key_header.clone().ok_or(ImmichError::NotConnected)?;
                builder = builder.header(API_KEY_HEADER, key);
            }
            Auth::Bearer(token) => {
                let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                    .map_err(|_| {
                        ImmichError::Protocol(
                            "Immich sent an access token that cannot be used".into(),
                        )
                    })?;
                value.set_sensitive(true);
                builder = builder.header(AUTHORIZATION, value);
            }
        }
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }

        let started = Instant::now();
        let response = builder.send().await.map_err(|err| {
            let error = transport_error(err);
            tracing::debug!(request = %request.describe(), error = %error, "immich request failed");
            error
        })?;
        let status = response.status();
        tracing::debug!(
            request = %request.describe(),
            status = status.as_u16(),
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "immich request"
        );

        if status.is_redirection() {
            return Err(self.redirected(&url, &response));
        }
        if status.is_success() {
            return read_body(response, MAX_RESPONSE_BYTES).await;
        }
        let body = read_capped(response, MAX_ERROR_BODY_BYTES).await;
        Err(status_error(status, &body))
    }

    /// `<base>/api/<path>?<query>`.
    fn url(&self, path: &[&str], query: &[(&str, &str)]) -> Result<Url, ImmichError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| ImmichError::InvalidUrl("the address cannot carry a path".into()))?
            .pop_if_empty()
            .push("api")
            .extend(path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    /// The error for a redirect: the base address the redirect points at, when it keeps the path
    /// this client appended (the usual http to https or host change), else the target as it is.
    fn redirected(&self, requested: &Url, response: &reqwest::Response) -> ImmichError {
        let target = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| requested.join(location).ok());
        let to = match target {
            None => "another address".to_owned(),
            Some(mut target) => {
                let _ = target.set_username("");
                let _ = target.set_password(None);
                target.set_query(None);
                target.set_fragment(None);
                let base_len = self.base.path().trim_end_matches('/').len();
                let appended = requested
                    .path()
                    .get(base_len..)
                    .unwrap_or_default()
                    .trim_end_matches('/');
                if !appended.is_empty() {
                    let kept = target
                        .path()
                        .trim_end_matches('/')
                        .strip_suffix(appended)
                        .map(str::to_owned);
                    if let Some(kept) = kept {
                        target.set_path(&kept);
                    }
                }
                redact_url(target.as_str()).trim_end_matches('/').to_owned()
            }
        };
        ImmichError::Redirected { to }
    }

    fn lock_health(&self) -> MutexGuard<'_, Health> {
        self.health.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remember_ok(&self) {
        let mut health = self.lock_health();
        health.last_ok_at = Some(now_secs());
        health.last_error = None;
        health.key_rejected = false;
    }

    fn remember_error(&self, error: &ImmichError) {
        let mut health = self.lock_health();
        health.last_error = Some(error.summary());
        if matches!(error, ImmichError::Unauthorized) && self.key_header.is_some() {
            health.key_rejected = true;
        }
    }
}

/// The result of one search request.
enum Attempt {
    Page(SearchPage),
    /// The server does not speak the typed form; the error is what to report if the flat form
    /// fails as well.
    WrongStyle(ImmichError),
}

/// Whether an error says something is wrong with the server or the connection (as opposed to
/// with one request): that is what the health records.
fn shows_trouble(error: &ImmichError) -> bool {
    match error {
        ImmichError::Unreachable(_)
        | ImmichError::Unauthorized
        | ImmichError::Redirected { .. }
        | ImmichError::Protocol(_) => true,
        ImmichError::Upstream { status, .. } => *status >= 500,
        _ => false,
    }
}

/// Whether an error is an answer of the server that refuses just this request: the connection
/// works.
fn shows_contact(error: &ImmichError) -> bool {
    match error {
        ImmichError::Forbidden | ImmichError::NotFound => true,
        ImmichError::Upstream { status, .. } => *status < 500,
        _ => false,
    }
}

/// Whether `id` is shaped like an Immich id (a UUID): short, and made of characters that can go
/// into a URL path segment as they are.
pub(crate) fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Checks an id before it becomes a path segment.
fn safe_id(id: &str) -> Result<&str, ImmichError> {
    if is_safe_id(id) {
        Ok(id)
    } else {
        Err(ImmichError::invalid_field("id", "not a valid Immich id"))
    }
}

/// Normalises what the user typed as the server address into the base URL of the client.
///
/// Accepted: `http://` or `https://` addresses (no scheme is guessed). Dropped: user info, query,
/// fragment, trailing slashes and a trailing `/api` (people paste the API address). A path prefix
/// of a reverse proxy (`https://host/immich`) is kept. The error texts never repeat the input,
/// which may hold a password.
pub fn normalize_base_url(input: &str) -> Result<Url, ImmichError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ImmichError::InvalidUrl("the address is empty".into()));
    }
    let lowered = input.to_ascii_lowercase();
    if !(lowered.starts_with("http://") || lowered.starts_with("https://")) {
        return Err(ImmichError::InvalidUrl(
            "the address must start with http:// or https://".into(),
        ));
    }
    let mut url = Url::parse(input)
        .map_err(|_| ImmichError::InvalidUrl("the address is not a valid URL".into()))?;
    if url.host_str().is_none_or(str::is_empty) {
        return Err(ImmichError::InvalidUrl(
            "the address has no host name".into(),
        ));
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);

    let mut segments: Vec<String> = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if segments
        .last()
        .is_some_and(|segment| segment.eq_ignore_ascii_case("api"))
    {
        segments.pop();
    }
    url.set_path(&format!("/{}", segments.join("/")));
    Ok(url)
}

/// Reads a successful answer completely, failing when it is larger than `limit`.
async fn read_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, ImmichError> {
    let too_large = || ImmichError::Protocol("the response is larger than the limit".into());
    let limit_bytes = u64::try_from(limit).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|length| length > limit_bytes)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Reads at most `limit` bytes of an error answer; a read failure just ends it.
async fn read_capped(mut response: reqwest::Response, limit: usize) -> Vec<u8> {
    let mut body = Vec::new();
    while body.len() < limit {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = limit - body.len();
                body.extend_from_slice(chunk.get(..room.min(chunk.len())).unwrap_or_default());
            }
            Ok(None) | Err(_) => break,
        }
    }
    body
}

/// Turns a transport failure into text the user can act on, without URLs or secrets.
fn transport_error(error: reqwest::Error) -> ImmichError {
    if error.is_timeout() {
        let text = if error.is_connect() {
            "timed out while connecting"
        } else {
            "the request timed out"
        };
        return ImmichError::Unreachable(text.into());
    }
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let part = cause.to_string();
        if !text.contains(&part) {
            text.push_str(": ");
            text.push_str(&part);
        }
        source = cause.source();
    }
    let certificate = text.to_ascii_lowercase().contains("certificate");
    let mut message = truncate_chars(&redact_urls_in(&text), MAX_UPSTREAM_MESSAGE_CHARS - 80);
    if certificate {
        message.push_str(" (for a self-signed certificate, allow invalid certificates)");
    }
    ImmichError::Unreachable(message)
}

/// Maps an error status. 404 and the others carry no secret; the message of Immich is kept
/// (shortened) because it says what is wrong with the request.
fn status_error(status: StatusCode, body: &[u8]) -> ImmichError {
    match status {
        StatusCode::UNAUTHORIZED => ImmichError::Unauthorized,
        StatusCode::FORBIDDEN => ImmichError::Forbidden,
        StatusCode::NOT_FOUND => ImmichError::NotFound,
        _ => ImmichError::Upstream {
            status: status.as_u16(),
            message: upstream_message(status, body),
        },
    }
}

/// The `message` of a NestJS error body (a string, or a list of strings for validation errors).
/// Other bodies give their text unless they look like HTML; an empty one the reason phrase.
fn upstream_message(status: StatusCode, body: &[u8]) -> String {
    let from_json = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(
            |value| match value.get("message").or_else(|| value.get("error")) {
                Some(Value::String(text)) => Some(text.clone()),
                Some(Value::Array(parts)) => Some(
                    parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
                _ => None,
            },
        );
    let text = from_json.unwrap_or_else(|| {
        let plain = String::from_utf8_lossy(body);
        let plain = plain.trim();
        if plain.starts_with('<') {
            String::new()
        } else {
            plain.to_owned()
        }
    });
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if text.is_empty() {
        status.canonical_reason().unwrap_or("error").to_owned()
    } else {
        text
    };
    truncate_chars(&redact_urls_in(&text), MAX_UPSTREAM_MESSAGE_CHARS)
}
