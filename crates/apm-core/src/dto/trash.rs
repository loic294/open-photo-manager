//! Trash DTOs: the listing with its summary, the deletions (batches) and the restore and empty
//! requests.
//!
//! Owned by todo `building-trash-management`.
//!
//! A batch is the folder `.apm-trash/<batch>/` that one operation trashed into, so it is one
//! deletion as the user sees it. A batch is identified by `(volume_id, batch)`: a task that trashes
//! on two volumes uses the same batch name on both.
//!
//! The two GET queries are read with [`TrashListQuery::from_pairs`] and
//! [`TrashBatchesQuery::from_pairs`] (not with serde), so a bad value is answered with a 422 that
//! names every rejected field instead of the first parse error.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::common::{ConflictPolicy, SortOrder};
use super::settings::FieldError;
use crate::paths::{RelPath, TaskId, TrashItemId, VolumeId};

/// Page size used when a query does not say.
const DEFAULT_LIMIT: u32 = 50;
/// Largest page a client can ask for.
const MAX_LIMIT: u32 = 200;
/// The wire names of the sorts, in the order the error message lists them.
const SORT_NAMES: [(&str, TrashSort); 4] = [
    ("trashed_at", TrashSort::TrashedAt),
    ("name", TrashSort::Name),
    ("size", TrashSort::Size),
    ("path", TrashSort::Path),
];

/// One file or folder in the trash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashItemDto {
    pub id: TrashItemId,
    pub volume_id: VolumeId,
    /// Name of the volume, for display.
    pub volume: String,
    /// File or folder name: the last component of the original path.
    pub name: String,
    /// Folder the item was in, relative to the volume root (`""` for the root).
    pub dir: String,
    pub is_dir: bool,
    /// Bytes. For a folder: the indexed size of its content when it was trashed.
    pub size: u64,
    /// Why it was trashed: `user`, `replaced`, `junk`, `moved` or another free text; `null` for an
    /// item found on disk by the sync.
    pub reason: Option<String>,
    /// The folder below `.apm-trash/` that holds the item: one deletion.
    pub batch: String,
    /// The task that trashed it, when it is known.
    pub task_id: Option<TaskId>,
    /// Epoch seconds.
    pub trashed_at: i64,
    /// The item carries what the index knew when it was trashed (a file's metadata and tags, a
    /// folder's counts). `false` for an item found on disk by the sync: restoring it rescans.
    pub has_snapshot: bool,
}

/// What the trash listing is sorted by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TrashSort {
    /// When the item was trashed.
    #[default]
    TrashedAt,
    /// The file or folder name, ignoring the case of ASCII letters.
    Name,
    Size,
    /// The original path, ignoring the case of ASCII letters.
    Path,
}

impl TrashSort {
    /// The sort a query string value names.
    pub fn parse(value: &str) -> Option<Self> {
        SORT_NAMES
            .iter()
            .find_map(|(name, sort)| (*name == value).then_some(*sort))
    }

    /// The direction a query without `order` gets: newest and largest first, names from A to Z.
    pub fn natural_order(self) -> SortOrder {
        match self {
            Self::TrashedAt | Self::Size => SortOrder::Desc,
            Self::Name | Self::Path => SortOrder::Asc,
        }
    }
}

/// Query of `GET /api/trash`. Every field is optional on the wire; the TypeScript type needs them
/// all, so a client sends `null` for what it does not filter on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct TrashListQuery {
    /// Only the items of this volume.
    pub volume_id: Option<VolumeId>,
    /// Only the items of this batch. Needs `volume_id`: batch names repeat across volumes.
    pub batch: Option<String>,
    /// Text the original path contains, ignoring the case of ASCII letters (`%` and `_` are
    /// ordinary characters).
    pub q: Option<String>,
    pub sort: TrashSort,
    /// `null` is the natural direction of `sort`: newest and largest first, names from A to Z.
    pub order: Option<SortOrder>,
    pub offset: u32,
    /// Page size, 1 to 200 (default 50).
    pub limit: u32,
}

impl Default for TrashListQuery {
    fn default() -> Self {
        Self {
            volume_id: None,
            batch: None,
            q: None,
            sort: TrashSort::default(),
            order: None,
            offset: 0,
            limit: DEFAULT_LIMIT,
        }
    }
}

impl TrashListQuery {
    /// Largest page.
    pub const MAX_LIMIT: u32 = MAX_LIMIT;

    /// The direction the listing is sorted in.
    pub fn effective_order(&self) -> SortOrder {
        self.order.unwrap_or_else(|| self.sort.natural_order())
    }

    /// Reads the `key=value` pairs of a query string. An empty value is an absent one, an unknown
    /// key is ignored and a repeated key keeps its last value. Every rejected value is reported.
    pub fn from_pairs<'a>(
        pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, Vec<FieldError>> {
        let mut query = Self::default();
        let mut errors = Vec::new();
        for (key, value) in pairs {
            match key {
                "volume_id" => query.volume_id = read_volume(value, &mut errors),
                "batch" => query.batch = text(value),
                "q" => query.q = text(value),
                "sort" if !value.is_empty() => match TrashSort::parse(value) {
                    Some(sort) => query.sort = sort,
                    None => errors.push(FieldError::new(
                        "sort",
                        "must be one of trashed_at, name, size, path",
                    )),
                },
                "order" => query.order = read_order(value, &mut errors),
                "offset" => read_offset(value, &mut query.offset, &mut errors),
                "limit" => read_limit(value, &mut query.limit, &mut errors),
                _ => {}
            }
        }
        if query.batch.is_some() && query.volume_id.is_none() {
            errors.push(FieldError::new(
                "batch",
                "needs volume_id: batch names repeat across volumes",
            ));
        }
        if errors.is_empty() {
            Ok(query)
        } else {
            Err(errors)
        }
    }
}

/// What the trash holds now, and the settings its page quotes. The numbers are the ones an
/// "Empty trash" confirmation shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashSummary {
    /// Items in the whole trash, whatever the filters of the request.
    pub items: u64,
    /// Bytes in the whole trash.
    pub bytes: u64,
    /// The highest item id in the trash (0 when it is empty). `POST /api/trash/empty` takes it as
    /// `up_to_id`, so only what the user has seen is deleted.
    pub max_id: i64,
    /// The volumes that hold trash, by name.
    pub volumes: Vec<TrashVolumeSummary>,
    /// Days after which trashed items are deleted automatically; `null` keeps them until
    /// "Empty trash".
    pub retention_days: Option<u32>,
    /// What a restore does when an original name is taken again (the `default_conflict_policy`
    /// setting).
    pub conflict_policy: ConflictPolicy,
}

/// The trash of one volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashVolumeSummary {
    pub volume_id: VolumeId,
    pub name: String,
    pub items: u64,
    pub bytes: u64,
    /// The volume is reachable now. Nothing can be restored from or emptied on an offline volume.
    pub online: bool,
    /// The volume accepts changes. A read-only volume cannot be restored to or emptied either.
    pub writable: bool,
}

/// Answer of `GET /api/trash`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashListResponse {
    /// One page.
    pub items: Vec<TrashItemDto>,
    /// Items that match the filters.
    pub total: u64,
    pub summary: TrashSummary,
}

/// All the items one operation trashed on one volume: one deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashBatchDto {
    pub volume_id: VolumeId,
    /// Name of the volume, for display.
    pub volume: String,
    pub batch: String,
    /// The task that trashed the items, when it is known.
    pub task_id: Option<TaskId>,
    /// Epoch seconds: when the first item of the batch was trashed.
    pub trashed_at: i64,
    /// Items still in the trash.
    pub items: u64,
    pub bytes: u64,
    /// The distinct reasons of the items (at most 5), most frequent first. Items without a reason
    /// add none.
    pub reasons: Vec<String>,
}

/// Query of `GET /api/trash/batches`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct TrashBatchesQuery {
    /// Only the batches of this volume.
    pub volume_id: Option<VolumeId>,
    pub offset: u32,
    /// Page size, 1 to 200 (default 50).
    pub limit: u32,
}

impl Default for TrashBatchesQuery {
    fn default() -> Self {
        Self {
            volume_id: None,
            offset: 0,
            limit: DEFAULT_LIMIT,
        }
    }
}

impl TrashBatchesQuery {
    /// Reads the `key=value` pairs of a query string, like [`TrashListQuery::from_pairs`].
    pub fn from_pairs<'a>(
        pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, Vec<FieldError>> {
        let mut query = Self::default();
        let mut errors = Vec::new();
        for (key, value) in pairs {
            match key {
                "volume_id" => query.volume_id = read_volume(value, &mut errors),
                "offset" => read_offset(value, &mut query.offset, &mut errors),
                "limit" => read_limit(value, &mut query.limit, &mut errors),
                _ => {}
            }
        }
        if errors.is_empty() {
            Ok(query)
        } else {
            Err(errors)
        }
    }
}

/// Answer of `GET /api/trash/batches`, newest deletion first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashBatchesResponse {
    pub batches: Vec<TrashBatchDto>,
    /// Batches that match the query.
    pub total: u64,
    pub summary: TrashSummary,
}

/// What a restore brings back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum TrashTarget {
    /// These items (1 to 10,000), for example the rows the user ticked.
    Ids { ids: Vec<TrashItemId> },
    /// Everything of one batch that is still in the trash: "Undo deletion".
    Batch { volume_id: VolumeId, batch: String },
}

/// Body of `POST /api/trash/restore`. The request never carries paths: the items come back to the
/// place the trash remembers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashRestoreRequest {
    pub target: TrashTarget,
    /// What to do when an original name is taken again; `null` is the `default_conflict_policy`
    /// setting.
    pub conflict_policy: Option<ConflictPolicy>,
}

impl TrashRestoreRequest {
    /// Most items one request may name.
    pub const MAX_IDS: usize = 10_000;

    /// Rejects an empty or oversized selection and a batch name that is not one folder name.
    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let error = match &self.target {
            TrashTarget::Ids { ids } if ids.is_empty() => {
                Some(FieldError::new("target.ids", "select at least one item"))
            }
            TrashTarget::Ids { ids } if ids.len() > Self::MAX_IDS => Some(FieldError::new(
                "target.ids",
                format!("at most {} items at a time", Self::MAX_IDS),
            )),
            TrashTarget::Ids { .. } => None,
            TrashTarget::Batch { batch, .. } => check_batch_name(batch, "target.batch"),
        };
        error.map_or(Ok(()), |error| Err(vec![error]))
    }
}

/// Body of `POST /api/trash/empty`: the only way to delete for good.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TrashEmptyRequest {
    /// Only this volume's trash; `null` is every volume.
    pub volume_id: Option<VolumeId>,
    /// The `summary.max_id` the user saw. Items trashed after that are kept.
    pub up_to_id: i64,
}

impl TrashEmptyRequest {
    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        if self.up_to_id < 0 {
            return Err(vec![FieldError::new("up_to_id", "must not be negative")]);
        }
        Ok(())
    }
}

/// A batch name is one folder name: the request must not be able to name a path.
fn check_batch_name(batch: &str, field: &str) -> Option<FieldError> {
    RelPath::root()
        .join(batch)
        .err()
        .map(|_| FieldError::new(field, "must be the name of one trash batch, not a path"))
}

/// Empty text is "not set".
fn text(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn read_volume(value: &str, errors: &mut Vec<FieldError>) -> Option<VolumeId> {
    if value.is_empty() {
        return None;
    }
    match value.parse() {
        Ok(id) => Some(id),
        Err(_) => {
            errors.push(FieldError::new("volume_id", "must be a whole number"));
            None
        }
    }
}

fn read_order(value: &str, errors: &mut Vec<FieldError>) -> Option<SortOrder> {
    match value {
        "" => None,
        "asc" => Some(SortOrder::Asc),
        "desc" => Some(SortOrder::Desc),
        _ => {
            errors.push(FieldError::new("order", "must be asc or desc"));
            None
        }
    }
}

fn read_offset(value: &str, offset: &mut u32, errors: &mut Vec<FieldError>) {
    if value.is_empty() {
        return;
    }
    match value.parse() {
        Ok(parsed) => *offset = parsed,
        Err(_) => errors.push(FieldError::new(
            "offset",
            "must be a whole number, 0 or more",
        )),
    }
}

fn read_limit(value: &str, limit: &mut u32, errors: &mut Vec<FieldError>) {
    if value.is_empty() {
        return;
    }
    match value.parse::<u32>() {
        Ok(parsed) if (1..=MAX_LIMIT).contains(&parsed) => *limit = parsed,
        _ => errors.push(FieldError::new(
            "limit",
            format!("must be a whole number from 1 to {MAX_LIMIT}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::testing::roundtrip;

    fn field_names(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|error| error.field.as_str()).collect()
    }

    fn summary() -> TrashSummary {
        TrashSummary {
            items: 3,
            bytes: 4_500,
            max_id: 17,
            volumes: vec![TrashVolumeSummary {
                volume_id: 2,
                name: "photos".to_owned(),
                items: 3,
                bytes: 4_500,
                online: true,
                writable: false,
            }],
            retention_days: Some(30),
            conflict_policy: ConflictPolicy::Smart,
        }
    }

    #[test]
    fn item_roundtrips_with_nullable_fields() {
        let wire = roundtrip(&TrashItemDto {
            id: 9,
            volume_id: 2,
            volume: "photos".to_owned(),
            name: "IMG_1.jpg".to_owned(),
            dir: "2019/trip".to_owned(),
            is_dir: false,
            size: 1_234,
            reason: None,
            batch: "20250101-120000-t5".to_owned(),
            task_id: Some(5),
            trashed_at: 1_735_732_800,
            has_snapshot: true,
        });
        assert_eq!(wire["reason"], json!(null));
        assert_eq!(wire["task_id"], json!(5));
        assert_eq!(wire["has_snapshot"], json!(true));
    }

    #[test]
    fn responses_roundtrip() {
        let wire = roundtrip(&TrashListResponse {
            items: Vec::new(),
            total: 0,
            summary: summary(),
        });
        assert_eq!(wire["summary"]["max_id"], json!(17));
        assert_eq!(wire["summary"]["conflict_policy"], json!("smart"));
        assert_eq!(wire["summary"]["volumes"][0]["writable"], json!(false));

        let wire = roundtrip(&TrashBatchesResponse {
            batches: vec![TrashBatchDto {
                volume_id: 2,
                volume: "photos".to_owned(),
                batch: "20250101-120000-t5".to_owned(),
                task_id: None,
                trashed_at: 1,
                items: 4,
                bytes: 8,
                reasons: vec!["user".to_owned(), "replaced".to_owned()],
            }],
            total: 1,
            summary: summary(),
        });
        assert_eq!(wire["batches"][0]["reasons"], json!(["user", "replaced"]));
    }

    #[test]
    fn sort_uses_snake_case_and_has_a_natural_direction() {
        assert_eq!(roundtrip(&TrashSort::TrashedAt), json!("trashed_at"));
        assert_eq!(roundtrip(&TrashSort::Path), json!("path"));
        assert_eq!(TrashSort::default(), TrashSort::TrashedAt);
        for (name, sort) in SORT_NAMES {
            assert_eq!(TrashSort::parse(name), Some(sort));
            assert_eq!(roundtrip(&sort), json!(name));
        }
        assert_eq!(TrashSort::parse("Name"), None);
        assert_eq!(TrashSort::TrashedAt.natural_order(), SortOrder::Desc);
        assert_eq!(TrashSort::Size.natural_order(), SortOrder::Desc);
        assert_eq!(TrashSort::Name.natural_order(), SortOrder::Asc);
        assert_eq!(TrashSort::Path.natural_order(), SortOrder::Asc);
    }

    #[test]
    fn list_query_defaults_to_the_newest_page() {
        let query = TrashListQuery::from_pairs([]).unwrap();
        assert_eq!(query, TrashListQuery::default());
        assert_eq!(query.limit, 50);
        assert_eq!(query.effective_order(), SortOrder::Desc);
        let parsed: TrashListQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, query);
    }

    #[test]
    fn list_query_reads_every_field() {
        let query = TrashListQuery::from_pairs([
            ("volume_id", "2"),
            ("batch", "20250101-120000-t5"),
            ("q", "100%_beach"),
            ("sort", "name"),
            ("order", "desc"),
            ("offset", "100"),
            ("limit", "200"),
        ])
        .unwrap();
        // Spelled out without `..` so a new field has to be added to this test (and to the parser).
        assert_eq!(
            query,
            TrashListQuery {
                volume_id: Some(2),
                batch: Some("20250101-120000-t5".to_owned()),
                q: Some("100%_beach".to_owned()),
                sort: TrashSort::Name,
                order: Some(SortOrder::Desc),
                offset: 100,
                limit: 200,
            }
        );
        roundtrip(&query);
        assert_eq!(query.effective_order(), SortOrder::Desc);
    }

    #[test]
    fn list_query_treats_empty_values_and_unknown_keys_as_absent() {
        let query = TrashListQuery::from_pairs([
            ("volume_id", ""),
            ("batch", ""),
            ("q", ""),
            ("sort", ""),
            ("order", ""),
            ("offset", ""),
            ("limit", ""),
            ("page", "7"),
        ])
        .unwrap();
        assert_eq!(query, TrashListQuery::default());
    }

    #[test]
    fn list_query_keeps_the_last_of_a_repeated_key() {
        let query =
            TrashListQuery::from_pairs([("sort", "size"), ("sort", "path"), ("limit", "5")])
                .unwrap();
        assert_eq!(query.sort, TrashSort::Path);
        assert_eq!(query.limit, 5);
    }

    #[test]
    fn list_query_reports_every_rejected_field() {
        let errors = TrashListQuery::from_pairs([
            ("volume_id", "one"),
            ("sort", "date"),
            ("order", "up"),
            ("offset", "-1"),
            ("limit", "0"),
        ])
        .unwrap_err();
        assert_eq!(
            field_names(&errors),
            ["volume_id", "sort", "order", "offset", "limit"]
        );
        assert!(errors[1].message.contains("trashed_at"));
        for limit in ["0", "201", "abc", "-5", "99999999999"] {
            let errors = TrashListQuery::from_pairs([("limit", limit)]).unwrap_err();
            assert_eq!(field_names(&errors), ["limit"], "limit {limit}");
        }
        for limit in ["1", "200"] {
            assert!(TrashListQuery::from_pairs([("limit", limit)]).is_ok());
        }
    }

    #[test]
    fn a_batch_filter_needs_its_volume() {
        let errors = TrashListQuery::from_pairs([("batch", "20250101-120000-t5")]).unwrap_err();
        assert_eq!(field_names(&errors), ["batch"]);
        assert!(
            TrashListQuery::from_pairs([("batch", "x"), ("volume_id", "1")]).is_ok(),
            "any batch name is accepted when the volume is there"
        );
    }

    #[test]
    fn batches_query_reads_its_fields_and_rejects_the_rest() {
        let query = TrashBatchesQuery::from_pairs([
            ("volume_id", "3"),
            ("offset", "50"),
            ("limit", "25"),
            ("sort", "name"),
        ])
        .unwrap();
        assert_eq!(
            query,
            TrashBatchesQuery {
                volume_id: Some(3),
                offset: 50,
                limit: 25,
            }
        );
        roundtrip(&query);
        assert_eq!(
            TrashBatchesQuery::from_pairs([]).unwrap(),
            TrashBatchesQuery::default()
        );
        let errors =
            TrashBatchesQuery::from_pairs([("volume_id", "x"), ("limit", "500")]).unwrap_err();
        assert_eq!(field_names(&errors), ["volume_id", "limit"]);
    }

    #[test]
    fn restore_target_is_tagged_by_kind() {
        let wire = roundtrip(&TrashRestoreRequest {
            target: TrashTarget::Ids { ids: vec![1, 2] },
            conflict_policy: Some(ConflictPolicy::Rename),
        });
        assert_eq!(
            wire,
            json!({"target": {"kind": "ids", "ids": [1, 2]}, "conflict_policy": "rename"})
        );
        let wire = roundtrip(&TrashRestoreRequest {
            target: TrashTarget::Batch {
                volume_id: 2,
                batch: "20250101-120000-t5".to_owned(),
            },
            conflict_policy: None,
        });
        assert_eq!(wire["target"]["kind"], json!("batch"));
        assert_eq!(wire["conflict_policy"], json!(null));
        let parsed: TrashRestoreRequest =
            serde_json::from_value(json!({"target": {"kind": "ids", "ids": [4]}})).unwrap();
        assert_eq!(parsed.conflict_policy, None);
    }

    #[test]
    fn restore_validation_rejects_empty_oversized_and_path_like_targets() {
        let ids = |ids: Vec<TrashItemId>| TrashRestoreRequest {
            target: TrashTarget::Ids { ids },
            conflict_policy: None,
        };
        let batch = |name: &str| TrashRestoreRequest {
            target: TrashTarget::Batch {
                volume_id: 1,
                batch: name.to_owned(),
            },
            conflict_policy: None,
        };
        assert!(ids(vec![1]).validate().is_ok());
        assert!(
            ids((1..=TrashRestoreRequest::MAX_IDS as i64).collect())
                .validate()
                .is_ok()
        );
        let errors = ids(Vec::new()).validate().unwrap_err();
        assert_eq!(field_names(&errors), ["target.ids"]);
        let errors = ids((1..=TrashRestoreRequest::MAX_IDS as i64 + 1).collect())
            .validate()
            .unwrap_err();
        assert_eq!(field_names(&errors), ["target.ids"]);

        assert!(batch("20250101-120000-t5").validate().is_ok());
        assert!(batch("any odd name.v2").validate().is_ok());
        for bad in ["", ".", "..", "a/b", "../x", "/abs", "nul\0"] {
            let errors = batch(bad).validate().unwrap_err();
            assert_eq!(field_names(&errors), ["target.batch"], "batch {bad:?}");
        }
    }

    #[test]
    fn empty_request_roundtrips_and_rejects_a_negative_cutoff() {
        let wire = roundtrip(&TrashEmptyRequest {
            volume_id: Some(2),
            up_to_id: 17,
        });
        assert_eq!(wire, json!({"volume_id": 2, "up_to_id": 17}));
        let parsed: TrashEmptyRequest = serde_json::from_value(json!({"up_to_id": 0})).unwrap();
        assert_eq!(parsed.volume_id, None);
        assert!(parsed.validate().is_ok());
        let errors = TrashEmptyRequest {
            volume_id: None,
            up_to_id: -1,
        }
        .validate()
        .unwrap_err();
        assert_eq!(field_names(&errors), ["up_to_id"]);
    }
}
