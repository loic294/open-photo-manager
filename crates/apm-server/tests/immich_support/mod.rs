//! A stateful in-process Immich for the integration tests of the Immich endpoints, and the
//! [`Fixture`] that wires it to a real application.
//!
//! [`FakeImmich`] answers the endpoints the application uses the way Immich 3.4 does where it
//! matters for the connection:
//!
//! - a login session (`Authorization: Bearer`) may do everything, an API key (`x-api-key`) only
//!   what its permissions allow (403 `Missing required permission: ...` otherwise), and the
//!   library endpoints refuse an account that is not an administrator;
//! - a key that was deleted stops working at once (401), and so does a session that logged out;
//! - the secret of a key is only ever seen in the answer that creates it.
//!
//! Every request is recorded with the credential it carried, so a test can say what the
//! application sent and with which credential. A fault ([`FakeImmich::fail`]) makes one endpoint
//! answer an error status until [`FakeImmich::heal`].
//!
//! The fake runs on a wiremock `MockServer` built outside the pool: dropping it closes its port
//! ([`FakeImmich::stop`]), which is how a test makes Immich unreachable.
//!
//! `POST /search/metadata` follows the request generation of the client (the typed filter and a
//! cursor from Immich 3.2, the flat fields and a page number before) and shows the flags an asset
//! has; see `search.rs` for what a test can shape: flags, `updatedAt`, short, empty, slow and
//! failing pages.
//!
//! `assets.rs` adds what the outbox of file operations calls: one asset, bulk changes and deletes,
//! albums, tags and the scan of a library over real directories (mounted with
//! [`FakeImmich::mount`]). A forced delete of an asset that is online is recorded as a
//! [`Violation`]: every test that drives the application asserts there is none.
//!
//! [`Fixture`] adds a `TestApp`, the capture of the log of the test and the checks that no secret
//! reaches a response, the log or a file of the data directory; `sync.rs` adds what the tests of
//! the asset sync need on top of it and `outbox.rs` what the tests of the outbox need.

#![allow(dead_code, unused_imports, clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use apm_server::config::Config;
use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::common::TestApp;
use crate::common::seed::Seed;

mod assets;
mod outbox;
mod search;
mod sync;

pub use assets::{Album, Tag, Violation};
pub use outbox::{
    OutboxRow, PassState, SHORT, Site, delete_of, first_pass, id_of, many_photos, moved_photo,
    next_pass, one_photo, one_photo_limited, photos_in,
};
pub use search::{START_MS, rfc3339};
pub use sync::{StoredAsset, StoredScope, eventually, updated_after};

pub const EMAIL: &str = "me@example.org";
pub const USER_NAME: &str = "Test User";
pub const VERSION: &str = "v3.4.0";
const TIMESTAMP: &str = "2025-01-01T00:00:00.000Z";

static COUNTER: AtomicU64 = AtomicU64::new(1);
static NULL: Value = Value::Null;

fn next_number() -> u64 {
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A secret no other secret of this test run contains: the prefix, the process and a fixed-width
/// counter, so one secret is never a prefix of another.
pub fn unique_secret(prefix: &str) -> String {
    format!("{prefix}.{}.{:06}", std::process::id(), next_number())
}

/// A UUID-shaped id, unique in the test run.
pub fn new_id() -> String {
    format!("00000000-0000-4000-8000-{:012x}", next_number())
}

// ----------------------------------------------------------------------------
// The state of the fake
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub secret: String,
    pub permissions: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Library {
    pub id: String,
    pub name: String,
    pub import_paths: Vec<String>,
    pub exclusion_patterns: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub id: String,
    pub library_id: String,
    pub original_path: String,
    /// Immich found the file missing.
    pub is_offline: bool,
    pub is_trashed: bool,
    /// `timeline`, `archive`, `hidden` or `locked`.
    pub visibility: String,
    /// Epoch milliseconds.
    pub updated_at_ms: i64,
    pub is_favorite: bool,
    /// Stars, 1 to 5; `None` is no rating.
    pub rating: Option<i32>,
    pub description: Option<String>,
    /// The ids of the tags the asset has.
    pub tags: Vec<String>,
    /// Immich has read the metadata; an asset that was just imported may not have it yet.
    pub has_metadata: bool,
    /// The order the assets were added in, which is the order of a search.
    seq: u64,
}

impl Asset {
    fn file_name(&self) -> &str {
        self.original_path
            .rsplit('/')
            .next()
            .unwrap_or(&self.original_path)
    }
}

/// One request the fake received.
#[derive(Clone, Debug)]
pub struct Call {
    pub method: String,
    /// The whole path, `/api/...` included.
    pub path: String,
    /// The raw `Authorization` header.
    pub authorization: Option<String>,
    pub api_key: Option<String>,
    pub body: Option<Value>,
    pub status: u16,
}

impl Call {
    pub fn is(&self, method: &str, path: &str) -> bool {
        self.method == method && self.path == path
    }

    pub fn bearer(&self) -> Option<&str> {
        self.authorization.as_deref()?.strip_prefix("Bearer ")
    }
}

struct Fault {
    method: String,
    /// An exact path, or a prefix when it ends with `*`.
    path: String,
    status: u16,
}

impl Fault {
    fn matches(&self, method: &str, path: &str) -> bool {
        self.method == method
            && match self.path.strip_suffix('*') {
                Some(prefix) => path.starts_with(prefix),
                None => self.path == path,
            }
    }
}

pub struct State {
    pub version: String,
    pub is_admin: bool,
    pub email: String,
    pub password: String,
    pub user_id: String,
    /// The live login sessions (their bearer tokens).
    pub sessions: Vec<String>,
    /// The live API keys.
    pub keys: Vec<ApiKey>,
    pub libraries: Vec<Library>,
    pub assets: Vec<Asset>,
    pub calls: Vec<Call>,
    /// The password and every credential this server ever handed out, for the leak checks.
    pub issued: Vec<String>,
    /// How long every answer is held back: a network that is not the loopback. See
    /// [`FakeImmich::set_latency`].
    pub latency: Duration,
    faults: Vec<Fault>,
    /// How the searches behave on top of the data; see `search.rs`.
    search: search::Behaviour,
    /// The albums, the tags, the mounts and the forced deletes; see `assets.rs`.
    world: assets::World,
}

type Reply = (u16, Option<Value>);

fn failure(status: u16, error: &str, message: &str) -> Reply {
    (
        status,
        Some(json!({ "message": message, "error": error, "statusCode": status })),
    )
}

fn reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

struct Credentials {
    authorization: Option<String>,
    api_key: Option<String>,
}

impl Credentials {
    fn bearer(&self) -> Option<&str> {
        self.authorization.as_deref()?.strip_prefix("Bearer ")
    }
}

/// Who a request is.
enum Who {
    Nobody,
    /// A credential Immich does not know.
    Unknown,
    Session,
    Key(Vec<String>),
}

#[derive(Clone, Copy, Default)]
struct Need {
    permission: Option<&'static str>,
    admin: bool,
}

fn needs(permission: &'static str) -> Need {
    Need {
        permission: Some(permission),
        admin: false,
    }
}

impl Need {
    fn admin(self) -> Self {
        Self {
            admin: true,
            ..self
        }
    }
}

impl State {
    fn new(is_admin: bool) -> Self {
        let password = unique_secret("fake-password");
        Self {
            version: VERSION.to_owned(),
            is_admin,
            email: EMAIL.to_owned(),
            password: password.clone(),
            user_id: new_id(),
            sessions: Vec::new(),
            keys: Vec::new(),
            libraries: Vec::new(),
            assets: Vec::new(),
            calls: Vec::new(),
            issued: vec![password],
            latency: Duration::ZERO,
            faults: Vec::new(),
            search: search::Behaviour::default(),
            world: assets::World::default(),
        }
    }

    fn handle(&mut self, request: &Request) -> Reply {
        let header = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let credentials = Credentials {
            authorization: header("authorization"),
            api_key: header("x-api-key"),
        };
        let body = serde_json::from_slice::<Value>(&request.body).ok();
        let method = request.method.as_str().to_owned();
        let path = request.url.path().to_owned();
        let query: Vec<(String, String)> = request
            .url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let reply = self.answer(&method, &path, &query, &credentials, body.as_ref());
        self.calls.push(Call {
            method,
            path,
            authorization: credentials.authorization,
            api_key: credentials.api_key,
            body,
            status: reply.0,
        });
        reply
    }

    fn answer(
        &mut self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        credentials: &Credentials,
        body: Option<&Value>,
    ) -> Reply {
        if let Some(fault) = self.faults.iter().find(|fault| fault.matches(method, path)) {
            return failure(
                fault.status,
                reason(fault.status),
                "the fake Immich was told to fail",
            );
        }
        let Some(route) = path.strip_prefix("/api/") else {
            return failure(404, "Not Found", &format!("Cannot {method} {path}"));
        };
        let segments: Vec<&str> = route.split('/').collect();
        let body = body.unwrap_or(&NULL);
        if method == "POST" && segments == ["auth", "login"] {
            return self.login(body);
        }

        let who = self.who(credentials);
        match (method, segments.as_slice()) {
            ("POST", ["auth", "logout"]) => {
                if let Some(refusal) = self.guard(&who, Need::default()) {
                    return refusal;
                }
                if let Some(token) = credentials.bearer() {
                    self.sessions.retain(|session| session != token);
                }
                (
                    200,
                    Some(json!({ "successful": true, "redirectUri": "/auth/login?autoLaunch=0" })),
                )
            }
            ("POST", ["api-keys"]) => self.create_key(&who, body),
            ("DELETE", ["api-keys", id]) => self.delete_key(&who, id),
            ("GET", ["users", "me"]) => {
                if let Some(refusal) = self.guard(&who, needs("user.read")) {
                    return refusal;
                }
                (
                    200,
                    Some(json!({
                        "id": self.user_id,
                        "email": self.email,
                        "name": USER_NAME,
                        "isAdmin": self.is_admin,
                        "avatarColor": "primary",
                        "createdAt": TIMESTAMP,
                        "updatedAt": TIMESTAMP,
                    })),
                )
            }
            ("GET", ["server", "about"]) => {
                if let Some(refusal) = self.guard(&who, needs("server.about")) {
                    return refusal;
                }
                (
                    200,
                    Some(json!({ "version": self.version, "licensed": false })),
                )
            }
            ("GET", ["libraries"]) => {
                if let Some(refusal) = self.guard(&who, needs("library.read").admin()) {
                    return refusal;
                }
                let listed: Vec<Value> = self
                    .libraries
                    .iter()
                    .map(|library| self.library_json(library))
                    .collect();
                (200, Some(Value::Array(listed)))
            }
            ("GET", ["libraries", id]) => {
                if let Some(refusal) = self.guard(&who, needs("library.read").admin()) {
                    return refusal;
                }
                match self.libraries.iter().find(|library| library.id == *id) {
                    Some(library) => (200, Some(self.library_json(library))),
                    None => unknown_library(),
                }
            }
            ("PUT", ["libraries", id]) => self.update_library(&who, id, body),
            ("POST", ["libraries", id, "scan"]) => self.request_scan(&who, id),
            ("POST", ["search", "metadata"]) => self.search(&who, body),
            ("GET", ["assets", id]) => self.get_asset(&who, id),
            ("PUT", ["assets"]) => self.update_assets(&who, body),
            ("DELETE", ["assets"]) => self.delete_assets(&who, body),
            ("GET", ["albums"]) => self.list_albums(&who, query),
            ("PUT", ["albums", id, "assets"]) => self.add_to_album(&who, id, body),
            ("GET", ["tags"]) => self.list_tags(&who),
            ("PUT", ["tags", "assets"]) => self.tag_assets(&who, body),
            _ => failure(404, "Not Found", &format!("Cannot {method} {path}")),
        }
    }

    fn who(&self, credentials: &Credentials) -> Who {
        if let Some(token) = credentials.bearer() {
            return if self.sessions.iter().any(|session| session == token) {
                Who::Session
            } else {
                Who::Unknown
            };
        }
        if let Some(secret) = &credentials.api_key {
            return match self.keys.iter().find(|key| &key.secret == secret) {
                Some(key) => Who::Key(key.permissions.clone()),
                None => Who::Unknown,
            };
        }
        Who::Nobody
    }

    /// The refusal for a request that may not do what it asks, `None` when it may.
    fn guard(&self, who: &Who, need: Need) -> Option<Reply> {
        match who {
            Who::Nobody => return Some(failure(401, "Unauthorized", "Authentication required")),
            Who::Unknown => return Some(failure(401, "Unauthorized", "Invalid credentials")),
            Who::Key(permissions) => {
                if let Some(permission) = need.permission
                    && !permissions.iter().any(|granted| granted == permission)
                {
                    return Some(failure(
                        403,
                        "Forbidden",
                        &format!("Missing required permission: {permission}"),
                    ));
                }
            }
            Who::Session => {}
        }
        (need.admin && !self.is_admin).then(|| failure(403, "Forbidden", "Forbidden"))
    }

    fn login(&mut self, body: &Value) -> Reply {
        if body["email"].as_str() != Some(&self.email)
            || body["password"].as_str() != Some(&self.password)
        {
            return failure(401, "Unauthorized", "Incorrect email or password");
        }
        let token = unique_secret("fake-session");
        self.issued.push(token.clone());
        self.sessions.push(token.clone());
        (
            201,
            Some(json!({
                "accessToken": token,
                "userId": self.user_id,
                "userEmail": self.email,
                "name": USER_NAME,
                "isAdmin": self.is_admin,
                "isOnboarded": true,
                "profileImagePath": "",
                "shouldChangePassword": false,
            })),
        )
    }

    fn create_key(&mut self, who: &Who, body: &Value) -> Reply {
        if let Some(refusal) = self.guard(who, needs("apiKey.create")) {
            return refusal;
        }
        let Some(name) = body["name"].as_str() else {
            return failure(400, "Bad Request", "name must be a string");
        };
        let Some(permissions) = body["permissions"].as_array() else {
            return failure(400, "Bad Request", "permissions must be an array");
        };
        let key = ApiKey {
            id: new_id(),
            name: name.to_owned(),
            secret: unique_secret("fake-api-key"),
            permissions: permissions
                .iter()
                .filter_map(|permission| permission.as_str().map(str::to_owned))
                .collect(),
        };
        self.issued.push(key.secret.clone());
        let created = json!({
            "secret": key.secret,
            "apiKey": {
                "id": key.id,
                "name": key.name,
                "permissions": key.permissions,
                "createdAt": TIMESTAMP,
                "updatedAt": TIMESTAMP,
            },
        });
        self.keys.push(key);
        (201, Some(created))
    }

    fn delete_key(&mut self, who: &Who, id: &str) -> Reply {
        if let Some(refusal) = self.guard(who, needs("apiKey.delete")) {
            return refusal;
        }
        let before = self.keys.len();
        self.keys.retain(|key| key.id != id);
        if self.keys.len() == before {
            return failure(400, "Bad Request", "API Key not found");
        }
        (204, None)
    }

    fn update_library(&mut self, who: &Who, id: &str, body: &Value) -> Reply {
        if let Some(refusal) = self.guard(who, needs("library.update").admin()) {
            return refusal;
        }
        let Some(position) = self.libraries.iter().position(|library| library.id == id) else {
            return unknown_library();
        };
        let library = &mut self.libraries[position];
        if let Some(patterns) = body["exclusionPatterns"].as_array() {
            library.exclusion_patterns = patterns
                .iter()
                .filter_map(|pattern| pattern.as_str().map(str::to_owned))
                .collect();
        }
        if let Some(name) = body["name"].as_str() {
            library.name = name.to_owned();
        }
        (200, Some(self.library_json(&self.libraries[position])))
    }

    fn library_json(&self, library: &Library) -> Value {
        let assets = self
            .assets
            .iter()
            .filter(|asset| asset.library_id == library.id)
            .count();
        json!({
            "id": library.id,
            "name": library.name,
            "ownerId": self.user_id,
            "importPaths": library.import_paths,
            "exclusionPatterns": library.exclusion_patterns,
            "assetCount": assets,
            "createdAt": TIMESTAMP,
            "updatedAt": TIMESTAMP,
            "refreshedAt": null,
        })
    }
}

fn unknown_library() -> Reply {
    failure(400, "Bad Request", "Not found or no library.read access")
}

struct Responder(Arc<Mutex<State>>);

impl Respond for Responder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.0.lock().unwrap();
        let (status, body) = state.handle(request);
        let delay = state.search.delay_for(request) + state.latency;
        drop(state);
        let mut template = ResponseTemplate::new(status);
        if !delay.is_zero() {
            template = template.set_delay(delay);
        }
        match body {
            Some(body) => template.set_body_json(body),
            None => template,
        }
    }
}

// ----------------------------------------------------------------------------
// The fake server
// ----------------------------------------------------------------------------

pub struct FakeImmich {
    server: Option<MockServer>,
    address: SocketAddr,
    url: String,
    state: Arc<Mutex<State>>,
}

impl FakeImmich {
    /// Starts a fake whose one account is an administrator or not.
    pub async fn start(is_admin: bool) -> Self {
        let state = Arc::new(Mutex::new(State::new(is_admin)));
        let server = MockServer::builder().start().await;
        Mock::given(any())
            .respond_with(Responder(Arc::clone(&state)))
            .mount(&server)
            .await;
        Self {
            address: *server.address(),
            url: server.uri(),
            server: Some(server),
            state,
        }
    }

    /// The address to give the application, `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    /// The state, for a test that needs more than the helpers below. Never hold it across an
    /// `.await`: the server answers through the same lock.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn password(&self) -> String {
        self.state().password.clone()
    }

    /// Stops listening: from now on connecting to the address is refused.
    pub async fn stop(&mut self) {
        drop(self.server.take());
        let started = Instant::now();
        while TcpStream::connect_timeout(&self.address, Duration::from_millis(200)).is_ok() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the fake Immich did not stop listening"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn add_library(&self, name: &str, import_paths: &[&str]) -> String {
        let library = Library {
            id: new_id(),
            name: name.to_owned(),
            import_paths: import_paths.iter().map(|path| (*path).to_owned()).collect(),
            exclusion_patterns: Vec::new(),
        };
        let id = library.id.clone();
        self.state().libraries.push(library);
        id
    }

    pub fn add_asset(&self, library_id: &str, original_path: &str) -> String {
        self.add_asset_with(library_id, original_path, |_| {})
    }

    pub fn library(&self, id: &str) -> Library {
        self.state()
            .libraries
            .iter()
            .find(|library| library.id == id)
            .cloned()
            .expect("the fake Immich has no such library")
    }

    /// Makes `method path` answer `status` until [`FakeImmich::heal`]. `path` is the whole path
    /// (`/api/users/me`); a trailing `*` matches every path that starts with the rest.
    pub fn fail(&self, method: &str, path: &str, status: u16) {
        self.state().faults.push(Fault {
            method: method.to_owned(),
            path: path.to_owned(),
            status,
        });
    }

    /// Holds every answer back by `latency` (the time a request takes on a real network), until
    /// it is set to zero. Not a fault: [`FakeImmich::heal`] leaves it alone. The delay does not
    /// hold the others back, so requests that overlap wait together.
    pub fn set_latency(&self, latency: Duration) {
        self.state().latency = latency;
    }

    pub fn heal(&self) {
        let mut state = self.state();
        state.faults.clear();
        state.search.fail_after = None;
        state.search.delay = Duration::ZERO;
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state().calls.clone()
    }

    /// How many requests arrived since the last `clear_calls`, without copying them.
    pub fn call_count(&self) -> usize {
        self.state().calls.len()
    }

    pub fn calls_to(&self, method: &str, path: &str) -> Vec<Call> {
        self.state()
            .calls
            .iter()
            .filter(|call| call.is(method, path))
            .cloned()
            .collect()
    }

    pub fn count(&self, method: &str, path: &str) -> usize {
        self.calls_to(method, path).len()
    }

    /// `METHOD /api/path` of every request so far, in the order they arrived.
    pub fn sequence(&self) -> Vec<String> {
        self.state()
            .calls
            .iter()
            .map(|call| format!("{} {}", call.method, call.path))
            .collect()
    }

    pub fn clear_calls(&self) {
        self.state().calls.clear();
    }

    pub fn keys(&self) -> Vec<ApiKey> {
        self.state().keys.clone()
    }

    pub fn key_count(&self) -> usize {
        self.state().keys.len()
    }

    pub fn session_count(&self) -> usize {
        self.state().sessions.len()
    }

    /// Deletes every API key, as an administrator does in the Immich web interface.
    pub fn revoke_all_keys(&self) {
        self.state().keys.clear();
    }

    /// The password and every credential handed out so far.
    pub fn secrets(&self) -> Vec<String> {
        self.state().issued.clone()
    }
}

// ----------------------------------------------------------------------------
// Capturing the log
// ----------------------------------------------------------------------------

/// Collects the log lines of one test.
#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<Vec<u8>>>);

thread_local! {
    static CAPTURING: RefCell<Option<LogBuffer>> = const { RefCell::new(None) };
}

/// Writes each event to the buffer of the thread that emitted it, if any.
struct ThreadWriter;

impl io::Write for ThreadWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        CAPTURING.with(|slot| {
            if let Some(logs) = slot.borrow().as_ref() {
                logs.0.lock().unwrap().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Stops the capture of the current thread when dropped.
pub struct CaptureGuard;

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURING.with(|slot| *slot.borrow_mut() = None);
    }
}

impl LogBuffer {
    /// Routes the events of the current thread (the whole test, on the current-thread runtime of
    /// `#[tokio::test]`, spawned tasks included) into this buffer, down to debug level. Keep the
    /// guard alive for the length of the test.
    ///
    /// One subscriber serves the whole test binary: with one per test, `tracing` caches whether a
    /// callsite is enabled from whichever thread reaches it first.
    pub fn capture(&self) -> CaptureGuard {
        static SUBSCRIBER: Once = Once::new();
        SUBSCRIBER.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .without_time()
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(|| ThreadWriter)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("no other subscriber is installed by these tests");
        });
        CAPTURING.with(|slot| *slot.borrow_mut() = Some(self.clone()));
        CaptureGuard
    }

    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

// ----------------------------------------------------------------------------
// Looking for secrets at rest
// ----------------------------------------------------------------------------

/// Every file below `dir` (the database and its write-ahead log included) that contains `needle`.
pub fn files_containing(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    fn walk(dir: &Path, needle: &[u8], hits: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, needle, hits);
            } else if let Ok(bytes) = std::fs::read(&path)
                && bytes.windows(needle.len()).any(|window| window == needle)
            {
                hits.push(path);
            }
        }
    }
    assert!(!needle.is_empty(), "an empty needle is in every file");
    let mut hits = Vec::new();
    walk(dir, needle, &mut hits);
    hits
}

/// The kind of a secret, for messages that must not repeat the secret.
fn label(secret: &str) -> &str {
    match secret.split_once('.') {
        Some((kind, _)) if kind.starts_with("fake-") => kind,
        _ => "watched secret",
    }
}

// ----------------------------------------------------------------------------
// The fixture
// ----------------------------------------------------------------------------

/// A real application, a fake Immich and the log of the test.
pub struct Fixture {
    pub app: TestApp,
    pub fake: FakeImmich,
    pub logs: LogBuffer,
    watched: Mutex<Vec<String>>,
    responses: Mutex<Vec<String>>,
    _capture: CaptureGuard,
}

impl Fixture {
    /// An application and a fake whose account is an administrator, not connected yet.
    pub async fn new() -> Self {
        Self::start(true, |_| {}).await
    }

    /// Like [`Fixture::new`] with an account that is not an administrator.
    pub async fn limited() -> Self {
        Self::start(false, |_| {}).await
    }

    /// An administrator connection, made through the endpoint.
    pub async fn connected() -> Self {
        let fixture = Self::new().await;
        fixture.connect_ok().await;
        fixture
    }

    /// [`Fixture::connected`] on an application whose test `Config` `tweak` adjusted first.
    pub async fn connected_with(tweak: impl FnOnce(&mut Config)) -> Self {
        let fixture = Self::start(true, tweak).await;
        fixture.connect_ok().await;
        fixture
    }

    /// A connection of an account that is not an administrator.
    pub async fn connected_limited() -> Self {
        let fixture = Self::limited().await;
        fixture.connect_ok().await;
        fixture
    }

    async fn start(is_admin: bool, tweak: impl FnOnce(&mut Config)) -> Self {
        let logs = LogBuffer::default();
        let capture = logs.capture();
        let app = TestApp::with_config(tweak).await;
        let fake = FakeImmich::start(is_admin).await;
        Self {
            app,
            fake,
            logs,
            watched: Mutex::new(Vec::new()),
            responses: Mutex::new(Vec::new()),
            _capture: capture,
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        self.app.state.config.data_dir.clone()
    }

    /// The request body of `POST /api/immich/connect` with the right password.
    pub fn credentials(&self) -> Value {
        json!({
            "server_url": self.fake.url(),
            "email": EMAIL,
            "password": self.fake.password(),
        })
    }

    pub async fn connect(&self) -> (StatusCode, Value) {
        self.post("/api/immich/connect", self.credentials()).await
    }

    /// Connects and checks that it worked; returns the connection.
    pub async fn connect_ok(&self) -> Value {
        let (status, body) = self.connect().await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, value) = self.app.call(method, path, body).await;
        self.responses.lock().unwrap().push(value.to_string());
        (status, value)
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.call(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, path, Some(body)).await
    }

    pub async fn post_empty(&self, path: &str) -> (StatusCode, Value) {
        self.call(Method::POST, path, None).await
    }

    pub async fn put(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::PUT, path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> (StatusCode, Value) {
        self.call(Method::DELETE, path, None).await
    }

    /// `GET /api/immich/status`, which must be answered; `check` also asks Immich.
    pub async fn status(&self, check: bool) -> Value {
        let path = if check {
            "/api/immich/status?check=true"
        } else {
            "/api/immich/status"
        };
        let (status, body) = self.get(path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// `PUT /api/immich/mappings` with `entries` (see [`entry`]).
    pub async fn put_mappings(&self, entries: Vec<Value>) -> (StatusCode, Value) {
        self.put("/api/immich/mappings", json!({ "mappings": entries }))
            .await
    }

    /// Registers the volume `name` and returns its id.
    pub async fn volume(&self, name: &str) -> i64 {
        self.app.add_volume(name).await.0
    }

    /// Writes an indexed file of a volume into the database; its kind follows its extension.
    pub async fn index_file(&self, volume_id: i64, dir: &str, name: &str) -> i64 {
        Seed::new(&self.app.state.db)
            .file(volume_id, dir, name)
            .insert()
            .await
    }

    pub fn log(&self) -> String {
        self.logs.contents()
    }

    /// Adds a secret the checks of [`Fixture::assert_no_leak`] must not find anywhere (a password
    /// the test typed, for example).
    pub fn watch(&self, secret: &str) {
        self.watched.lock().unwrap().push(secret.to_owned());
    }

    /// Checks that no credential of the fake, no password and no watched secret is in the log, in
    /// a response the application sent, or in a file of the data directory.
    pub fn assert_no_leak(&self) {
        let mut secrets = self.fake.secrets();
        secrets.extend(self.watched.lock().unwrap().iter().cloned());
        let log = self.log();
        let responses = self.responses.lock().unwrap().clone();
        let data_dir = self.data_dir();
        for secret in &secrets {
            let kind = label(secret);
            assert!(!log.contains(secret.as_str()), "a {kind} reached the log");
            assert!(
                !responses.iter().any(|body| body.contains(secret.as_str())),
                "a {kind} reached a response"
            );
            let files = files_containing(&data_dir, secret.as_bytes());
            assert!(
                files.is_empty(),
                "a {kind} is stored in the clear: {files:?}"
            );
        }
    }
}

/// One entry of the `mappings` of `PUT /api/immich/mappings`.
pub fn entry(volume_id: i64, library_id: Option<&str>, path_prefix: &str) -> Value {
    json!({
        "volume_id": volume_id,
        "library_id": library_id,
        "path_prefix": path_prefix,
    })
}

/// The `field` of every problem of a 422 body, in order.
pub fn problem_fields(body: &Value) -> Vec<String> {
    body["error"]["fields"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .map(|field| field["field"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}
