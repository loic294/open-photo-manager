//! Typed application settings service backed by the database.
//!
//! Owned by todo `designing-database-schema`. Exposes strongly typed accessors
//! (and the `GET`/`PUT /api/settings` DTO in `apm_core::dto::settings`); other
//! modules read settings through this service, never straight from the table.

/// Settings service. Empty until `designing-database-schema` implements it.
#[derive(Clone, Debug, Default)]
pub struct SettingsService;

impl SettingsService {
    pub fn new() -> Self {
        Self
    }
}
