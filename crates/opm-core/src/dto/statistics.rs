//! Daily index observations and committed library activity.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatisticsQuery {
    /// Inclusive library-calendar dates. Omit both for the last 30 days.
    pub from: Option<String>,
    pub to: Option<String>,
    pub volume_id: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatisticsKind {
    pub kind: String,
    pub files: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatisticsInventory {
    pub volume_id: i64,
    pub volume_name: String,
    pub online: bool,
    pub baseline_complete: bool,
    pub last_scan_at: Option<i64>,
    pub held_removals: i64,
    pub files: i64,
    pub bytes: i64,
    pub kinds: Vec<StatisticsKind>,
    pub metadata_pending: i64,
    pub metadata_failed: i64,
    pub hashes_pending: i64,
    pub hashes_failed: i64,
    pub thumbnails_pending: i64,
    pub thumbnails_failed: i64,
    /// Trash entries may represent whole folders; this is not a file count.
    pub trash_items: i64,
    pub trash_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatisticsSnapshot {
    pub segment_id: i64,
    pub day: String,
    pub timezone: String,
    pub observed_at: i64,
    pub inventory: StatisticsInventory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatisticsActivity {
    pub segment_id: i64,
    pub day: String,
    pub timezone: String,
    pub volume_id: i64,
    pub volume_name: String,
    /// baseline, added, modified, removed, trashed, duplicates, duplicate_sidecars,
    /// restored, or purged. Purged bytes are logical bytes permanently deleted.
    pub action: String,
    pub count: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatisticsCoverage {
    pub started_at: i64,
    pub last_seen_at: i64,
    pub stopped_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatisticsSegment {
    pub id: i64,
    pub timezone: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StatisticsResponse {
    pub timezone: String,
    pub from: String,
    pub to: String,
    pub observed_at: i64,
    pub started_at: i64,
    pub pending_events: i64,
    pub snapshots: Vec<StatisticsSnapshot>,
    pub activity: Vec<StatisticsActivity>,
    pub coverage: Vec<StatisticsCoverage>,
    pub segments: Vec<StatisticsSegment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_json_round_trip() {
        let value = StatisticsSnapshot {
            segment_id: 1,
            day: "2026-11-13".into(),
            timezone: "America/Los_Angeles".into(),
            observed_at: 1_794_610_000,
            inventory: StatisticsInventory {
                volume_id: 1,
                files: 12,
                kinds: vec![StatisticsKind {
                    kind: "image".into(),
                    files: 12,
                    bytes: 42,
                }],
                ..Default::default()
            },
        };
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<StatisticsSnapshot>(&encoded).unwrap(),
            value
        );
    }
}
