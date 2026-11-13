//! The plan merge of the browse API as pure functions, so every rule of the module docs of
//! [`super`] is tested without a database or a provider.

use std::collections::HashMap;

use opm_core::dto::{FileItem, FolderEntry, FoldersResponse, FooterCounts, Overlay};
use opm_core::names::normalize_name;
use opm_core::paths::FileId;

use crate::plans::overlay::{FolderOverlay, GhostPage, Window};
use crate::query::FolderRow;

/// The folder list: the real rows with what the plan says about them, plus the ghost folders,
/// sorted together by normalised name and then name. The footer is the real counts plus the ghosts.
///
/// A real folder takes `matches` and `overlay` from `per_child` and adds `ghost_count` and
/// `ghost_total` to its `count` and `total`; a folder without an entry is left alone.
pub(super) fn folders(
    rows: Vec<FolderRow>,
    real: FooterCounts,
    overlay: FolderOverlay,
) -> FoldersResponse {
    let FolderOverlay {
        per_child,
        ghost_folders,
        ghosts,
        ..
    } = overlay;
    let mut folders: Vec<FolderEntry> = rows
        .into_iter()
        .map(|row| {
            let info = per_child.get(&row.name).copied();
            let mut entry = FolderEntry::from(row);
            if let Some(info) = info {
                entry.count += info.ghost_count;
                entry.total += info.ghost_total;
                entry.matches = info.matches;
                entry.overlay = info.overlay;
            }
            entry
        })
        .collect();
    folders.extend(ghost_folders);
    folders.sort_by_cached_key(|folder| (normalize_name(&folder.name), folder.name.clone()));
    FoldersResponse {
        folders,
        counts: FooterCounts {
            in_folder: real.in_folder + ghosts.in_folder,
            in_subfolders: real.in_subfolders + ghosts.in_subfolders,
        },
    }
}

/// The part of the ghost list that completes a page.
///
/// A listing is the real files followed by the ghosts. The page `[offset, offset + limit)` takes
/// the real files from `offset` while there are some (`real_on_page` of them), and the ghosts for
/// whatever room that leaves, starting at ghost `max(offset - real_total, 0)`. A page can span the
/// boundary, and a page past the real files is made of ghosts only.
pub(super) fn ghost_window(
    offset: i64,
    limit: i64,
    real_total: i64,
    real_on_page: usize,
) -> Window {
    let real_on_page = i64::try_from(real_on_page).unwrap_or(i64::MAX);
    Window {
        skip: (offset - real_total).max(0),
        take: (limit - real_on_page).max(0),
    }
}

/// One page of items: the real files with the flags the plan gave them (a file without flags
/// keeps its default overlay), then the ghosts, cut to `take` in case the provider returned more.
pub(super) fn items(
    mut real: Vec<FileItem>,
    flags: &HashMap<FileId, Overlay>,
    ghosts: GhostPage,
    take: i64,
) -> Vec<FileItem> {
    for item in &mut real {
        if let Some(overlay) = flags.get(&item.id) {
            item.overlay = *overlay;
        }
    }
    let take = usize::try_from(take).unwrap_or(0);
    real.extend(ghosts.items.into_iter().take(take));
    real
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plans::overlay::FolderPlanInfo;

    fn row(name: &str, count: i64, total: i64) -> FolderRow {
        FolderRow {
            name: name.to_owned(),
            path: name.to_owned(),
            volume_id: 1,
            count,
            total,
            preview: None,
        }
    }

    fn ghost_folder(name: &str, count: i64, total: i64) -> FolderEntry {
        FolderEntry {
            name: name.to_owned(),
            path: name.to_owned(),
            volume_id: 1,
            count,
            total,
            matches: None,
            preview: None,
            overlay: Overlay {
                planned: true,
                ..Overlay::default()
            },
        }
    }

    fn item(id: FileId) -> FileItem {
        FileItem {
            id,
            name: format!("{id}.jpg"),
            ..FileItem::default()
        }
    }

    #[test]
    fn the_ghost_window_follows_the_real_files() {
        // (offset, limit, real_total, real_on_page) -> (skip, take)
        let cases = [
            ((0, 3, 5, 3), (0, 0)),
            ((2, 3, 5, 3), (0, 0)),
            ((3, 4, 5, 2), (0, 2)),
            ((5, 3, 5, 0), (0, 3)),
            ((7, 3, 5, 0), (2, 3)),
            ((0, 200, 0, 0), (0, 200)),
            ((0, 200, 3, 3), (0, 197)),
            ((10, 5, 3, 0), (7, 5)),
        ];
        for ((offset, limit, real_total, on_page), (skip, take)) in cases {
            let window = ghost_window(offset, limit, real_total, on_page);
            assert_eq!(
                (window.skip, window.take),
                (skip, take),
                "offset {offset} limit {limit} real_total {real_total} on page {on_page}"
            );
        }
    }

    #[test]
    fn folders_merge_the_plan_into_the_real_rows() {
        let overlay = FolderOverlay {
            per_child: HashMap::from([
                (
                    "Beta".to_owned(),
                    FolderPlanInfo {
                        matches: Some(1),
                        overlay: Overlay {
                            matched: true,
                            ..Overlay::default()
                        },
                        ghost_count: 4,
                        ghost_total: 5,
                    },
                ),
                (
                    "elsewhere".to_owned(),
                    FolderPlanInfo {
                        ghost_count: 100,
                        ..FolderPlanInfo::default()
                    },
                ),
            ]),
            ghost_folders: vec![ghost_folder("Alpine", 2, 3)],
            ghosts: FooterCounts {
                in_folder: 6,
                in_subfolders: 9,
            },
            dir_planned: true,
        };
        let real = FooterCounts {
            in_folder: 10,
            in_subfolders: 20,
        };

        let response = folders(vec![row("Beta", 2, 3), row("alpha", 0, 0)], real, overlay);

        let names: Vec<&str> = response.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Alpine", "Beta"]);
        let untouched = &response.folders[0];
        assert_eq!((untouched.count, untouched.total), (0, 0));
        assert_eq!(untouched.matches, None);
        assert_eq!(untouched.overlay, Overlay::default());
        let ghost = &response.folders[1];
        assert_eq!((ghost.count, ghost.total), (2, 3));
        assert!(ghost.overlay.planned);
        let touched = &response.folders[2];
        assert_eq!((touched.count, touched.total), (6, 8));
        assert_eq!(touched.matches, Some(1));
        assert!(touched.overlay.matched);
        assert_eq!(
            response.counts,
            FooterCounts {
                in_folder: 16,
                in_subfolders: 29
            }
        );
    }

    #[test]
    fn folders_without_a_plan_are_the_real_rows() {
        let real = FooterCounts {
            in_folder: 1,
            in_subfolders: 2,
        };
        let response = folders(
            vec![row("b", 1, 1), row("a", 2, 2)],
            real,
            FolderOverlay::default(),
        );
        assert_eq!(response.counts, real);
        let listed: Vec<(&str, i64, i64)> = response
            .folders
            .iter()
            .map(|f| (f.name.as_str(), f.count, f.total))
            .collect();
        assert_eq!(listed, [("a", 2, 2), ("b", 1, 1)]);
    }

    #[test]
    fn items_get_their_flags_then_the_ghosts() {
        let flags = HashMap::from([(
            2,
            Overlay {
                will_delete: true,
                ..Overlay::default()
            },
        )]);
        let ghost = |id| FileItem {
            overlay: Overlay {
                planned: true,
                ..Overlay::default()
            },
            ..item(id)
        };
        let ghosts = GhostPage {
            total: 5,
            items: vec![ghost(10), ghost(11), ghost(12)],
        };

        let page = items(vec![item(1), item(2)], &flags, ghosts, 2);

        let ids: Vec<FileId> = page.iter().map(|i| i.id).collect();
        assert_eq!(ids, [1, 2, 10, 11]);
        assert_eq!(page[0].overlay, Overlay::default());
        assert!(page[1].overlay.will_delete);
        assert!(page[2].overlay.planned && page[3].overlay.planned);
    }

    #[test]
    fn a_zero_take_keeps_only_the_real_files() {
        let ghosts = GhostPage {
            total: 1,
            items: vec![item(10)],
        };
        let page = items(vec![item(1)], &HashMap::new(), ghosts, 0);
        assert_eq!(page.len(), 1);
    }
}
