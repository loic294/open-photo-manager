//! Pure domain logic and API data types for Open Photo Manager.
//!
//! This crate performs no filesystem, network or database IO of its own and has no async runtime
//! dependency (`hash` only wraps readers it is handed), so its unit tests are fast and
//! deterministic. Every type that crosses the HTTP
//! boundary lives in [`dto`] and is exported to TypeScript by `ts-rs`
//! (`make gen-api`).

pub mod conflict;
pub mod consts;
pub mod datetime;
pub mod dto;
pub mod filter;
pub mod hash;
pub mod junk;
pub mod kind;
pub mod matching;
pub mod metadata;
pub mod mounts;
pub mod names;
pub mod paths;
pub mod pattern;
pub mod progress;
pub mod rename;
pub mod sidecar;
