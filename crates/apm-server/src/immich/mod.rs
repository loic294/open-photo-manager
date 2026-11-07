//! Immich integration: API client, credential storage, asset sync and the
//! outbox that mirrors local file operations to Immich.
//!
//! `client`, `crypto` and `models` are owned by todo `building-immich-client`,
//! `sync` by `syncing-immich-assets`, `outbox` by
//! `reflecting-file-ops-in-immich`.

pub mod client;
pub mod crypto;
pub mod models;
pub mod outbox;
pub mod sync;

/// Immich service. Empty until `building-immich-client` implements it.
#[derive(Clone, Debug, Default)]
pub struct ImmichService;

impl ImmichService {
    pub fn new() -> Self {
        Self
    }
}
