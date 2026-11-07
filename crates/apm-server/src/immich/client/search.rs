//! The two request bodies of `POST /search/metadata` and the reading of its answer.
//!
//! Immich 3.2 replaced the flat search fields (`libraryId`, `updatedAfter`, `page`, ...) by a typed
//! `filter`, an `orderBy` and a `cursor`, and deprecated the flat form. A [`MetadataSearch`]
//! describes a search once; [`body`] turns it into either generation and [`read_page`] reads either
//! answer into one [`SearchPage`].
//!
//! The generations also differ in what they return when a field is left out: the typed filter has
//! no implicit trash or visibility filter, the flat form hides trashed assets (unless `withDeleted`,
//! which `isOffline: true` implies) and everything but archived and timeline assets, and its name
//! and path fields are substring matches. The documented semantics of [`MetadataSearch`] are
//! therefore enforced on the answer as well ([`keep`]), so a caller sees the same from every server.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::immich::error::{ImmichError, MAX_UPSTREAM_MESSAGE_CHARS, truncate_chars};
use crate::immich::models::{
    AssetVisibility, ImmichAsset, MetadataSearch, OfflineFilter, PageToken, SearchPage,
    format_rfc3339_ms,
};
use crate::security::redact_urls_in;

/// Which request generation a call uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Style {
    /// Immich 3.2 and later: `filter`, `orderBy`, `cursor`.
    Filter,
    /// Before 3.2: flat fields and `page`.
    Flat,
}

/// What reading an answer produced.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reading {
    Page(SearchPage),
    /// A typed-filter request that the server answered in the old paging style: it does not know
    /// the filter form and ignored it.
    OldServer,
}

pub(super) fn body(style: Style, search: &MetadataSearch) -> Value {
    match style {
        Style::Filter => filter_body(search),
        Style::Flat => flat_body(search),
    }
}

fn offline_flag(filter: OfflineFilter) -> Option<bool> {
    match filter {
        OfflineFilter::Any => None,
        OfflineFilter::OnlyOffline => Some(true),
        OfflineFilter::OnlyOnline => Some(false),
    }
}

/// The typed body. Trashed assets are excluded explicitly (`trashedAt` equals null) because this
/// form has no implicit trash filter.
fn filter_body(search: &MetadataSearch) -> Value {
    let mut filter = Map::new();
    if let Some(library) = &search.library_id {
        filter.insert("libraryId".into(), json!({ "eq": library }));
    }
    if !search.include_trashed {
        filter.insert("trashedAt".into(), json!({ "eq": null }));
    }
    if let Some(offline) = offline_flag(search.offline) {
        filter.insert("isOffline".into(), json!({ "eq": offline }));
    }
    if let Some(ms) = search.updated_after_ms {
        filter.insert("updatedAt".into(), json!({ "gt": format_rfc3339_ms(ms) }));
    }
    if let Some(visibility) = search.visibility.and_then(AssetVisibility::wire) {
        filter.insert("visibility".into(), json!({ "eq": visibility }));
    }
    if let Some(name) = &search.original_file_name {
        filter.insert("originalFileName".into(), json!({ "eq": name }));
    }
    if let Some(prefix) = &search.original_path_prefix {
        filter.insert("originalPath".into(), json!({ "startsWith": prefix }));
    }

    let mut body = Map::new();
    body.insert("size".into(), json!(search.effective_size()));
    body.insert(
        "orderBy".into(),
        json!({ "field": "fileCreatedAt", "direction": search.order.wire() }),
    );
    body.insert("filter".into(), Value::Object(filter));
    if search.with_exif {
        body.insert("withExif".into(), Value::Bool(true));
    }
    if let Some(PageToken::Cursor(cursor)) = &search.after {
        body.insert("cursor".into(), json!(cursor));
    }
    Value::Object(body)
}

/// The flat body of servers older than 3.2.
fn flat_body(search: &MetadataSearch) -> Value {
    let page = match &search.after {
        Some(PageToken::Page(page)) => (*page).max(1),
        _ => 1,
    };
    let mut body = Map::new();
    body.insert("size".into(), json!(search.effective_size()));
    body.insert("page".into(), json!(page));
    body.insert("order".into(), json!(search.order.wire()));
    if let Some(library) = &search.library_id {
        body.insert("libraryId".into(), json!(library));
    }
    if let Some(ms) = search.updated_after_ms {
        body.insert("updatedAfter".into(), json!(format_rfc3339_ms(ms)));
    }
    if search.include_trashed {
        body.insert("withDeleted".into(), Value::Bool(true));
    }
    if let Some(offline) = offline_flag(search.offline) {
        body.insert("isOffline".into(), Value::Bool(offline));
    }
    if let Some(visibility) = search.visibility.and_then(AssetVisibility::wire) {
        body.insert("visibility".into(), json!(visibility));
    }
    if let Some(name) = &search.original_file_name {
        body.insert("originalFileName".into(), json!(name));
    }
    if let Some(prefix) = &search.original_path_prefix {
        body.insert("originalPath".into(), json!(prefix));
    }
    if search.with_exif {
        body.insert("withExif".into(), Value::Bool(true));
    }
    Value::Object(body)
}

#[derive(Deserialize)]
struct Answer {
    assets: AssetsAnswer,
}

#[derive(Deserialize)]
struct AssetsAnswer {
    #[serde(default)]
    items: Vec<ImmichAsset>,
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
    /// A string on the wire; tolerated as a number as well.
    #[serde(default, rename = "nextPage")]
    next_page: Option<Value>,
}

fn protocol(detail: &str) -> ImmichError {
    ImmichError::Protocol(detail.to_owned())
}

/// Reads one answer of `style` for `search`.
///
/// With `detect_old_server` a typed-filter answer that shows the server ignored the filter form is
/// reported as [`Reading::OldServer`] instead of as a result: either it pages in the old style
/// (`nextPage` without `nextCursor`), which would otherwise look like a complete last page, or it
/// holds assets of a library other than the one asked for.
pub(super) fn read_page(
    style: Style,
    search: &MetadataSearch,
    body: &[u8],
    detect_old_server: bool,
) -> Result<Reading, ImmichError> {
    let answer: Answer = serde_json::from_slice(body).map_err(|err| {
        ImmichError::Protocol(format!(
            "unexpected response from POST /search/metadata: {}",
            truncate_chars(
                &redact_urls_in(&err.to_string()),
                MAX_UPSTREAM_MESSAGE_CHARS
            )
        ))
    })?;
    let AssetsAnswer {
        items,
        next_cursor,
        next_page,
    } = answer.assets;

    let next_page = parse_next_page(next_page)?;
    let next_cursor = next_cursor.filter(|cursor| !cursor.is_empty());
    if detect_old_server && style == Style::Filter && next_cursor.is_none() && next_page.is_some() {
        return Ok(Reading::OldServer);
    }

    if let Some(library) = &search.library_id {
        let foreign = items.iter().any(|asset| {
            asset
                .library_id
                .as_deref()
                .is_some_and(|other| !other.eq_ignore_ascii_case(library))
        });
        if foreign {
            // Only a server that drops fields it does not know answers a library filter with
            // assets of other libraries; while probing that is the same sign as the paging style.
            if detect_old_server && style == Style::Filter {
                return Ok(Reading::OldServer);
            }
            return Err(protocol(
                "Immich ignored the library filter of the search; refusing to mix in assets of other libraries",
            ));
        }
    }

    let next = match style {
        Style::Filter => next_cursor.map(PageToken::Cursor),
        Style::Flat => next_page.map(PageToken::Page),
    };
    if next.is_some() && next == search.after {
        return Err(protocol(
            "Immich answered with the page token it was asked to continue from",
        ));
    }

    let items = items
        .into_iter()
        .filter(|asset| keep(search, asset))
        .collect();
    Ok(Reading::Page(SearchPage { items, next }))
}

fn parse_next_page(value: Option<Value>) -> Result<Option<u32>, ImmichError> {
    let unusable = || protocol("Immich sent a page number that is not a number");
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(None);
            }
            text.parse().map(Some).map_err(|_| unusable())
        }
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|page| u32::try_from(page).ok())
            .map(Some)
            .ok_or_else(unusable),
        Some(_) => Err(unusable()),
    }
}

/// The documented semantics of [`MetadataSearch`], applied to one returned asset (see the module
/// docs for why the server's own filtering is not trusted).
fn keep(search: &MetadataSearch, asset: &ImmichAsset) -> bool {
    (search.include_trashed || !asset.trashed())
        && match search.offline {
            OfflineFilter::Any => true,
            OfflineFilter::OnlyOffline => asset.is_offline,
            OfflineFilter::OnlyOnline => !asset.is_offline,
        }
        && search
            .visibility
            .is_none_or(|wanted| wanted == AssetVisibility::Unknown || asset.visibility == wanted)
        && search
            .updated_after_ms
            .is_none_or(|after| asset.updated_at_ms.is_none_or(|updated| updated > after))
        && search
            .original_file_name
            .as_deref()
            .is_none_or(|name| asset.original_file_name == name)
        && search
            .original_path_prefix
            .as_deref()
            .is_none_or(|prefix| asset.original_path.starts_with(prefix))
}

/// Whether the message of a 400 answer looks like the server refusing the typed filter form, which
/// is the case for a server older than 3.2 that validates strictly.
pub(super) fn rejects_the_filter_form(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "should not exist",
        "filter",
        "orderby",
        "cursor",
        "trashedat",
        "updatedat",
        "isoffline",
        "libraryid",
    ]
    .iter()
    .any(|word| message.contains(word))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const LIBRARY: &str = "6f1c6a1e-0d6f-4a53-9a4c-3d9c7c1a2b11";
    const OTHER_LIBRARY: &str = "0b5a8f0e-72a1-4e2b-8d57-9c7a4b1d6e22";

    fn page(style: Style, search: &MetadataSearch, answer: Value) -> SearchPage {
        let bytes = serde_json::to_vec(&answer).unwrap();
        match read_page(style, search, &bytes, false).unwrap() {
            Reading::Page(page) => page,
            Reading::OldServer => panic!("unexpected old-server reading"),
        }
    }

    fn item(id: &str, path: &str) -> Value {
        json!({
            "id": id,
            "originalPath": path,
            "originalFileName": path.rsplit('/').next().unwrap(),
            "libraryId": LIBRARY,
            "isOffline": false,
            "isTrashed": false,
            "updatedAt": "2024-05-01T10:00:00.000Z",
        })
    }

    #[test]
    fn the_minimal_filter_body_excludes_trashed_assets_explicitly() {
        let body = body(Style::Filter, &MetadataSearch::default());
        assert_eq!(
            body,
            json!({
                "size": 250,
                "orderBy": { "field": "fileCreatedAt", "direction": "asc" },
                "filter": { "trashedAt": { "eq": null } },
            })
        );
    }

    #[test]
    fn the_full_filter_body_carries_every_field() {
        let search = MetadataSearch {
            library_id: Some(LIBRARY.into()),
            updated_after_ms: Some(1_714_557_600_123),
            offline: OfflineFilter::OnlyOnline,
            include_trashed: false,
            visibility: Some(AssetVisibility::Archive),
            original_file_name: Some("IMG_0001.jpg".into()),
            original_path_prefix: Some("/mnt/photo/2024/".into()),
            order: crate::immich::models::SortOrder::Desc,
            with_exif: true,
            size: 5000,
            after: Some(PageToken::Cursor("opaque-cursor".into())),
        };
        assert_eq!(
            body(Style::Filter, &search),
            json!({
                "size": 1000,
                "orderBy": { "field": "fileCreatedAt", "direction": "desc" },
                "withExif": true,
                "cursor": "opaque-cursor",
                "filter": {
                    "libraryId": { "eq": LIBRARY },
                    "trashedAt": { "eq": null },
                    "isOffline": { "eq": false },
                    "updatedAt": { "gt": "2024-05-01T10:00:00.123Z" },
                    "visibility": { "eq": "archive" },
                    "originalFileName": { "eq": "IMG_0001.jpg" },
                    "originalPath": { "startsWith": "/mnt/photo/2024/" },
                },
            })
        );
    }

    #[test]
    fn including_trashed_assets_drops_the_trash_filter() {
        let search = MetadataSearch {
            include_trashed: true,
            offline: OfflineFilter::OnlyOffline,
            ..MetadataSearch::in_library(LIBRARY)
        };
        let body = body(Style::Filter, &search);
        assert_eq!(
            body["filter"],
            json!({ "libraryId": { "eq": LIBRARY }, "isOffline": { "eq": true } })
        );
    }

    #[test]
    fn the_flat_body_uses_the_old_field_names() {
        let minimal = body(Style::Flat, &MetadataSearch::default());
        assert_eq!(minimal, json!({ "size": 250, "page": 1, "order": "asc" }));

        let search = MetadataSearch {
            library_id: Some(LIBRARY.into()),
            updated_after_ms: Some(1_714_557_600_000),
            offline: OfflineFilter::OnlyOffline,
            include_trashed: true,
            visibility: Some(AssetVisibility::Hidden),
            original_file_name: Some("a.jpg".into()),
            original_path_prefix: Some("/mnt/photo/".into()),
            order: crate::immich::models::SortOrder::Desc,
            with_exif: true,
            size: 0,
            after: Some(PageToken::Page(3)),
        };
        assert_eq!(
            body(Style::Flat, &search),
            json!({
                "size": 1,
                "page": 3,
                "order": "desc",
                "libraryId": LIBRARY,
                "updatedAfter": "2024-05-01T10:00:00.000Z",
                "withDeleted": true,
                "isOffline": true,
                "visibility": "hidden",
                "originalFileName": "a.jpg",
                "originalPath": "/mnt/photo/",
                "withExif": true,
            })
        );
    }

    #[test]
    fn a_filter_answer_continues_with_its_cursor() {
        let search = MetadataSearch::in_library(LIBRARY);
        let answer = json!({ "assets": {
            "items": [item("a1", "/mnt/photo/a.jpg")],
            "count": 1, "total": 1, "nextCursor": "c2", "nextPage": null,
        }});
        let first = page(Style::Filter, &search, answer);
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.next, Some(PageToken::Cursor("c2".into())));

        let last = json!({ "assets": { "items": [], "nextCursor": null, "nextPage": null }});
        assert_eq!(page(Style::Filter, &search, last).next, None);
    }

    #[test]
    fn a_flat_answer_continues_with_its_page_number() {
        let search = MetadataSearch::in_library(LIBRARY);
        let string_page = json!({ "assets": { "items": [], "nextPage": "2" }});
        assert_eq!(
            page(Style::Flat, &search, string_page).next,
            Some(PageToken::Page(2))
        );
        let number_page = json!({ "assets": { "items": [], "nextPage": 7 }});
        assert_eq!(
            page(Style::Flat, &search, number_page).next,
            Some(PageToken::Page(7))
        );
        let last = json!({ "assets": { "items": [], "nextPage": null }});
        assert_eq!(page(Style::Flat, &search, last).next, None);
        let empty = json!({ "assets": { "items": [], "nextPage": "" }});
        assert_eq!(page(Style::Flat, &search, empty).next, None);
    }

    #[test]
    fn an_unreadable_page_number_is_a_protocol_error() {
        let search = MetadataSearch::default();
        for bad in [json!("two"), json!(-1), json!([1]), json!(1.5)] {
            let bytes =
                serde_json::to_vec(&json!({ "assets": { "items": [], "nextPage": bad } })).unwrap();
            let err = read_page(Style::Flat, &search, &bytes, false).unwrap_err();
            assert!(matches!(err, ImmichError::Protocol(_)), "{bad}: {err:?}");
        }
    }

    #[test]
    fn a_repeated_page_token_is_refused_so_a_scan_cannot_loop_forever() {
        let search = MetadataSearch {
            after: Some(PageToken::Cursor("same".into())),
            ..MetadataSearch::default()
        };
        let bytes = serde_json::to_vec(&json!({ "assets": { "items": [], "nextCursor": "same" } }))
            .unwrap();
        let err = read_page(Style::Filter, &search, &bytes, false).unwrap_err();
        assert!(matches!(err, ImmichError::Protocol(_)), "{err:?}");
    }

    #[test]
    fn a_server_that_ignores_the_library_filter_is_refused() {
        let search = MetadataSearch::in_library(LIBRARY);
        let mut foreign = item("b1", "/other/b.jpg");
        foreign["libraryId"] = json!(OTHER_LIBRARY);
        let bytes = serde_json::to_vec(&json!({ "assets": {
            "items": [item("a1", "/mnt/photo/a.jpg"), foreign],
        }}))
        .unwrap();
        let err = read_page(Style::Filter, &search, &bytes, false).unwrap_err();
        assert!(matches!(err, ImmichError::Protocol(_)), "{err:?}");
        let flat_err = read_page(Style::Flat, &search, &bytes, true).unwrap_err();
        assert!(matches!(flat_err, ImmichError::Protocol(_)), "{flat_err:?}");

        // While probing an unknown server this is the sign of a server that predates the filter.
        assert_eq!(
            read_page(Style::Filter, &search, &bytes, true).unwrap(),
            Reading::OldServer
        );

        // Without a library filter every library is fine, and an asset that does not report its
        // library is not evidence of anything.
        let any = MetadataSearch::default();
        assert!(read_page(Style::Filter, &any, &bytes, false).is_ok());
        let mut anonymous = item("c1", "/mnt/photo/c.jpg");
        anonymous["libraryId"] = Value::Null;
        let bytes = serde_json::to_vec(&json!({ "assets": { "items": [anonymous] } })).unwrap();
        assert!(read_page(Style::Filter, &search, &bytes, false).is_ok());
    }

    #[test]
    fn the_answer_is_held_to_the_documented_semantics() {
        let mut trashed = item("t1", "/mnt/photo/t.jpg");
        trashed["deletedAt"] = json!("2024-05-02T00:00:00.000Z");
        let mut offline = item("o1", "/mnt/photo/o.jpg");
        offline["isOffline"] = json!(true);
        let mut hidden = item("h1", "/mnt/photo/h.jpg");
        hidden["visibility"] = json!("hidden");
        let mut old = item("old", "/mnt/photo/old.jpg");
        old["updatedAt"] = json!("2020-01-01T00:00:00.000Z");
        let substring = item("s1", "/data/mnt/photo/s.jpg");
        let answer = json!({ "assets": { "items": [
            item("a1", "/mnt/photo/a.jpg"), trashed, offline, hidden, old, substring,
        ]}});
        let ids = |search: &MetadataSearch| -> Vec<String> {
            page(Style::Filter, search, answer.clone())
                .items
                .into_iter()
                .map(|asset| asset.id)
                .collect()
        };

        assert_eq!(
            ids(&MetadataSearch::default()),
            ["a1", "o1", "h1", "old", "s1"],
            "trashed assets are dropped unless asked for"
        );
        let with_trashed = MetadataSearch {
            include_trashed: true,
            ..MetadataSearch::default()
        };
        assert_eq!(ids(&with_trashed).len(), 6);
        let only_offline = MetadataSearch {
            offline: OfflineFilter::OnlyOffline,
            ..MetadataSearch::default()
        };
        assert_eq!(ids(&only_offline), ["o1"]);
        let only_online = MetadataSearch {
            offline: OfflineFilter::OnlyOnline,
            ..MetadataSearch::default()
        };
        assert_eq!(ids(&only_online), ["a1", "h1", "old", "s1"]);
        let hidden_only = MetadataSearch {
            visibility: Some(AssetVisibility::Hidden),
            ..MetadataSearch::default()
        };
        assert_eq!(ids(&hidden_only), ["h1"]);
        let recent = MetadataSearch {
            updated_after_ms: Some(1_700_000_000_000),
            ..MetadataSearch::default()
        };
        assert_eq!(ids(&recent), ["a1", "o1", "h1", "s1"]);
        let by_name = MetadataSearch {
            original_file_name: Some("a.jpg".into()),
            ..MetadataSearch::default()
        };
        assert_eq!(ids(&by_name), ["a1"]);
        let by_prefix = MetadataSearch {
            original_path_prefix: Some("/mnt/photo/".into()),
            ..MetadataSearch::default()
        };
        assert_eq!(
            ids(&by_prefix),
            ["a1", "o1", "h1", "old"],
            "a substring match elsewhere in the path is not a prefix match"
        );
    }

    #[test]
    fn an_old_server_is_recognised_by_its_paging_style() {
        let search = MetadataSearch::default();
        let old = serde_json::to_vec(&json!({ "assets": {
            "items": [item("a1", "/mnt/photo/a.jpg")], "nextPage": "2",
        }}))
        .unwrap();
        assert_eq!(
            read_page(Style::Filter, &search, &old, true).unwrap(),
            Reading::OldServer
        );
        // Known versions are trusted: nothing is second-guessed.
        assert!(matches!(
            read_page(Style::Filter, &search, &old, false).unwrap(),
            Reading::Page(_)
        ));
        // A last page of a new server has neither token.
        let last =
            serde_json::to_vec(&json!({ "assets": { "items": [], "nextCursor": null }})).unwrap();
        assert!(matches!(
            read_page(Style::Filter, &search, &last, true).unwrap(),
            Reading::Page(_)
        ));
    }

    #[test]
    fn garbage_is_a_protocol_error_without_secrets() {
        let err = read_page(
            Style::Filter,
            &MetadataSearch::default(),
            b"<html>login</html>",
            false,
        )
        .unwrap_err();
        assert!(matches!(err, ImmichError::Protocol(_)), "{err:?}");
    }

    #[test]
    fn a_400_about_unknown_fields_triggers_the_fallback() {
        for message in [
            "property filter should not exist",
            "property orderBy should not exist; property cursor should not exist",
            "filter must be an object",
            "libraryId must be a UUID",
        ] {
            assert!(rejects_the_filter_form(message), "{message}");
        }
        for message in [
            "Internal server error",
            "size must not be greater than 1000",
        ] {
            assert!(!rejects_the_filter_form(message), "{message}");
        }
    }
}
