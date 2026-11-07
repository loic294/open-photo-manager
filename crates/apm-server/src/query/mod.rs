//! The file query engine: one implementation that turns a `Scope`, a `Filter`
//! and a sort order into SQL (via `sqlx::QueryBuilder`) for folder listings,
//! item pages, id lists, facets and plan overlays.
//!
//! Owned by todo `building-filter-query-engine`. Every other module that needs
//! to select files (plans, selections, immich sync, ...) reuses it instead of
//! writing its own SQL for filters.
