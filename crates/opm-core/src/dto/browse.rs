//! Browse DTOs: folder tree, item listing, id lists and facets.
//!
//! Owned by todo `building-browse-api`.
//!
//! `BrowseRequest` is also the body of `POST /api/facets`; `ItemsRequest` is the
//! body of `POST /api/browse/items` and `POST /api/browse/ids` (offset and limit
//! are ignored for ids).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::common::{Filter, Overlay, PlanRef, SortKey, SortOrder};
use super::files::FileItem;
use crate::paths::{FileId, VolumeId};

/// Page size used when a request does not specify a limit.
pub const DEFAULT_PAGE_SIZE: u32 = 200;

/// Browse context shared by the folders, items, ids and facets endpoints.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct BrowseRequest {
    /// Volume to browse; `null` means every volume.
    pub volume_id: Option<VolumeId>,
    /// Directory relative to the volume root, `/`-separated. `""` is the root.
    pub dir: String,
    pub filter: Filter,
    /// Active plan whose overlay should be computed, if any.
    pub plan: Option<PlanRef>,
}

/// A browse context plus sorting and paging. The browse fields are flattened
/// into the same JSON object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
#[ts(export)]
pub struct ItemsRequest {
    #[serde(flatten)]
    #[ts(flatten)]
    pub browse: BrowseRequest,
    pub sort: SortKey,
    pub order: SortOrder,
    pub offset: u32,
    /// Page size; the server clamps it to its maximum.
    pub limit: u32,
}

impl Default for ItemsRequest {
    fn default() -> Self {
        Self {
            browse: BrowseRequest::default(),
            sort: SortKey::default(),
            order: SortOrder::default(),
            offset: 0,
            limit: DEFAULT_PAGE_SIZE,
        }
    }
}

/// An indexed photo used as a folder preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FolderPreview {
    pub id: FileId,
    pub mtime: i64,
}

/// A subfolder row in the folder tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FolderEntry {
    /// Last path component.
    pub name: String,
    /// Full path from the volume root, `/`-separated.
    pub path: String,
    pub volume_id: VolumeId,
    /// Items below this folder (recursive) that match the filter.
    pub count: i64,
    /// Items below this folder (recursive), ignoring the filter.
    pub total: i64,
    /// Plan matches below this folder; only present when a plan is active.
    pub matches: Option<i64>,
    /// A matching image anywhere below this folder, or null when none exists.
    pub preview: Option<FolderPreview>,
    pub overlay: Overlay,
}

/// Counts shown in the pane footer, both honouring the filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FooterCounts {
    /// Items directly in the current folder.
    pub in_folder: i64,
    /// Items in the folders below the current one.
    pub in_subfolders: i64,
}

/// Response of `POST /api/browse/folders`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FoldersResponse {
    pub folders: Vec<FolderEntry>,
    pub counts: FooterCounts,
}

/// Response of `POST /api/browse/items`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ItemsResponse {
    pub items: Vec<FileItem>,
    /// Total matching items, ignoring offset and limit.
    pub total: i64,
    /// Offset of the first returned item.
    pub offset: u32,
}

/// Response of `POST /api/browse/ids`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IdsResponse {
    pub ids: Vec<FileId>,
}

/// One value of a facet with the number of items that carry it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FacetCount {
    pub value: String,
    pub count: i64,
}

/// Response of `POST /api/facets`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Facets {
    pub cameras: Vec<FacetCount>,
    pub tags: Vec<FacetCount>,
    /// `value` is the star rating as a string (`"0"` = unrated).
    pub ratings: Vec<FacetCount>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dto::common::{FlagFilter, PlanSide};
    use crate::dto::testing::roundtrip;

    fn sample_browse() -> BrowseRequest {
        BrowseRequest {
            volume_id: Some(1),
            dir: "2024".to_owned(),
            filter: Filter {
                cameras: vec!["Canon EOS R5".to_owned()],
                ratings: vec![5],
                flags: vec![FlagFilter::Selected],
                tags: vec!["family".to_owned()],
                selection_id: None,
            },
            plan: Some(PlanRef {
                plan_id: 7,
                side: PlanSide::Source,
            }),
        }
    }

    #[test]
    fn browse_request_roundtrips_and_defaults() {
        roundtrip(&sample_browse());
        let parsed: BrowseRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, BrowseRequest::default());
    }

    #[test]
    fn items_request_is_flat_on_the_wire() {
        let request = ItemsRequest {
            browse: sample_browse(),
            sort: SortKey::Date,
            order: SortOrder::Desc,
            offset: 400,
            limit: 100,
        };
        let wire = roundtrip(&request);
        let keys: Vec<&str> = wire
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "volume_id",
            "dir",
            "filter",
            "plan",
            "sort",
            "order",
            "offset",
            "limit",
        ] {
            assert!(keys.contains(&key), "missing flat key {key}: {keys:?}");
        }
        assert!(!keys.contains(&"browse"), "browse must be flattened");
        assert_eq!(wire["sort"], json!("date"));
        assert_eq!(wire["filter"]["ratings"], json!([5]));
    }

    #[test]
    fn items_request_parses_flat_json_and_applies_defaults() {
        let parsed: ItemsRequest =
            serde_json::from_str(r#"{"volume_id":3,"dir":"a/b","sort":"size","limit":25}"#)
                .unwrap();
        assert_eq!(parsed.browse.volume_id, Some(3));
        assert_eq!(parsed.browse.dir, "a/b");
        assert_eq!(parsed.browse.filter, Filter::default());
        assert_eq!(parsed.sort, SortKey::Size);
        assert_eq!(parsed.order, SortOrder::Asc);
        assert_eq!(parsed.offset, 0);
        assert_eq!(parsed.limit, 25);

        let empty: ItemsRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, ItemsRequest::default());
        assert_eq!(empty.limit, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn responses_roundtrip() {
        let folder = FolderEntry {
            name: "Trip".to_owned(),
            path: "2024/Trip".to_owned(),
            volume_id: 1,
            count: 12,
            total: 30,
            matches: Some(4),
            preview: Some(FolderPreview { id: 8, mtime: 123 }),
            overlay: Overlay {
                matched: true,
                planned: true,
                will_delete: true,
            },
        };
        let wire = roundtrip(&FoldersResponse {
            folders: vec![folder],
            counts: FooterCounts {
                in_folder: 5,
                in_subfolders: 25,
            },
        });
        assert_eq!(wire["folders"][0]["matches"], json!(4));
        assert_eq!(
            wire["folders"][0]["preview"],
            json!({"id": 8, "mtime": 123})
        );
        assert_eq!(wire["counts"]["in_subfolders"], json!(25));

        roundtrip(&ItemsResponse {
            items: vec![FileItem::default()],
            total: 1,
            offset: 0,
        });
        roundtrip(&IdsResponse { ids: vec![1, 2, 3] });
        roundtrip(&Facets {
            cameras: vec![FacetCount {
                value: "Canon EOS R5".to_owned(),
                count: 10,
            }],
            tags: vec![],
            ratings: vec![FacetCount {
                value: "5".to_owned(),
                count: 2,
            }],
        });
    }
}
