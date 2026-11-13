//! A hand-written [`PlanOverlay`] for tests: the answers are plain data, every call is recorded,
//! and it can be made to fail. It proves the browse API against the trait alone, with no plan
//! engine behind it.
//!
//! ```ignore
//! let plan = FakePlan::default()
//!     .with_ghosts(vec![ghost(7, volume, "inbox", "IMG_0001.CR3")])
//!     .with_flags([(7, Overlay { matched: true, ..Overlay::default() })]);
//! let response = service::items(&db, &plan, &request).await.unwrap();
//! assert_eq!(plan.calls().len(), 2);
//! ```
//!
//! The ghost list is returned exactly as given, whatever the view says: tests pass it already in
//! the order the browse API asks for, and read the view back from [`FakePlan::calls`]. The window
//! is honoured unless [`FakePlan::ignoring_the_window`] asks for a provider that does not.

use std::collections::HashMap;
use std::sync::Mutex;

use futures_util::future::BoxFuture;
use opm_core::dto::{FileItem, FolderEntry, FooterCounts, Overlay, PlanRef};
use opm_core::paths::{FileId, VolumeId};
use opm_server::error::{AppError, AppResult};
use opm_server::plans::overlay::{
    FolderOverlay, FolderPlanInfo, GhostPage, GhostView, PlanOverlay, Window,
};

/// One call the browse API made to the provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    ItemFlags {
        plan: PlanRef,
        ids: Vec<FileId>,
    },
    GhostItems {
        plan: PlanRef,
        view: GhostView,
        window: Window,
    },
    FolderOverlay {
        plan: PlanRef,
        view: GhostView,
        real_children: Vec<String>,
    },
}

#[derive(Default)]
pub struct FakePlan {
    flags: HashMap<FileId, Overlay>,
    ghosts: Vec<FileItem>,
    folder: FolderOverlay,
    failure: Option<fn() -> AppError>,
    whole_list: bool,
    calls: Mutex<Vec<Call>>,
}

impl FakePlan {
    /// A provider that answers every call with the error `failure` builds.
    pub fn failing(failure: fn() -> AppError) -> Self {
        Self {
            failure: Some(failure),
            ..Self::default()
        }
    }

    /// A provider that breaks the contract: `ghost_items` returns the whole ghost list from its
    /// start, whatever the window.
    pub fn ignoring_the_window(mut self) -> Self {
        self.whole_list = true;
        self
    }

    /// The flags `item_flags` reports, by file id.
    pub fn with_flags(mut self, flags: impl IntoIterator<Item = (FileId, Overlay)>) -> Self {
        self.flags = flags.into_iter().collect();
        self
    }

    /// The ghost files of every view, in listing order.
    pub fn with_ghosts(mut self, ghosts: Vec<FileItem>) -> Self {
        self.ghosts = ghosts;
        self
    }

    /// The answer of `folder_overlay`.
    pub fn with_folder_overlay(mut self, folder: FolderOverlay) -> Self {
        self.folder = folder;
        self
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }

    fn check(&self) -> AppResult<()> {
        match self.failure {
            Some(failure) => Err(failure()),
            None => Ok(()),
        }
    }
}

impl PlanOverlay for FakePlan {
    fn item_flags<'a>(
        &'a self,
        plan: PlanRef,
        ids: &'a [FileId],
    ) -> BoxFuture<'a, AppResult<HashMap<FileId, Overlay>>> {
        self.record(Call::ItemFlags {
            plan,
            ids: ids.to_vec(),
        });
        Box::pin(async move {
            self.check()?;
            Ok(ids
                .iter()
                .filter_map(|id| self.flags.get(id).map(|flags| (*id, *flags)))
                .collect())
        })
    }

    fn ghost_items<'a>(
        &'a self,
        plan: PlanRef,
        view: &'a GhostView,
        window: Window,
    ) -> BoxFuture<'a, AppResult<GhostPage>> {
        self.record(Call::GhostItems {
            plan,
            view: view.clone(),
            window,
        });
        Box::pin(async move {
            self.check()?;
            let (skip, take) = if self.whole_list {
                (0, self.ghosts.len())
            } else {
                (
                    usize::try_from(window.skip).unwrap(),
                    usize::try_from(window.take).unwrap(),
                )
            };
            Ok(GhostPage {
                total: i64::try_from(self.ghosts.len()).unwrap(),
                items: self.ghosts.iter().skip(skip).take(take).cloned().collect(),
            })
        })
    }

    fn folder_overlay<'a>(
        &'a self,
        plan: PlanRef,
        view: &'a GhostView,
        real_children: &'a [String],
    ) -> BoxFuture<'a, AppResult<FolderOverlay>> {
        self.record(Call::FolderOverlay {
            plan,
            view: view.clone(),
            real_children: real_children.to_vec(),
        });
        Box::pin(async move {
            self.check()?;
            Ok(self.folder.clone())
        })
    }
}

/// A ghost row: `id` is the real source file, the rest is the planned location.
pub fn ghost(id: FileId, volume_id: VolumeId, dir: &str, name: &str) -> FileItem {
    FileItem {
        id,
        volume_id,
        dir: dir.to_owned(),
        name: name.to_owned(),
        overlay: Overlay {
            planned: true,
            ..Overlay::default()
        },
        ..FileItem::default()
    }
}

/// A ghost folder row: it does not exist yet, `count` and `total` count the ghosts below it.
pub fn ghost_folder(
    volume_id: VolumeId,
    path: &str,
    count: i64,
    total: i64,
    matches: Option<i64>,
) -> FolderEntry {
    FolderEntry {
        name: path.rsplit('/').next().unwrap().to_owned(),
        path: path.to_owned(),
        volume_id,
        count,
        total,
        matches,
        preview: None,
        overlay: Overlay {
            planned: true,
            ..Overlay::default()
        },
    }
}

/// A folder overlay with no data about real folders and no ghosts.
pub fn empty_overlay() -> FolderOverlay {
    FolderOverlay {
        per_child: HashMap::new(),
        ghost_folders: Vec::new(),
        ghosts: FooterCounts::default(),
        dir_planned: false,
    }
}

pub fn child(
    matches: Option<i64>,
    overlay: Overlay,
    ghost_count: i64,
    ghost_total: i64,
) -> FolderPlanInfo {
    FolderPlanInfo {
        matches,
        overlay,
        ghost_count,
        ghost_total,
    }
}
