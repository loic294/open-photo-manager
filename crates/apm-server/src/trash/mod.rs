//! User-facing trash management: listing trashed items, restore, and emptying
//! (as a task). The physical move into the trash area is `fsops::trash`.
//!
//! Owned by todo `building-trash-management` (with `api/trash.rs`).

/// Trash service. Empty until `building-trash-management` implements it.
#[derive(Clone, Debug, Default)]
pub struct TrashService;

impl TrashService {
    pub fn new() -> Self {
        Self
    }
}
