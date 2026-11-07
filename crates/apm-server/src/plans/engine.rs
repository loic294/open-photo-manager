//! Plan engine: creation, lifecycle (draft, ready, executed, discarded),
//! statistics and execution as a task through `fsops`.
//!
//! Owned by todo `building-plan-engine`. The one and only plan engine: the
//! duplicate, move and cleanup planners plug into it instead of re-implementing
//! it.
