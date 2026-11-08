//! Runtime filesystem capabilities for mounted volumes and the SQLite data directory.
//!
//! The mount table is advisory: an unavailable table or an unknown filesystem type means the
//! existing native behaviour. See `mounts` for the injected, byte-preserving parser.

mod mounts;
pub mod probe;
pub mod warnings;

use std::fs;
use std::path::Path;

use apm_core::dto::WatchMode;

use crate::config::WatchPolicy;

pub use apm_core::mounts::MountInfo;
pub use probe::{VolumeCapabilities, VolumeProbe};

pub(crate) fn mount_type(path: &Path) -> Option<String> {
    let text = fs::read("/proc/self/mountinfo").ok()?;
    mounts::filesystem_type(&text, path)
}

pub(crate) fn effective_watch_mode(
    policy: WatchPolicy,
    fs_type: Option<&str>,
    enabled: bool,
) -> (WatchMode, Option<String>) {
    if !enabled {
        return (WatchMode::Watching, None);
    }
    match policy {
        WatchPolicy::Poll => (
            WatchMode::PollingOnly,
            Some("filesystem watching is disabled by APM_WATCH_MODE=poll".to_owned()),
        ),
        WatchPolicy::Watch => (
            WatchMode::Watching,
            Some("filesystem watching is forced by APM_WATCH_MODE=watch".to_owned()),
        ),
        WatchPolicy::Auto if fs_type.is_some_and(apm_core::mounts::is_polling_only) => {
            let fs_type = fs_type.unwrap_or_default();
            (
                WatchMode::PollingOnly,
                Some(format!(
                    "filesystem type {fs_type} does not deliver reliable change events"
                )),
            )
        }
        WatchPolicy::Auto => (WatchMode::Watching, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_policy_precedence_and_reasons_are_stable() {
        assert_eq!(
            effective_watch_mode(WatchPolicy::Poll, Some("ext4"), true).0,
            WatchMode::PollingOnly
        );
        assert_eq!(
            effective_watch_mode(WatchPolicy::Watch, Some("9p"), true).0,
            WatchMode::Watching
        );
        assert_eq!(
            effective_watch_mode(WatchPolicy::Auto, Some("9p"), true).0,
            WatchMode::PollingOnly
        );
        assert_eq!(
            effective_watch_mode(WatchPolicy::Auto, None, true).0,
            WatchMode::Watching
        );
        assert_eq!(
            effective_watch_mode(WatchPolicy::Poll, Some("9p"), false),
            (WatchMode::Watching, None)
        );
    }
}
