//! Request and response helpers shared by the `browse_*` tests. Free functions on purpose: other
//! test helpers add methods to `TestApp`, and two modules must not define the same one.

use axum::http::{Method, StatusCode};
use opm_core::paths::FileId;
use serde_json::Value;

use super::TestApp;

/// `POST /api/browse/folders`.
pub async fn folders(app: &TestApp, body: Value) -> (StatusCode, Value) {
    app.call(Method::POST, "/api/browse/folders", Some(body))
        .await
}

/// `POST /api/browse/items`.
pub async fn items(app: &TestApp, body: Value) -> (StatusCode, Value) {
    app.call(Method::POST, "/api/browse/items", Some(body))
        .await
}

/// `POST /api/browse/ids`.
pub async fn ids(app: &TestApp, body: Value) -> (StatusCode, Value) {
    app.call(Method::POST, "/api/browse/ids", Some(body)).await
}

/// `POST /api/facets`.
pub async fn facets(app: &TestApp, body: Value) -> (StatusCode, Value) {
    app.call(Method::POST, "/api/facets", Some(body)).await
}

/// `GET path`, parsed as JSON.
pub async fn get(app: &TestApp, path: &str) -> (StatusCode, Value) {
    app.call(Method::GET, path, None).await
}

/// The `name` of every entry of `response[key]` (`"items"` or `"folders"`), in order.
pub fn names(response: &Value, key: &str) -> Vec<String> {
    response[key]
        .as_array()
        .unwrap_or_else(|| panic!("no list at {key}: {response}"))
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The `id` of every item of an items response, in order.
pub fn item_ids(response: &Value) -> Vec<FileId> {
    response["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items: {response}"))
        .iter()
        .map(|item| item["id"].as_i64().unwrap())
        .collect()
}

/// The ids of an ids response, in order.
pub fn id_list(response: &Value) -> Vec<FileId> {
    response["ids"]
        .as_array()
        .unwrap_or_else(|| panic!("no ids: {response}"))
        .iter()
        .map(|id| id.as_i64().unwrap())
        .collect()
}

/// `(count, total)` of every folder of a folders response, keyed in order with [`names`].
pub fn counts(response: &Value) -> Vec<(i64, i64)> {
    response["folders"]
        .as_array()
        .unwrap_or_else(|| panic!("no folders: {response}"))
        .iter()
        .map(|folder| {
            (
                folder["count"].as_i64().unwrap(),
                folder["total"].as_i64().unwrap(),
            )
        })
        .collect()
}

/// `(value, count)` of every entry of a facet list.
pub fn facet(response: &Value, key: &str) -> Vec<(String, i64)> {
    response[key]
        .as_array()
        .unwrap_or_else(|| panic!("no facet {key}: {response}"))
        .iter()
        .map(|entry| {
            (
                entry["value"].as_str().unwrap().to_owned(),
                entry["count"].as_i64().unwrap(),
            )
        })
        .collect()
}

/// The error code of an error response.
pub fn error_code(response: &Value) -> &str {
    response["error"]["code"].as_str().unwrap_or_else(|| {
        panic!("not an error body: {response}");
    })
}
