//! Filesystem watcher: turns `notify` events into debounced incremental
//! re-index requests and `FsChange` server events.
//!
//! Owned by todo `watching-filesystem`.
