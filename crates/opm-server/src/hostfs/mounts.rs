//! Mount-table accessors used by the host filesystem probes.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use opm_core::mounts::{MountInfo, lookup, parse_mountinfo};

pub fn for_path(input: &[u8], path: &[u8]) -> Option<MountInfo> {
    lookup(&parse_mountinfo(input), path).cloned()
}

pub fn filesystem_type(input: &[u8], path: &Path) -> Option<String> {
    for_path(input, path.as_os_str().as_bytes()).map(|mount| mount.fs_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_longest_mount_point() {
        let input = b"1 0 0:1 / /volumes rw - ext4 /dev/root rw\n\
                     2 1 0:2 / /volumes/windows rw - 9p D: rw";
        assert_eq!(
            filesystem_type(input, Path::new("/volumes/windows/photos")).as_deref(),
            Some("9p")
        );
    }
}
