//! Background task engine: queueing, progress, cancellation, persistence of
//! task state and the broadcast of `ServerEvent`s to SSE subscribers.
//!
//! Owned by todo `building-task-engine` (with `api/tasks.rs` and
//! `api/events.rs`). All long-running work (indexing, file moves, trash,
//! Immich sync) runs as a task through this engine.

/// Task engine. Empty until `building-task-engine` implements it.
#[derive(Clone, Debug, Default)]
pub struct TaskEngine;

impl TaskEngine {
    pub fn new() -> Self {
        Self
    }
}
