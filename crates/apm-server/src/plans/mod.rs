//! Plans: materialized proposals (duplicate resolution, moves, cleanup) that
//! the user reviews as an overlay before executing them as tasks.
//!
//! The shared engine (`engine`, `store`, `overlay`, `selections`) is owned by
//! todo `building-plan-engine`; each plan kind has its own submodule and owner.

pub mod cleanup;
pub mod duplicates;
pub mod engine;
pub mod moves;
pub mod overlay;
pub mod selections;
pub mod store;

/// Plan service. Empty until `building-plan-engine` implements it.
#[derive(Clone, Debug, Default)]
pub struct PlanService;

impl PlanService {
    pub fn new() -> Self {
        Self
    }
}
