//! API data-transfer types: the single source of truth for the HTTP contract.
//!
//! Every type that crosses the wire is defined here, derives
//! `Debug, Clone, Serialize, Deserialize, TS` and carries `#[ts(export)]`, so
//! `make gen-api` regenerates the complete TypeScript contract into
//! `web/src/api/generated/` (one flat file per type, no barrel file).
//!
//! Conventions:
//! - JSON field names are snake_case; enums use `#[serde(rename_all = "snake_case")]`.
//! - Ids are integers (see `crate::paths`); `i64`/`u64` export as TypeScript `number`.
//! - Request structs carry `#[serde(default)]` where a default is sensible. The
//!   TypeScript fields stay required, so clients always send complete objects.
//! - Type names are globally unique: each type becomes `<TypeName>.ts`.
//! - Each module below is owned by the todo that fills it in; edit only the file
//!   of your own area, never this one.

pub mod browse;
pub mod cleanup;
pub mod common;
pub mod duplicates;
pub mod files;
pub mod format;
pub mod immich;
pub mod index;
pub mod moves;
pub mod plans;
pub mod rename;
pub mod settings;
pub mod tasks;
pub mod trash;

#[cfg(test)]
pub(crate) mod testing;

pub use browse::*;
pub use cleanup::*;
pub use common::*;
pub use duplicates::*;
pub use files::*;
pub use format::*;
// The next three modules are still empty; the allow stays so their owners never
// need to touch this file.
#[allow(unused_imports)]
pub use immich::*;
pub use index::*;
pub use moves::*;
pub use plans::*;
pub use rename::*;
#[allow(unused_imports)]
pub use settings::*;
pub use tasks::*;
#[allow(unused_imports)]
pub use trash::*;
