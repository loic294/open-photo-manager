//! SQLite access: connection pool and pragmas, migrations, shared row helpers.
//!
//! Owned by todo `designing-database-schema`. Migrations live in
//! `crates/apm-server/migrations/`: `0001_core.sql` belongs to the schema todo,
//! `0100+` to the Immich todos, `0200+` to plans/move/cleanup. `build.rs` makes
//! cargo notice new migration files.
//!
//! Only runtime `sqlx::query`, `sqlx::query_as` and `sqlx::QueryBuilder` are
//! allowed; the compile-time `query!` macros are not.

/// Handle to the SQLite index database. Empty until
/// `designing-database-schema` adds the pool.
#[derive(Clone, Debug, Default)]
pub struct Db;

impl Db {
    pub fn new() -> Self {
        Self
    }
}
