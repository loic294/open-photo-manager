//! Pure domain logic and API data types for Advanced Photo Manager.
//!
//! This crate performs no IO and has no async runtime or database dependency, so
//! its unit tests are fast and deterministic. Every type that crosses the HTTP
//! boundary lives in [`dto`] and is exported to TypeScript by `ts-rs`
//! (`make gen-api`).

pub mod conflict;
pub mod consts;
pub mod datetime;
pub mod dto;
pub mod filter;
pub mod junk;
pub mod kind;
pub mod matching;
pub mod names;
pub mod paths;
pub mod pattern;
pub mod progress;
pub mod sidecar;
