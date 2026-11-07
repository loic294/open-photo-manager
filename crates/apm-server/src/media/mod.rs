//! Media serving: thumbnails, previews and original-file streaming.
//!
//! Owned by todo `serving-media` (the `GET /api/files/{id}/thumb|preview|content`
//! handlers live in `api/media.rs`).

pub mod preview;
pub mod serve;
pub mod thumbs;

/// Media service. Empty until `serving-media` implements it.
#[derive(Clone, Debug, Default)]
pub struct MediaService;

impl MediaService {
    pub fn new() -> Self {
        Self
    }
}
