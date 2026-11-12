//! The search of the fake Immich (`POST /search/metadata`) and what a test can shape about it.
//!
//! Both request generations of the client are answered the way a server of that generation
//! answers (see `client/search.rs`):
//!
//! - the typed one (Immich 3.2 and later): `filter` with `libraryId`, `trashedAt`, `isOffline`,
//!   `updatedAt` (`gt`, a strict lower bound), `visibility`, `originalFileName` (`eq`) and
//!   `originalPath` (`startsWith`), paged by an opaque `cursor`. Without a `trashedAt` or a
//!   `visibility` it returns trashed assets and every visibility an API key can see;
//! - the flat one (before 3.2): the same fields at the top, paged by `page`, answered with
//!   `nextPage`. It hides trashed assets (unless `withDeleted`, which `isOffline: true` implies)
//!   and every visibility but `archive` and `timeline` unless one is asked for, and its name and
//!   path are substring matches.
//!
//! Assets of the `locked` visibility are never returned: an API key cannot see them.
//!
//! `size` is limited to 1000, as in the specification. Pages follow the order the assets were
//! added in; a cursor is the position of the last asset of the page before it, so assets that
//! come and go while a search is being read do not shift it.
//!
//! What a test shapes:
//!
//! - per asset: [`FakeImmich::add_asset_with`] and [`FakeImmich::update_asset`] (flags, visibility,
//!   `updatedAt`), [`FakeImmich::remove_asset`] for a hard delete;
//! - the clock `updatedAt` follows ([`FakeImmich::advance`]), frozen at 2025-01-01 until moved;
//!   an asset takes the clock when it is added and when [`FakeImmich::update_asset`] changes it;
//! - the pages: [`FakeImmich::page_cap`] (a server that returns fewer assets than `size`),
//!   [`FakeImmich::stutter`] (an empty page that is not the last before every page of assets);
//! - the answers: [`FakeImmich::hold_searches`] and [`FakeImmich::hold_searches_after`] (an answer
//!   arrives late, so a test can act while a page is in flight) and
//!   [`FakeImmich::fail_search_after`].

use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat};
use serde_json::{Value, json};
use wiremock::Request;

use super::{
    Asset, FakeImmich, Reply, State, TIMESTAMP, Who, failure, needs, new_id, next_number, reason,
};

const SEARCH: &str = "/api/search/metadata";

/// `updatedAt` of every asset before a test moves the clock: 2025-01-01T00:00:00.000Z.
pub const START_MS: i64 = 1_735_689_600_000;

/// How the searches behave on top of what the assets say.
pub(super) struct Behaviour {
    /// Epoch milliseconds.
    pub(super) clock_ms: i64,
    pub(super) page_cap: Option<usize>,
    pub(super) stutter: bool,
    pub(super) delay: Duration,
    /// How many searches have arrived, and how many of them (the first ones) are not held back.
    pub(super) searches: usize,
    pub(super) undelayed: usize,
    /// `(searches still answered, status)`: once they are used up, a search answers `status`.
    pub(super) fail_after: Option<(usize, u16)>,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            clock_ms: START_MS,
            page_cap: None,
            stutter: false,
            delay: Duration::ZERO,
            searches: 0,
            undelayed: 0,
            fail_after: None,
        }
    }
}

impl Behaviour {
    /// How long the answer to `request` is held back.
    pub(super) fn delay_for(&self, request: &Request) -> Duration {
        if request.method.as_str() == "POST"
            && request.url.path() == SEARCH
            && self.searches > self.undelayed
        {
            self.delay
        } else {
            Duration::ZERO
        }
    }
}

/// An instant as Immich writes it, `2025-01-01T00:00:00.000Z`.
pub fn rfc3339(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .expect("a time the fake can show")
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_rfc3339(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// A name or a path that must match.
enum Text {
    Equal(String),
    Prefix(String),
    Contains(String),
}

impl Text {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Equal(wanted) => value == wanted,
            Self::Prefix(wanted) => value.starts_with(wanted.as_str()),
            Self::Contains(wanted) => value.contains(wanted.as_str()),
        }
    }
}

/// What one search asks for, read from either request generation.
struct Query {
    flat: bool,
    library: Option<String>,
    name: Option<Text>,
    path: Option<Text>,
    hide_trashed: bool,
    offline: Option<bool>,
    updated_after_ms: Option<i64>,
    visibility: Option<String>,
    size: usize,
    /// The page of a flat search, from 1.
    page: usize,
    cursor: Option<String>,
}

impl Query {
    fn read(body: &Value) -> Result<Self, String> {
        let size = match body.get("size") {
            None | Some(Value::Null) => 250,
            Some(size) => size
                .as_u64()
                .filter(|size| (1..=1000).contains(size))
                .and_then(|size| usize::try_from(size).ok())
                .ok_or("size must not be greater than 1000 or less than 1")?,
        };
        if body.get("filter").is_some() {
            Self::typed(body, size)
        } else {
            Self::flat(body, size)
        }
    }

    fn typed(body: &Value, size: usize) -> Result<Self, String> {
        let filter = &body["filter"];
        let text =
            |field: &str, operator: &str| filter[field][operator].as_str().map(str::to_owned);
        Ok(Self {
            flat: false,
            library: text("libraryId", "eq"),
            name: text("originalFileName", "eq").map(Text::Equal),
            path: text("originalPath", "startsWith").map(Text::Prefix),
            hide_trashed: filter
                .get("trashedAt")
                .is_some_and(|trashed| trashed.get("eq").is_some_and(Value::is_null)),
            offline: filter["isOffline"]["eq"].as_bool(),
            updated_after_ms: time(&filter["updatedAt"]["gt"], "updatedAt")?,
            visibility: text("visibility", "eq"),
            size,
            page: 1,
            cursor: body["cursor"].as_str().map(str::to_owned),
        })
    }

    fn flat(body: &Value, size: usize) -> Result<Self, String> {
        let text = |field: &str| body[field].as_str().map(str::to_owned);
        let shows_trashed =
            body["withDeleted"] == Value::Bool(true) || body["isOffline"] == Value::Bool(true);
        let page = match body.get("page") {
            None | Some(Value::Null) => 1,
            Some(page) => page
                .as_u64()
                .filter(|page| *page >= 1)
                .and_then(|page| usize::try_from(page).ok())
                .ok_or("page must not be less than 1")?,
        };
        Ok(Self {
            flat: true,
            library: text("libraryId"),
            name: text("originalFileName").map(Text::Contains),
            path: text("originalPath").map(Text::Contains),
            hide_trashed: !shows_trashed,
            offline: body["isOffline"].as_bool(),
            updated_after_ms: time(&body["updatedAfter"], "updatedAfter")?,
            visibility: text("visibility"),
            size,
            page,
            cursor: None,
        })
    }

    fn matches(&self, asset: &Asset) -> bool {
        self.library
            .as_deref()
            .is_none_or(|library| asset.library_id == library)
            && self
                .name
                .as_ref()
                .is_none_or(|name| name.matches(asset.file_name()))
            && self
                .path
                .as_ref()
                .is_none_or(|path| path.matches(&asset.original_path))
            && !(self.hide_trashed && asset.is_trashed)
            && self
                .offline
                .is_none_or(|offline| asset.is_offline == offline)
            && self
                .updated_after_ms
                .is_none_or(|after| asset.updated_at_ms > after)
            && self.shows(&asset.visibility)
    }

    fn shows(&self, visibility: &str) -> bool {
        visibility != "locked"
            && match &self.visibility {
                Some(wanted) => wanted == visibility,
                None => !self.flat || matches!(visibility, "timeline" | "archive"),
            }
    }
}

fn time(value: &Value, field: &str) -> Result<Option<i64>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => parse_rfc3339(text)
            .map(Some)
            .ok_or_else(|| format!("{field} must be a valid ISO 8601 date string")),
        _ => Err(format!("{field} must be a valid ISO 8601 date string")),
    }
}

/// A typed cursor: the position of the last asset before it, and whether it only stands for an
/// empty page of a stuttering server.
fn read_cursor(raw: &str) -> Option<(u64, bool)> {
    let rest = raw.strip_prefix("seq:")?;
    match rest.strip_suffix(":pause") {
        Some(position) => Some((position.parse().ok()?, true)),
        None => Some((rest.parse().ok()?, false)),
    }
}

impl State {
    pub(super) fn search(&mut self, who: &Who, body: &Value) -> Reply {
        if let Some(refusal) = self.guard(who, needs("asset.read")) {
            return refusal;
        }
        let query = match Query::read(body) {
            Ok(query) => query,
            Err(message) => return failure(400, "Bad Request", &message),
        };
        self.search.searches += 1;
        if let Some((remaining, status)) = &mut self.search.fail_after {
            if *remaining == 0 {
                let status = *status;
                return failure(
                    status,
                    reason(status),
                    "the fake Immich was told to fail the searches",
                );
            }
            *remaining -= 1;
        }

        let matching: Vec<&Asset> = self
            .assets
            .iter()
            .filter(|asset| query.matches(asset))
            .collect();
        let take = query
            .size
            .min(self.search.page_cap.unwrap_or(usize::MAX).max(1));

        if query.flat {
            let start = (query.page - 1).saturating_mul(take);
            let page: Vec<&Asset> = matching.iter().skip(start).take(take).copied().collect();
            let next_page =
                (matching.len() > start.saturating_add(take)).then(|| (query.page + 1).to_string());
            return self.page(&page, matching.len(), None, next_page);
        }

        let (after, paused) = match query.cursor.as_deref() {
            None => (0, false),
            Some(raw) => match read_cursor(raw) {
                Some(cursor) => cursor,
                None => return failure(400, "Bad Request", "cursor is not valid"),
            },
        };
        if self.search.stutter && !paused {
            return self.page(
                &[],
                matching.len(),
                Some(format!("seq:{after}:pause")),
                None,
            );
        }
        let rest: Vec<&Asset> = matching
            .iter()
            .filter(|asset| asset.seq > after)
            .copied()
            .collect();
        let page: Vec<&Asset> = rest.iter().take(take).copied().collect();
        let next_cursor = match page.last() {
            Some(last) if rest.len() > page.len() => Some(format!("seq:{}", last.seq)),
            _ => None,
        };
        self.page(&page, matching.len(), next_cursor, None)
    }

    /// The answer holding `page`. A flat answer has `nextPage` and no `nextCursor`, a typed one
    /// the other way round.
    fn page(
        &self,
        page: &[&Asset],
        total: usize,
        next_cursor: Option<String>,
        next_page: Option<String>,
    ) -> Reply {
        let items: Vec<Value> = page.iter().map(|asset| self.asset_json(asset)).collect();
        let mut assets = json!({
            "total": total,
            "count": items.len(),
            "items": items,
            "facets": [],
        });
        match (next_cursor, next_page) {
            (None, None) => {
                assets["nextCursor"] = Value::Null;
                assets["nextPage"] = Value::Null;
            }
            (cursor, page) => {
                if let Some(cursor) = cursor {
                    assets["nextCursor"] = json!(cursor);
                }
                if let Some(page) = page {
                    assets["nextPage"] = json!(page);
                }
            }
        }
        (
            200,
            Some(json!({
                "albums": { "total": 0, "count": 0, "items": [], "facets": [] },
                "assets": assets,
            })),
        )
    }

    pub(super) fn asset_json(&self, asset: &Asset) -> Value {
        json!({
            "id": asset.id,
            "libraryId": asset.library_id,
            "ownerId": self.user_id,
            "originalPath": asset.original_path,
            "originalFileName": asset.file_name(),
            "checksum": "AAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "isOffline": asset.is_offline,
            "isTrashed": asset.is_trashed,
            "isFavorite": asset.is_favorite,
            "visibility": asset.visibility,
            "type": "IMAGE",
            "fileCreatedAt": TIMESTAMP,
            "updatedAt": rfc3339(asset.updated_at_ms),
            "deletedAt": asset.is_trashed.then(|| rfc3339(asset.updated_at_ms)),
        })
    }
}

impl FakeImmich {
    /// Adds an asset like [`FakeImmich::add_asset`] and lets `change` set what a plain one lacks:
    /// offline, trashed, another visibility, another `updatedAt`.
    pub fn add_asset_with(
        &self,
        library_id: &str,
        original_path: &str,
        change: impl FnOnce(&mut Asset),
    ) -> String {
        let mut state = self.state();
        let mut asset = Asset {
            id: new_id(),
            library_id: library_id.to_owned(),
            original_path: original_path.to_owned(),
            is_offline: false,
            is_trashed: false,
            visibility: "timeline".to_owned(),
            updated_at_ms: state.search.clock_ms,
            is_favorite: false,
            rating: None,
            description: None,
            tags: Vec::new(),
            has_metadata: true,
            seq: next_number(),
        };
        change(&mut asset);
        let id = asset.id.clone();
        state.assets.push(asset);
        id
    }

    /// Changes an asset the way Immich does, which stamps it: `updatedAt` takes the clock unless
    /// `change` set it itself.
    pub fn update_asset(&self, id: &str, change: impl FnOnce(&mut Asset)) {
        let mut state = self.state();
        let clock = state.search.clock_ms;
        let asset = state
            .assets
            .iter_mut()
            .find(|asset| asset.id == id)
            .expect("the fake Immich has no such asset");
        let before = asset.updated_at_ms;
        change(asset);
        if asset.updated_at_ms == before {
            asset.updated_at_ms = clock;
        }
    }

    /// Deletes an asset for good: no search finds it again, and nothing says it is gone.
    pub fn remove_asset(&self, id: &str) {
        self.state().assets.retain(|asset| asset.id != id);
    }

    pub fn asset(&self, id: &str) -> Asset {
        self.state()
            .assets
            .iter()
            .find(|asset| asset.id == id)
            .cloned()
            .expect("the fake Immich has no such asset")
    }

    /// The clock `updatedAt` follows, epoch milliseconds.
    pub fn now_ms(&self) -> i64 {
        self.state().search.clock_ms
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        let by = i64::try_from(by.as_millis()).expect("a step the clock can take");
        self.state().search.clock_ms += by;
    }

    /// A server that never returns more than `cap` assets in a page, whatever `size` says.
    pub fn page_cap(&self, cap: Option<usize>) {
        self.state().search.page_cap = cap;
    }

    /// With `on`, every page of assets comes after an empty page that does not end the search.
    pub fn stutter(&self, on: bool) {
        self.state().search.stutter = on;
    }

    /// Holds each answer to a search back by `delay`; the request is recorded when it arrives.
    pub fn hold_searches(&self, delay: Duration) {
        self.hold_searches_after(0, delay);
    }

    /// Like [`FakeImmich::hold_searches`], after `answered` more searches that are answered at
    /// once. [`FakeImmich::heal`] lets every answer through again.
    pub fn hold_searches_after(&self, answered: usize, delay: Duration) {
        let mut state = self.state();
        state.search.undelayed = state.search.searches + answered;
        state.search.delay = delay;
    }

    /// After `answered` more searches, every search answers `status` until [`FakeImmich::heal`].
    pub fn fail_search_after(&self, answered: usize, status: u16) {
        self.state().search.fail_after = Some((answered, status));
    }

    /// The bodies of the searches received so far, in order.
    pub fn search_bodies(&self) -> Vec<Value> {
        self.calls_to("POST", SEARCH)
            .into_iter()
            .map(|call| call.body.unwrap_or(Value::Null))
            .collect()
    }

    /// Waits until at least `count` searches have arrived.
    pub async fn wait_for_searches(&self, count: usize) {
        let started = Instant::now();
        while self.count("POST", SEARCH) < count {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the fake Immich received {} searches, not {count}",
                self.count("POST", SEARCH)
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
