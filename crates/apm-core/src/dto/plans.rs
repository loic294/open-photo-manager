//! Plan DTOs: the generic plan lifecycle (create, read, execute, result) and the selection sets
//! that hand results from one tool to another.
//!
//! Owned by todo `building-plan-engine`. Nothing here is specific to one kind of plan: the
//! parameters of a plan and the options of its execution travel as JSON objects that the planner
//! of the kind parses and documents. A planner keeps its own request and response types in a
//! `dto/<area>.rs` file next to this one, never in this file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::common::PlanSide;
use crate::paths::{FileId, PlanId, SelectionId, TaskId};

/// Kind of plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanKind {
    Duplicates,
    Move,
    Cleanup,
    Rename,
}

/// Where a plan is in its life, as a client sees it. A plan that is still being built is not
/// visible, so there is no `building` state on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanState {
    /// Built and waiting: it can be browsed as an overlay and executed once.
    #[default]
    Ready,
    /// A task is carrying it out; `PlanDto::task_id` names the task.
    Executing,
    /// The execution is over (completed, partial, cancelled or interrupted); `PlanDto::result` says
    /// what happened. A plan is executed at most once.
    Executed,
}

/// Something worth telling the user about a plan, such as files that were left out of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanWarning {
    /// Stable snake_case code a client can switch on, documented by the planner that raises it.
    pub code: String,
    /// A sentence for the user.
    pub message: String,
    /// How many items the warning concerns.
    pub count: u64,
}

/// One item an execution could not process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ItemFailure {
    /// The index id of the file, when the item is a file the index knows.
    pub file_id: Option<FileId>,
    /// The volume name and the path inside the volume ("Photos/2019/IMG_0001.jpg"): never an
    /// absolute path of the host.
    pub path: String,
    /// A stable code of a file operation failure or a planner's safety skip.
    pub code: String,
    /// A sentence for the user.
    pub message: String,
}

/// What an execution did. Every item the execution looked at is counted exactly once in `done`,
/// `skipped` or `failed`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanResult {
    /// Items that were processed.
    pub done: u64,
    /// Items that needed nothing or were safely refused (already in place, stale match, no survivor).
    pub skipped: u64,
    /// Items that could not be processed; `failures` says why.
    pub failed: u64,
    /// Bytes moved, copied or deleted.
    pub bytes: u64,
    /// The first 200 non-success details (failures and coded safety skips), in processing order.
    /// `failed` and `skipped` count their outcomes separately, including omitted details.
    pub failures: Vec<ItemFailure>,
    /// The execution was cancelled before it went through every item.
    pub cancelled: bool,
    /// A sentence about how the execution ended when the counters do not say it: it was
    /// interrupted by a restart, or it failed as a whole.
    pub note: Option<String>,
}

/// A plan as clients see it. The parameters it was built from are not echoed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanDto {
    pub id: PlanId,
    pub kind: PlanKind,
    pub state: PlanState,
    /// Counters of the planner, under stable snake_case keys that the planner documents.
    pub stats: BTreeMap<String, u64>,
    pub warnings: Vec<PlanWarning>,
    /// Set once the plan is executed.
    pub result: Option<PlanResult>,
    /// The `plan_execute` task, once the plan was executed.
    pub task_id: Option<TaskId>,
    /// Epoch seconds.
    pub created_at: i64,
    /// Epoch seconds. Every use of the plan pushes it back: a plan nobody looks at for two hours
    /// is deleted.
    pub expires_at: i64,
}

/// Body of `POST /api/plans`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreatePlanRequest {
    pub kind: PlanKind,
    /// The parameters of the planner of `kind`: a JSON object, documented by that planner.
    #[serde(default)]
    #[ts(type = "Record<string, unknown>")]
    pub params: Value,
    /// The plan this one supersedes (the tool changed a parameter). It is deleted once the new
    /// plan is built, unless it is executing.
    pub replaces: Option<PlanId>,
}

/// Body of `POST /api/plans/{id}/execute`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExecutePlanRequest {
    /// The options of the planner of the plan's kind: a JSON object, documented by that planner.
    #[serde(default)]
    #[ts(type = "Record<string, unknown>")]
    pub options: Value,
    /// Restricts the execution to the files of this selection (the "effective selection" of the
    /// side the action works on). A planner that cannot honour it refuses the request.
    pub only: Option<SelectionId>,
}

/// Body of `POST /api/plans/{id}/selection`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanSelectionRequest {
    /// The side of the plan whose files are selected.
    #[serde(default)]
    pub side: PlanSide,
    /// Keeps only these files among the ones the plan involves (the explicit selection of the
    /// pane). `null`: every file the plan involves on `side`.
    pub restrict_ids: Option<Vec<FileId>>,
}

/// Body of `POST /api/selections`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateSelectionRequest {
    /// At most 1,000,000 ids. Duplicates and ids of files that do not exist are dropped.
    pub ids: Vec<FileId>,
    /// A short note for whoever looks at the selection later.
    pub label: Option<String>,
}

/// A server-side set of files, referenced by `Filter::selection_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SelectionDto {
    pub id: SelectionId,
    /// Files in the selection that still exist.
    pub item_count: u64,
    /// Bytes: the sum of the sizes of those files.
    pub total_size: u64,
    pub label: Option<String>,
    /// Epoch seconds.
    pub created_at: i64,
    /// Epoch seconds: a selection is deleted 24 hours after its last use.
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    fn failure() -> ItemFailure {
        ItemFailure {
            file_id: Some(7),
            path: "Photos/2019/IMG_0001.jpg".to_owned(),
            code: "source_changed".to_owned(),
            message: "the file changed since the plan was built".to_owned(),
        }
    }

    #[test]
    fn plan_kind_uses_snake_case() {
        assert_eq!(roundtrip(&PlanKind::Duplicates), json!("duplicates"));
        assert_eq!(roundtrip(&PlanKind::Move), json!("move"));
        assert_eq!(roundtrip(&PlanKind::Cleanup), json!("cleanup"));
        assert_eq!(roundtrip(&PlanKind::Rename), json!("rename"));
    }

    #[test]
    fn plan_state_has_no_building_state() {
        assert_eq!(roundtrip(&PlanState::Ready), json!("ready"));
        assert_eq!(roundtrip(&PlanState::Executing), json!("executing"));
        assert_eq!(roundtrip(&PlanState::Executed), json!("executed"));
        assert!(serde_json::from_value::<PlanState>(json!("building")).is_err());
        assert_eq!(PlanState::default(), PlanState::Ready);
    }

    #[test]
    fn a_plan_round_trips_with_its_result() {
        let plan = PlanDto {
            id: 3,
            kind: PlanKind::Duplicates,
            state: PlanState::Executed,
            stats: BTreeMap::from([("pairs".to_owned(), 12), ("matched".to_owned(), 9)]),
            warnings: vec![PlanWarning {
                code: "not_hashed".to_owned(),
                message: "3 files are not hashed yet".to_owned(),
                count: 3,
            }],
            result: Some(PlanResult {
                done: 8,
                skipped: 1,
                failed: 1,
                bytes: 4096,
                failures: vec![failure()],
                cancelled: false,
                note: None,
            }),
            task_id: Some(11),
            created_at: 1_700_000_000,
            expires_at: 1_700_007_200,
        };
        let json = roundtrip(&plan);
        assert_eq!(json["state"], json!("executed"));
        assert_eq!(json["stats"], json!({"matched": 9, "pairs": 12}));
        assert_eq!(
            json["result"]["failures"][0]["code"],
            json!("source_changed")
        );
        assert_eq!(json["result"]["note"], json!(null));
        assert!(
            json.get("params").is_none(),
            "the parameters are not echoed"
        );
    }

    #[test]
    fn a_ready_plan_has_neither_result_nor_task() {
        let plan = PlanDto {
            id: 1,
            kind: PlanKind::Cleanup,
            state: PlanState::Ready,
            stats: BTreeMap::new(),
            warnings: Vec::new(),
            result: None,
            task_id: None,
            created_at: 10,
            expires_at: 20,
        };
        let json = roundtrip(&plan);
        assert_eq!(json["result"], json!(null));
        assert_eq!(json["task_id"], json!(null));
        assert_eq!(json["stats"], json!({}));
    }

    #[test]
    fn requests_accept_missing_optional_parts() {
        let create: CreatePlanRequest =
            serde_json::from_value(json!({"kind": "move", "replaces": null})).unwrap();
        assert_eq!(create.kind, PlanKind::Move);
        assert!(
            create.params.is_null(),
            "an absent object is left to the engine"
        );
        assert_eq!(create.replaces, None);

        let execute: ExecutePlanRequest = serde_json::from_value(json!({"only": 5})).unwrap();
        assert!(execute.options.is_null());
        assert_eq!(execute.only, Some(5));

        let selection: PlanSelectionRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(selection.side, PlanSide::Source);
        assert_eq!(selection.restrict_ids, None);

        let body = CreateSelectionRequest {
            ids: vec![3, 1, 2],
            label: Some("duplicates".to_owned()),
        };
        assert_eq!(
            roundtrip(&body),
            json!({"ids": [3, 1, 2], "label": "duplicates"})
        );
    }

    #[test]
    fn requests_carry_their_json_objects_untouched() {
        let create = CreatePlanRequest {
            kind: PlanKind::Duplicates,
            params: json!({"mode": "hash", "nested": {"a": [1, 2, 3]}}),
            replaces: Some(2),
        };
        let json = roundtrip(&create);
        assert_eq!(json["params"]["nested"]["a"], json!([1, 2, 3]));
        assert_eq!(json["replaces"], json!(2));

        let execute = ExecutePlanRequest {
            options: json!({"action": "delete_source"}),
            only: None,
        };
        assert_eq!(
            roundtrip(&execute)["options"]["action"],
            json!("delete_source")
        );

        let selection = PlanSelectionRequest {
            side: PlanSide::Comparison,
            restrict_ids: Some(vec![9, 8]),
        };
        assert_eq!(
            roundtrip(&selection),
            json!({"side": "comparison", "restrict_ids": [9, 8]})
        );
    }

    #[test]
    fn a_selection_carries_its_size() {
        let selection = SelectionDto {
            id: 4,
            item_count: 2988,
            total_size: 13_314_398_617,
            label: None,
            created_at: 100,
            expires_at: 86_500,
        };
        let json = roundtrip(&selection);
        assert_eq!(json["total_size"], json!(13_314_398_617_u64));
        assert_eq!(json["label"], json!(null));
    }

    #[test]
    fn an_interrupted_result_carries_its_note() {
        let result = PlanResult {
            cancelled: true,
            note: Some("interrupted by restart".to_owned()),
            ..PlanResult::default()
        };
        let json = roundtrip(&result);
        assert_eq!(json["cancelled"], json!(true));
        assert_eq!(json["note"], json!("interrupted by restart"));
        assert_eq!(json["failures"], json!([]));
    }
}
