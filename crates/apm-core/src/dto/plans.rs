//! Plan DTOs: duplicate, move and cleanup plans, their statistics and the
//! selection sets used to hand results between tools.
//!
//! Owned by todo `building-plan-engine`. The move planner
//! (`implementing-move-planner`), duplicate matcher
//! (`implementing-duplicate-matching`) and cleanup planner
//! (`implementing-empty-folder-cleanup`) add their request/response types in
//! their own marked section below so that parallel changes merge cleanly.
//!
//! Planned types: `PlanDto`, `PlanStats`, plan creation and execution requests,
//! `SelectionDto`, the format-preview request/response.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Kind of plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanKind {
    Duplicates,
    Move,
    Cleanup,
}

// ---- Plan engine and selections (todo `building-plan-engine`) ----

// ---- Duplicate matching (todo `implementing-duplicate-matching`) ----

// ---- Move planner and format preview (todo `implementing-move-planner`) ----

// ---- Empty-folder cleanup (todo `implementing-empty-folder-cleanup`) ----

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    #[test]
    fn plan_kind_uses_snake_case() {
        assert_eq!(roundtrip(&PlanKind::Duplicates), json!("duplicates"));
        assert_eq!(roundtrip(&PlanKind::Move), json!("move"));
        assert_eq!(roundtrip(&PlanKind::Cleanup), json!("cleanup"));
    }
}
