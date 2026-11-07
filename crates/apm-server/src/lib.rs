//! Advanced Photo Manager server: HTTP API, SQLite index, indexer, task engine,
//! file operations, plan engine, media serving and the Immich integration.
//!
//! The binary in `main.rs` is a thin wrapper; everything testable lives in this
//! library so integration tests can build the router in-process.

pub mod api;
pub mod app;
pub mod config;
pub mod db;
pub mod error;
pub mod fsops;
pub mod immich;
pub mod indexer;
pub mod media;
pub mod plans;
pub mod query;
pub mod security;
pub mod settings;
pub mod state;
pub mod tasks;
pub mod trash;
