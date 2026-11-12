//! Bounded probes performed during volume discovery, never in an API handler.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::DirEntryExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use opm_core::mounts;

use super::mount_type;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeCapabilities {
    pub fs_type: Option<String>,
    pub case_insensitive: Option<bool>,
    pub inode_reliable: Option<bool>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeProbe {
    pub writable: bool,
    pub capabilities: VolumeCapabilities,
}

pub fn probe_volume(root: &Path) -> VolumeProbe {
    probe(root, true)
}

pub fn probe_writable(root: &Path) -> bool {
    probe(root, false).writable
}

fn probe(root: &Path, inspect: bool) -> VolumeProbe {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut capabilities = VolumeCapabilities {
        fs_type: inspect.then(|| mount_type(root)).flatten(),
        ..VolumeCapabilities::default()
    };
    for _ in 0..8 {
        let name = format!(
            ".opm-probe-ABC-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = root.join(&name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                if !inspect {
                    drop(file);
                    return VolumeProbe {
                        writable: remove_probe(&path),
                        capabilities,
                    };
                }
                let first = file.metadata().ok();
                let second = fs::symlink_metadata(&path).ok();
                capabilities.case_insensitive =
                    match fs::symlink_metadata(root.join(name.to_ascii_lowercase())) {
                        Ok(_) => Some(true),
                        Err(error) if error.kind() == ErrorKind::NotFound => Some(false),
                        Err(_) => None,
                    };
                let entry_inode = fs::read_dir(root)
                    .ok()
                    .and_then(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .find(|entry| entry.file_name().to_string_lossy() == name)
                    })
                    .map(|entry| entry.ino());
                capabilities.inode_reliable = match (first, second, entry_inode) {
                    (Some(first), Some(second), Some(entry_inode)) => {
                        let first_ino = first.ino();
                        let second_ino = second.ino();
                        let reliable = first.dev() != 0
                            && second.dev() != 0
                            && first.dev() == second.dev()
                            && first_ino == second_ino
                            && first_ino != 0
                            && second_ino != 0
                            && entry_inode == first_ino;
                        if reliable { Some(true) } else { Some(false) }
                    }
                    _ => None,
                };
                drop(file);
                let writable = remove_probe(&path);
                if capabilities.case_insensitive.is_none() {
                    capabilities
                        .notes
                        .push("case sensitivity could not be determined".into());
                }
                if capabilities.inode_reliable == Some(false) {
                    capabilities
                        .notes
                        .push("inode identity is unreliable on this mount".into());
                }
                return VolumeProbe {
                    writable,
                    capabilities,
                };
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => {
                return VolumeProbe {
                    writable: false,
                    capabilities,
                };
            }
        }
    }
    VolumeProbe {
        writable: false,
        capabilities,
    }
}

fn remove_probe(path: &Path) -> bool {
    match fs::remove_file(path) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "created a probe file but could not remove it"
            );
            false
        }
    }
}

pub(crate) fn wal_unsafe(fs_type: Option<&str>) -> bool {
    fs_type.is_some_and(mounts::is_wal_unsafe)
}
