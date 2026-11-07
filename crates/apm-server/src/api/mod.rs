//! HTTP API. One file per resource, each exposing `routes()`; paths inside are
//! relative to `/api` because `app::router` nests this router there.
//!
//! Keep this file line-oriented: one `mod` and one `merge` per resource,
//! alphabetical. Handlers stay thin and delegate to the service that owns the
//! concern. Use the extractors in [`extract`] (never the raw axum ones) so
//! malformed requests produce the JSON `ErrorBody`.

use axum::Router;

use crate::state::AppState;

pub mod browse;
pub mod events;
pub mod extract;
mod files;
mod format;
mod health;
mod immich;
mod index;
mod media;
mod plans;
mod selections;
mod settings;
mod tasks;
mod trash;
mod volumes;

/// The `/api` router (without the `/api` prefix).
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(browse::routes())
        .merge(events::routes())
        .merge(files::routes())
        .merge(format::routes())
        .merge(health::routes())
        .merge(immich::routes())
        .merge(index::routes())
        .merge(media::routes())
        .merge(plans::routes())
        .merge(selections::routes())
        .merge(settings::routes())
        .merge(tasks::routes())
        .merge(trash::routes())
        .merge(volumes::routes())
}
