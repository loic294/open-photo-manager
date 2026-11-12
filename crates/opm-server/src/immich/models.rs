//! Serde models of the Immich API payloads the client sends and receives.
//!
//! # Tolerance
//!
//! Immich changes quickly, so every *response* model is read tolerantly: unknown
//! fields are ignored, optional fields may be missing or `null`, and an enum value
//! this build does not know becomes an `Unknown` variant instead of failing a whole
//! page of assets. Only what the application uses is modelled. *Request* types only
//! ever put values they know on the wire (an `Unknown` variant is never sent).
//!
//! # Time
//!
//! Immich timestamps have millisecond precision and the sync keeps its high-water
//! mark in them, so every timestamp in this module is **epoch milliseconds**
//! (fields end in `_ms`), unlike the rest of the application, which stores epoch
//! seconds. [`parse_rfc3339_ms`] and [`format_rfc3339_ms`] cross the wire.
//!
//! # Where the API shapes come from
//!
//! The OpenAPI spec of Immich v3.4.0-rc.0 ([`SPEC_VERSION`]). Older servers are
//! covered where the shape differs, see [`MetadataSearch`].

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::security::Redacted;

use super::error::ImmichError;

/// The Immich release whose OpenAPI spec the client is written against. A server
/// with a newer major version works but is reported as "newer than tested".
pub const SPEC_VERSION: ImmichVersion = ImmichVersion::new(3, 4, 0);

/// The first release that takes the typed `filter` / `orderBy` / `cursor` search
/// body. Older servers only know the flat form (`libraryId`, `page`, ...), which
/// the spec marks deprecated since this version.
pub const SEARCH_FILTER_SINCE: ImmichVersion = ImmichVersion::new(3, 2, 0);

/// The default page size of [`MetadataSearch`].
pub const DEFAULT_PAGE_SIZE: u32 = 250;

/// The most assets one search page may hold (the spec's `size` maximum).
pub const MAX_PAGE_SIZE: u32 = 1000;

/// A parsed Immich server version.
///
/// Compares as `(major, minor, patch)`; a pre-release suffix such as `-rc.0` is
/// dropped when parsing, so `3.2.0-rc.0` equals `3.2.0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub struct ImmichVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ImmichVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses `v3.4.0-rc.0`, `3.4.0` or a short `v2.1` (missing parts are 0).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let numeric = text
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .next()?;
        let mut parts = numeric.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().map_or(Some(0), |part| part.parse().ok())?;
        let patch = parts.next().map_or(Some(0), |part| part.parse().ok())?;
        Some(Self::new(major, minor, patch))
    }

    /// The one place that compares versions for the search request shape:
    /// `true` from [`SEARCH_FILTER_SINCE`] on (typed `filter` + `cursor`), `false`
    /// before it (flat fields + `page`).
    pub fn supports_search_filter(self) -> bool {
        self >= SEARCH_FILTER_SINCE
    }
}

impl fmt::Display for ImmichVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Parses an RFC 3339 timestamp into epoch milliseconds.
pub fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|moment| moment.timestamp_millis())
}

/// Formats epoch milliseconds the way Immich writes timestamps
/// (`2024-01-01T00:00:00.000Z`). An out-of-range value becomes the epoch.
pub fn format_rfc3339_ms(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn timestamp_ms(value: Option<Value>) -> Option<i64> {
    match value? {
        Value::String(text) => parse_rfc3339_ms(&text),
        _ => None,
    }
}

/// What an asset is (`AssetTypeEnum`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub enum AssetType {
    #[serde(rename = "IMAGE")]
    Image,
    #[serde(rename = "VIDEO")]
    Video,
    #[serde(rename = "AUDIO")]
    Audio,
    #[serde(rename = "OTHER")]
    Other,
    /// A value this build does not know.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Where an asset shows up in Immich (`AssetVisibility`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetVisibility {
    Archive,
    #[default]
    Timeline,
    Hidden,
    /// In the locked folder. Needs a PIN session, so an API key never sees it.
    Locked,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

impl AssetVisibility {
    /// The value put on the wire, `None` for [`AssetVisibility::Unknown`].
    pub fn wire(self) -> Option<&'static str> {
        match self {
            Self::Archive => Some("archive"),
            Self::Timeline => Some("timeline"),
            Self::Hidden => Some("hidden"),
            Self::Locked => Some("locked"),
            Self::Unknown => None,
        }
    }
}

/// Sort direction of a search (always by capture time, `id` breaks ties).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

impl SortOrder {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// Which assets a search keeps by their `isOffline` flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OfflineFilter {
    /// No filter: online and offline assets.
    #[default]
    Any,
    OnlyOffline,
    OnlyOnline,
}

/// A tag on an asset (`TagResponseDto`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ImmichTag {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// The full path of the tag (`Parent/Child`); `name` is the last part.
    #[serde(default)]
    pub value: String,
}

/// The part of `exifInfo` the application uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExifInfo {
    /// Star rating as Immich stores it: 1 to 5, -1 for "rejected", `None` for unrated.
    pub rating: Option<i32>,
    /// The image description, as written; `None` when the asset has none.
    pub description: Option<String>,
}

/// A trimmed view of Immich's `AssetResponseDto`: what the sync and the outbox
/// need, every other field ignored.
///
/// `tags` and `exif_info` are only filled by `GET /assets/{id}` (and by a search
/// with `with_exif`); a plain search page may leave them empty, so read them with
/// `ImmichClient::get_asset` before relying on them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "RawAsset")]
pub struct ImmichAsset {
    pub id: String,
    /// Base64 SHA-1 of the original file, as Immich reports it.
    pub checksum: String,
    /// The path inside the Immich container, never normalised. Map it with `PathMap`.
    pub original_path: String,
    pub original_file_name: String,
    /// The external library the asset belongs to; `None` when the response lacks it.
    pub library_id: Option<String>,
    /// Immich found the file missing during a library scan. The only state in which
    /// an asset may be deleted through the API (see [`OfflineAssetId`]).
    pub is_offline: bool,
    pub is_trashed: bool,
    pub deleted_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub file_created_at_ms: Option<i64>,
    pub asset_type: AssetType,
    pub is_favorite: bool,
    pub visibility: AssetVisibility,
    pub tags: Vec<ImmichTag>,
    pub exif_info: Option<ExifInfo>,
    /// Immich finished reading the file's metadata (its import job ran). A server that does not
    /// report the flag counts as done, so only a server that says otherwise makes callers wait;
    /// an asset built by hand (`Default`) has none yet.
    pub has_metadata: bool,
}

impl ImmichAsset {
    /// In Immich's trash (whichever field the server version reports it in).
    pub fn trashed(&self) -> bool {
        self.is_trashed || self.deleted_at_ms.is_some()
    }

    /// Present in the library and not trashed: what "live in Immich" means.
    pub fn is_live(&self) -> bool {
        !self.is_offline && !self.trashed()
    }

    /// The star rating in the application's terms: 1 to 5, or 0 for unrated
    /// (no rating, "rejected" or an out-of-range value).
    pub fn rating(&self) -> u8 {
        match self.exif_info.as_ref().and_then(|exif| exif.rating) {
            Some(stars @ 1..=5) => u8::try_from(stars).unwrap_or(0),
            _ => 0,
        }
    }

    /// The description, `None` when there is none or it is blank.
    pub fn description(&self) -> Option<&str> {
        self.exif_info
            .as_ref()
            .and_then(|exif| exif.description.as_deref())
            .filter(|text| !text.trim().is_empty())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAsset {
    id: String,
    checksum: Option<String>,
    original_path: Option<String>,
    original_file_name: Option<String>,
    library_id: Option<String>,
    is_offline: Option<bool>,
    is_trashed: Option<bool>,
    deleted_at: Option<Value>,
    updated_at: Option<Value>,
    file_created_at: Option<Value>,
    #[serde(rename = "type")]
    asset_type: Option<AssetType>,
    is_favorite: Option<bool>,
    visibility: Option<AssetVisibility>,
    tags: Option<Vec<ImmichTag>>,
    exif_info: Option<RawExif>,
    has_metadata: Option<bool>,
}

#[derive(Deserialize)]
struct RawExif {
    rating: Option<Value>,
    description: Option<Value>,
}

impl From<RawAsset> for ImmichAsset {
    fn from(raw: RawAsset) -> Self {
        Self {
            id: raw.id,
            checksum: raw.checksum.unwrap_or_default(),
            original_path: raw.original_path.unwrap_or_default(),
            original_file_name: raw.original_file_name.unwrap_or_default(),
            library_id: raw.library_id,
            is_offline: raw.is_offline.unwrap_or(false),
            is_trashed: raw.is_trashed.unwrap_or(false),
            deleted_at_ms: timestamp_ms(raw.deleted_at),
            updated_at_ms: timestamp_ms(raw.updated_at),
            file_created_at_ms: timestamp_ms(raw.file_created_at),
            asset_type: raw.asset_type.unwrap_or_default(),
            is_favorite: raw.is_favorite.unwrap_or(false),
            visibility: raw.visibility.unwrap_or_default(),
            tags: raw.tags.unwrap_or_default(),
            exif_info: raw.exif_info.map(|exif| ExifInfo {
                rating: exif
                    .rating
                    .and_then(|rating| rating.as_i64())
                    .and_then(|rating| i32::try_from(rating).ok()),
                description: match exif.description {
                    Some(Value::String(text)) => Some(text),
                    _ => None,
                },
            }),
            has_metadata: raw.has_metadata.unwrap_or(true),
        }
    }
}

/// The id of an asset Immich reports as **offline**: the only kind of asset
/// `ImmichClient::delete_assets` accepts.
///
/// Why: for an external-library asset whose file still exists, a force-delete (and
/// emptying Immich's trash) removes the ORIGINAL FILE from disk. The application
/// moves files into its own trash folder instead and lets Immich notice the file is
/// gone (the asset turns offline on the next library scan); only then is the
/// leftover record removed through the API. The type system enforces it: the value
/// can only be built from an asset whose `is_offline` flag is set, so a delete of
/// an online asset does not compile, let alone run.
///
/// Callers should fetch the asset again (`get_asset`) right before deleting, so
/// the flag is fresh.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OfflineAssetId(String);

impl OfflineAssetId {
    /// `Some` only when the asset is offline (and has an id).
    pub fn confirm(asset: &ImmichAsset) -> Option<Self> {
        (asset.is_offline && !asset.id.is_empty()).then(|| Self(asset.id.clone()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a [`MetadataSearch`] continues: opaque to callers, take it from
/// [`SearchPage::next`] and hand it back as `after`.
///
/// Which variant a page carries depends on the server generation (`Cursor` from
/// Immich 3.2, `Page` before), and a token only works against the style that
/// issued it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageToken {
    Page(u32),
    Cursor(String),
}

/// One search request, independent of the server generation.
///
/// The client turns it into the typed `filter` body (Immich 3.2 and later) or the
/// flat body (older servers); the semantics below hold for both:
///
/// - `library_id`: only assets of that external library. ALWAYS set it for a
///   library scan, the search universe also holds the account's partners' assets.
///   The client fails with a protocol error when a server ignores the filter.
/// - `updated_after_ms`: strictly after this instant (epoch ms). The server cannot
///   order by update time, so a scan must read every page before it may advance a
///   high-water mark.
/// - `include_trashed`: trashed assets are skipped unless this is set, on every
///   server generation. Immich trashes an asset when its file disappears from disk
///   and flags it offline at the same time, so reading offline assets (and the
///   deletions a scan has to notice) needs this to be set.
/// - `offline`: filters by the offline flag, [`OfflineFilter::Any`] keeps both.
/// - `visibility`: `None` is "the server default": the typed filter returns
///   everything the account can see (archived and hidden included), the flat form
///   of older servers only archived and timeline assets. Ask for
///   `Some(AssetVisibility::Hidden)` in an extra pass when hidden assets matter.
///   Locked assets are never returned to an API key.
/// - `original_file_name`: exact match.
/// - `original_path_prefix`: a plain string prefix of the original path; pass
///   `"<mapped prefix>/"` to select a directory. Old servers match it as a
///   substring and `_` or `%` act as wildcards on every server, so the client
///   removes every item whose path does not really start with the prefix.
/// - `with_exif`: also return `exif_info` (the rating) with the items.
/// - `size`: clamped to `1..=MAX_PAGE_SIZE`.
/// - `after`: the token of the previous page.
///
/// A page can hold fewer items than `size`, even none (the client drops items that
/// do not match the exact semantics above): stop only when `next` is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataSearch {
    pub library_id: Option<String>,
    pub updated_after_ms: Option<i64>,
    pub offline: OfflineFilter,
    pub include_trashed: bool,
    pub visibility: Option<AssetVisibility>,
    pub original_file_name: Option<String>,
    pub original_path_prefix: Option<String>,
    pub order: SortOrder,
    pub with_exif: bool,
    pub size: u32,
    pub after: Option<PageToken>,
}

impl Default for MetadataSearch {
    fn default() -> Self {
        Self {
            library_id: None,
            updated_after_ms: None,
            offline: OfflineFilter::Any,
            include_trashed: false,
            visibility: None,
            original_file_name: None,
            original_path_prefix: None,
            order: SortOrder::Asc,
            with_exif: false,
            size: DEFAULT_PAGE_SIZE,
            after: None,
        }
    }
}

impl MetadataSearch {
    /// A scan of one library, otherwise the defaults.
    pub fn in_library(library_id: impl Into<String>) -> Self {
        Self {
            library_id: Some(library_id.into()),
            ..Self::default()
        }
    }

    /// The page size actually sent.
    pub fn effective_size(&self) -> u32 {
        self.size.clamp(1, MAX_PAGE_SIZE)
    }
}

/// One page of search results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPage {
    pub items: Vec<ImmichAsset>,
    /// Where to continue; `None` after the last page.
    pub next: Option<PageToken>,
}

/// Fields to change on assets (`PUT /assets`). `None` leaves a field alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetUpdate {
    pub is_favorite: Option<bool>,
    /// The application's rating: 0 clears it (sent as `null`), 1 to 5 sets it.
    /// Anything else is refused before a request is made.
    pub rating: Option<u8>,
    pub visibility: Option<AssetVisibility>,
    pub description: Option<String>,
}

impl AssetUpdate {
    pub fn is_empty(&self) -> bool {
        self.is_favorite.is_none()
            && self.rating.is_none()
            && self.visibility.is_none()
            && self.description.is_none()
    }

    /// The JSON body for these assets, with every field validated.
    pub(super) fn body(&self, ids: &[String]) -> Result<Value, ImmichError> {
        let mut body = serde_json::Map::new();
        body.insert("ids".into(), Value::from(ids));
        if let Some(favorite) = self.is_favorite {
            body.insert("isFavorite".into(), Value::Bool(favorite));
        }
        if let Some(stars) = self.rating {
            let rating = match stars {
                0 => Value::Null,
                1..=5 => Value::from(stars),
                _ => {
                    return Err(ImmichError::invalid_field(
                        "rating",
                        "a rating is 0 (none) to 5 stars",
                    ));
                }
            };
            body.insert("rating".into(), rating);
        }
        if let Some(visibility) = self.visibility {
            let wire = visibility
                .wire()
                .ok_or_else(|| ImmichError::invalid_field("visibility", "unknown visibility"))?;
            body.insert("visibility".into(), Value::from(wire));
        }
        if let Some(description) = &self.description {
            body.insert("description".into(), Value::from(description.as_str()));
        }
        Ok(Value::Object(body))
    }
}

/// The logged-in user (`GET /users/me`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImmichUser {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    /// Missing means "not an admin": the safe reading.
    #[serde(default)]
    pub is_admin: bool,
}

/// `GET /server/about`: the version string as the server writes it (`v3.4.0`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ServerAbout {
    #[serde(default)]
    pub version: String,
}

impl ServerAbout {
    pub fn parsed_version(&self) -> Option<ImmichVersion> {
        ImmichVersion::parse(&self.version)
    }
}

/// The result of `POST /auth/login`: a short-lived bearer token, only used to
/// create the API key (and to clean up when that fails).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginSession {
    pub access_token: Redacted<String>,
    pub user_id: String,
    #[serde(default, rename = "name")]
    pub user_name: String,
    #[serde(default)]
    pub is_admin: bool,
}

/// A freshly created API key; the secret is only ever returned this once.
///
/// Accepts both response shapes of `POST /api-keys`: `{ secret, id }` and the older
/// `{ secret, apiKey: { id } }`.
#[derive(Clone, Debug, Deserialize)]
#[serde(try_from = "RawCreatedApiKey")]
pub struct CreatedApiKey {
    pub id: String,
    pub secret: Redacted<String>,
}

#[derive(Deserialize)]
struct RawCreatedApiKey {
    secret: Option<Redacted<String>>,
    id: Option<String>,
    #[serde(rename = "apiKey")]
    api_key: Option<RawApiKeyRef>,
}

#[derive(Deserialize)]
struct RawApiKeyRef {
    id: Option<String>,
}

impl TryFrom<RawCreatedApiKey> for CreatedApiKey {
    type Error = &'static str;

    fn try_from(raw: RawCreatedApiKey) -> Result<Self, Self::Error> {
        let secret = raw
            .secret
            .filter(|secret| !secret.expose().is_empty())
            .ok_or("the response has no API key secret")?;
        let id = raw
            .id
            .or_else(|| raw.api_key.and_then(|key| key.id))
            .filter(|id| !id.is_empty())
            .ok_or("the response has no API key id")?;
        Ok(Self { id, secret })
    }
}

/// An external library (`LibraryResponseDto`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImmichLibrary {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub import_paths: Vec<String>,
    #[serde(default)]
    pub exclusion_patterns: Vec<String>,
    #[serde(default)]
    pub asset_count: u64,
}

/// An album that holds an asset (`GET /albums?assetId=`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImmichAlbum {
    pub id: String,
    #[serde(default)]
    pub album_name: String,
}

/// The outcome for one asset of a bulk call (`PUT /albums/{id}/assets`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkIdResult {
    pub id: String,
    #[serde(default)]
    pub success: bool,
    /// Immich's reason code (`duplicate`, `no_permission`, `not_found`, `unknown`).
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

impl BulkIdResult {
    /// Done, or already in the desired state (`duplicate`: the asset is already in
    /// the album).
    pub fn is_ok(&self) -> bool {
        self.success || self.error.as_deref() == Some("duplicate")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn asset(value: Value) -> ImmichAsset {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn version_parsing_tolerates_prefixes_and_suffixes() {
        let table = [
            ("v3.4.0-rc.0", Some((3, 4, 0))),
            ("3.4.0", Some((3, 4, 0))),
            ("  v2.1  ", Some((2, 1, 0))),
            ("V2", Some((2, 0, 0))),
            ("v1.135.3+build.7", Some((1, 135, 3))),
            ("3.4.0.1", Some((3, 4, 0))),
            ("", None),
            ("v", None),
            ("latest", None),
            ("3.x.1", None),
            ("3..1", None),
        ];
        for (text, expected) in table {
            let parsed = ImmichVersion::parse(text).map(|v| (v.major, v.minor, v.patch));
            assert_eq!(parsed, expected, "parsing {text:?}");
        }
    }

    #[test]
    fn versions_compare_numerically_and_display_plainly() {
        let low = ImmichVersion::new(2, 9, 9);
        let high = ImmichVersion::new(3, 0, 0);
        assert!(low < high);
        assert!(
            ImmichVersion::new(1, 20, 0) < ImmichVersion::new(1, 135, 0),
            "the minor part compares as a number, not as text"
        );
        assert_eq!(ImmichVersion::new(3, 4, 0).to_string(), "3.4.0");
        assert_eq!(
            ImmichVersion::parse("v3.2.0-rc.1"),
            Some(SEARCH_FILTER_SINCE)
        );
    }

    #[test]
    fn the_filter_style_starts_at_3_2_0() {
        assert!(!ImmichVersion::new(2, 7, 5).supports_search_filter());
        assert!(!ImmichVersion::new(3, 1, 9).supports_search_filter());
        assert!(ImmichVersion::new(3, 2, 0).supports_search_filter());
        assert!(SPEC_VERSION.supports_search_filter());
        assert!(ImmichVersion::new(4, 0, 0).supports_search_filter());
    }

    #[test]
    fn the_version_endpoint_shape_deserializes() {
        let version: ImmichVersion =
            serde_json::from_value(json!({"major": 3, "minor": 4, "patch": 0})).unwrap();
        assert_eq!(version, SPEC_VERSION);
    }

    #[test]
    fn timestamps_round_trip_with_millisecond_precision() {
        let ms = parse_rfc3339_ms("2024-03-05T10:20:30.123Z").unwrap();
        assert_eq!(ms, 1_709_634_030_123);
        assert_eq!(format_rfc3339_ms(ms), "2024-03-05T10:20:30.123Z");
        assert_eq!(
            parse_rfc3339_ms("2024-03-05T11:20:30.123+01:00"),
            Some(ms),
            "offsets are honoured"
        );
        assert_eq!(parse_rfc3339_ms("yesterday"), None);
        assert_eq!(format_rfc3339_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_rfc3339_ms(i64::MAX), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_full_asset_parses_into_the_trimmed_view() {
        let parsed = asset(json!({
            "id": "1b3c3a8e-6f5b-4f0e-9d62-5a6b8a2c1f10",
            "checksum": "2jmj7l5rSw0yVb/vlWAYkK/YBwk=",
            "originalPath": "/mnt/photo/2024/IMG_0001.jpg",
            "originalFileName": "IMG_0001.jpg",
            "libraryId": "9f1b9b6e-3a49-4a3f-8f0a-0d9d4f6b2c11",
            "isOffline": false,
            "isTrashed": false,
            "updatedAt": "2024-03-05T10:20:30.123Z",
            "fileCreatedAt": "2024-03-04T08:00:00.000Z",
            "type": "IMAGE",
            "isFavorite": true,
            "visibility": "archive",
            "tags": [{"id": "t1", "name": "Trip", "value": "Holidays/Trip", "color": "#fff"}],
            "exifInfo": {"rating": 4, "make": "Canon"},
            "ownerId": "ignored",
            "somethingNew": {"deeply": ["nested"]}
        }));
        assert_eq!(parsed.id, "1b3c3a8e-6f5b-4f0e-9d62-5a6b8a2c1f10");
        assert_eq!(parsed.original_path, "/mnt/photo/2024/IMG_0001.jpg");
        assert_eq!(
            parsed.library_id.as_deref(),
            Some("9f1b9b6e-3a49-4a3f-8f0a-0d9d4f6b2c11")
        );
        assert_eq!(parsed.updated_at_ms, Some(1_709_634_030_123));
        assert_eq!(parsed.file_created_at_ms, Some(1_709_539_200_000));
        assert_eq!(parsed.asset_type, AssetType::Image);
        assert_eq!(parsed.visibility, AssetVisibility::Archive);
        assert!(parsed.is_favorite && parsed.is_live());
        assert_eq!(parsed.tags[0].value, "Holidays/Trip");
        assert_eq!(parsed.rating(), 4);
    }

    #[test]
    fn a_minimal_asset_gets_safe_defaults() {
        let parsed = asset(json!({"id": "a"}));
        assert_eq!(parsed.checksum, "");
        assert_eq!(parsed.library_id, None);
        assert!(!parsed.is_offline && !parsed.trashed());
        assert_eq!(parsed.visibility, AssetVisibility::Timeline);
        assert_eq!(parsed.asset_type, AssetType::Unknown);
        assert_eq!(parsed.updated_at_ms, None);
        assert_eq!(parsed.rating(), 0);
        assert!(parsed.tags.is_empty());
    }

    #[test]
    fn nulls_and_odd_values_never_fail_the_page() {
        let parsed = asset(json!({
            "id": "a",
            "libraryId": null,
            "originalPath": null,
            "isOffline": null,
            "updatedAt": null,
            "fileCreatedAt": "not a date",
            "deletedAt": 12,
            "type": null,
            "visibility": null,
            "tags": null,
            "exifInfo": {"rating": null}
        }));
        assert_eq!(parsed.library_id, None);
        assert_eq!(parsed.original_path, "");
        assert_eq!(parsed.updated_at_ms, None);
        assert_eq!(parsed.file_created_at_ms, None);
        assert_eq!(parsed.deleted_at_ms, None);
        assert_eq!(parsed.exif_info, Some(ExifInfo::default()));
    }

    #[test]
    fn the_import_state_and_the_description_are_read() {
        let imported = asset(json!({
            "id": "a",
            "hasMetadata": true,
            "exifInfo": {"rating": 3, "description": "Sunset at the pier"}
        }));
        assert!(imported.has_metadata);
        assert_eq!(imported.description(), Some("Sunset at the pier"));
        assert_eq!(imported.rating(), 3);

        let importing = asset(json!({"id": "a", "hasMetadata": false}));
        assert!(!importing.has_metadata);
        assert_eq!(importing.description(), None);

        assert!(
            asset(json!({"id": "a"})).has_metadata,
            "a server that does not report the flag has nothing to wait for"
        );
        assert!(!ImmichAsset::default().has_metadata);
        for odd in [json!(""), json!("  \n"), json!(null), json!(12)] {
            let parsed = asset(json!({"id": "a", "exifInfo": {"description": odd}}));
            assert_eq!(parsed.description(), None);
        }
    }

    #[test]
    fn unknown_enum_values_map_to_unknown() {
        let parsed = asset(json!({"id": "a", "type": "HOLOGRAM", "visibility": "vault"}));
        assert_eq!(parsed.asset_type, AssetType::Unknown);
        assert_eq!(parsed.visibility, AssetVisibility::Unknown);
        assert_eq!(AssetVisibility::Unknown.wire(), None);
        assert_eq!(AssetVisibility::Hidden.wire(), Some("hidden"));
    }

    #[test]
    fn an_asset_without_an_id_is_rejected() {
        assert!(serde_json::from_value::<ImmichAsset>(json!({"originalPath": "/x"})).is_err());
    }

    #[test]
    fn trashed_covers_both_reporting_styles() {
        assert!(asset(json!({"id": "a", "isTrashed": true})).trashed());
        assert!(asset(json!({"id": "a", "deletedAt": "2024-01-01T00:00:00.000Z"})).trashed());
        assert!(!asset(json!({"id": "a", "deletedAt": null})).trashed());
        assert!(!asset(json!({"id": "a", "isOffline": true})).is_live());
    }

    #[test]
    fn ratings_outside_one_to_five_read_as_unrated() {
        let rating =
            |value: Value| asset(json!({"id": "a", "exifInfo": {"rating": value}})).rating();
        assert_eq!(rating(json!(5)), 5);
        assert_eq!(rating(json!(1)), 1);
        assert_eq!(rating(json!(0)), 0);
        assert_eq!(rating(json!(-1)), 0, "rejected");
        assert_eq!(rating(json!(9)), 0);
        assert_eq!(rating(json!("4")), 0);
    }

    #[test]
    fn only_an_offline_asset_becomes_an_offline_asset_id() {
        let online = asset(json!({"id": "a", "isOffline": false}));
        assert_eq!(OfflineAssetId::confirm(&online), None);

        let offline = asset(json!({"id": "b", "isOffline": true}));
        let confirmed = OfflineAssetId::confirm(&offline).unwrap();
        assert_eq!(confirmed.as_str(), "b");

        let blank = ImmichAsset {
            is_offline: true,
            ..ImmichAsset::default()
        };
        assert_eq!(OfflineAssetId::confirm(&blank), None, "no id, no deletion");
    }

    #[test]
    fn page_tokens_serialize_and_resume() {
        for token in [PageToken::Page(3), PageToken::Cursor("abc".into())] {
            let text = serde_json::to_string(&token).unwrap();
            assert_eq!(serde_json::from_str::<PageToken>(&text).unwrap(), token);
        }
        assert_eq!(
            serde_json::to_value(PageToken::Page(2)).unwrap(),
            json!({"page": 2})
        );
    }

    #[test]
    fn the_search_size_is_clamped() {
        let size = |size| {
            MetadataSearch {
                size,
                ..MetadataSearch::default()
            }
            .effective_size()
        };
        assert_eq!(size(0), 1);
        assert_eq!(size(250), 250);
        assert_eq!(size(5000), MAX_PAGE_SIZE);
        assert_eq!(MetadataSearch::default().size, DEFAULT_PAGE_SIZE);
        assert_eq!(
            MetadataSearch::in_library("lib").library_id.as_deref(),
            Some("lib")
        );
    }

    #[test]
    fn an_asset_update_builds_a_validated_body() {
        let ids = vec!["a".to_owned(), "b".to_owned()];
        let update = AssetUpdate {
            is_favorite: Some(true),
            rating: Some(4),
            visibility: Some(AssetVisibility::Archive),
            description: Some("Trip".into()),
        };
        assert_eq!(
            update.body(&ids).unwrap(),
            json!({
                "ids": ["a", "b"],
                "isFavorite": true,
                "rating": 4,
                "visibility": "archive",
                "description": "Trip"
            })
        );

        let clear = AssetUpdate {
            rating: Some(0),
            ..AssetUpdate::default()
        };
        assert_eq!(
            clear.body(&ids).unwrap(),
            json!({"ids": ["a", "b"], "rating": null}),
            "0 stars clears the rating"
        );
        assert!(AssetUpdate::default().is_empty());
        assert!(!clear.is_empty());
    }

    #[test]
    fn an_invalid_asset_update_is_refused_before_any_request() {
        let ids = vec!["a".to_owned()];
        let rating = AssetUpdate {
            rating: Some(6),
            ..AssetUpdate::default()
        };
        assert!(matches!(rating.body(&ids), Err(ImmichError::Validation(_))));
        let visibility = AssetUpdate {
            visibility: Some(AssetVisibility::Unknown),
            ..AssetUpdate::default()
        };
        assert!(matches!(
            visibility.body(&ids),
            Err(ImmichError::Validation(_))
        ));
    }

    #[test]
    fn login_and_key_responses_parse_and_hide_secrets() {
        let session: LoginSession = serde_json::from_value(json!({
            "accessToken": "token-123",
            "userId": "u1",
            "userEmail": "me@example.test",
            "name": "Me",
            "isAdmin": true,
            "shouldChangePassword": false
        }))
        .unwrap();
        assert_eq!(session.access_token.expose(), "token-123");
        assert!(session.is_admin);
        assert!(!format!("{session:?}").contains("token-123"));

        let flat: CreatedApiKey =
            serde_json::from_value(json!({"secret": "s3cret", "id": "k1", "name": "x"})).unwrap();
        assert_eq!(
            (flat.id.as_str(), flat.secret.expose().as_str()),
            ("k1", "s3cret")
        );
        let nested: CreatedApiKey =
            serde_json::from_value(json!({"secret": "s3cret", "apiKey": {"id": "k2"}})).unwrap();
        assert_eq!(nested.id, "k2");
        assert!(!format!("{nested:?}").contains("s3cret"));

        assert!(serde_json::from_value::<CreatedApiKey>(json!({"id": "k"})).is_err());
        assert!(serde_json::from_value::<CreatedApiKey>(json!({"secret": "s"})).is_err());
        assert!(serde_json::from_value::<CreatedApiKey>(json!({"secret": "", "id": "k"})).is_err());
    }

    #[test]
    fn small_response_models_are_tolerant() {
        let user: ImmichUser =
            serde_json::from_value(json!({"id": "u", "name": "Me", "email": "e", "extra": 1}))
                .unwrap();
        assert!(!user.is_admin, "a missing flag is not admin");

        let about: ServerAbout =
            serde_json::from_value(json!({"version": "v3.4.0-rc.0", "licensed": false})).unwrap();
        assert_eq!(about.parsed_version(), Some(SPEC_VERSION));

        let library: ImmichLibrary = serde_json::from_value(json!({
            "id": "l", "name": "Photos", "importPaths": ["/mnt/photo"],
            "exclusionPatterns": ["**/@eaDir/**"], "assetCount": 12, "ownerId": "u"
        }))
        .unwrap();
        assert_eq!(library.import_paths, ["/mnt/photo"]);
        assert_eq!(library.asset_count, 12);

        let album: ImmichAlbum =
            serde_json::from_value(json!({"id": "a", "albumName": "Trip", "shared": false}))
                .unwrap();
        assert_eq!(album.album_name, "Trip");
    }

    #[test]
    fn bulk_results_treat_a_duplicate_as_done() {
        let result = |value: Value| serde_json::from_value::<BulkIdResult>(value).unwrap();
        assert!(result(json!({"id": "a", "success": true})).is_ok());
        assert!(result(json!({"id": "a", "success": false, "error": "duplicate"})).is_ok());
        assert!(!result(json!({"id": "a", "success": false, "error": "no_permission"})).is_ok());
        assert!(!result(json!({"id": "a"})).is_ok());
    }
}
