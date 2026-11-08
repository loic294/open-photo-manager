//! Pure parsing and classification for Linux `/proc/self/mountinfo`.
//!
//! Paths and sources stay as bytes because mountinfo may describe non-UTF-8 names.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    pub mount_point: Vec<u8>,
    pub fs_type: String,
    pub source: Vec<u8>,
}

pub fn parse_mountinfo(input: &[u8]) -> Vec<MountInfo> {
    input
        .split(|byte| *byte == b'\n')
        .filter_map(parse_line)
        .collect()
}

fn parse_line(line: &[u8]) -> Option<MountInfo> {
    let separator = line.windows(3).position(|part| part == b" - ")?;
    let left = line[..separator]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let right = line[separator + 3..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if left.len() < 6 || right.len() < 3 {
        return None;
    }
    let fs_type = std::str::from_utf8(right[0]).ok()?.to_owned();
    Some(MountInfo {
        mount_point: decode_mount_field(left[4]),
        fs_type,
        source: decode_mount_field(right[1]),
    })
}

fn decode_mount_field(field: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\'
            && index + 3 < field.len()
            && field[index + 1..index + 4]
                .iter()
                .all(|byte| (b'0'..=b'7').contains(byte))
        {
            let value = u16::from(field[index + 1] - b'0') * 64
                + u16::from(field[index + 2] - b'0') * 8
                + u16::from(field[index + 3] - b'0');
            if value <= u16::from(u8::MAX) {
                decoded.push(value as u8);
                index += 4;
            } else {
                decoded.push(field[index]);
                index += 1;
            }
        } else {
            decoded.push(field[index]);
            index += 1;
        }
    }
    decoded
}

pub fn lookup<'a>(entries: &'a [MountInfo], path: &[u8]) -> Option<&'a MountInfo> {
    entries
        .iter()
        .filter(|entry| is_component_prefix(&entry.mount_point, path))
        .max_by_key(|entry| entry.mount_point.len())
}

fn is_component_prefix(prefix: &[u8], path: &[u8]) -> bool {
    let prefix = trim_trailing_slashes(prefix);
    let path = trim_trailing_slashes(path);
    prefix == b"/"
        || path == prefix
        || (path.starts_with(prefix) && path.get(prefix.len()) == Some(&b'/'))
}

fn trim_trailing_slashes(path: &[u8]) -> &[u8] {
    let end = path
        .iter()
        .rposition(|byte| *byte != b'/')
        .map_or(1, |index| index + 1);
    &path[..end]
}

pub fn is_polling_only(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "9p" | "drvfs"
            | "plan9"
            | "cifs"
            | "smb3"
            | "smbfs"
            | "nfs"
            | "nfs4"
            | "virtiofs"
            | "vboxsf"
            | "fuse"
    ) || fs_type.starts_with("fuse.")
}

pub fn is_wal_unsafe(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "9p" | "drvfs" | "plan9" | "cifs" | "smb3" | "smbfs" | "nfs" | "nfs4" | "vboxsf" | "fuse"
    ) || fs_type.starts_with("fuse.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_example_mounts_and_escaped_paths() {
        // Illustrative examples from memory, not observations of Docker Desktop.
        let text = br#"31 24 0:27 / / rw,relatime - overlay overlay rw
42 31 8:1 / /volumes/photos rw,relatime - btrfs /dev/md0 rw
43 31 0:42 / /volumes/windows rw,relatime - 9p D:\134 rw"#;
        let entries = parse_mountinfo(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].fs_type, "overlay");
        assert_eq!(entries[1].fs_type, "btrfs");
        assert_eq!(entries[2].mount_point, b"/volumes/windows");
        assert_eq!(entries[2].source, br"D:\");
    }

    #[test]
    fn decodes_space_tab_newline_backslash_and_non_utf8_bytes() {
        let entries =
            parse_mountinfo(b"1 0 0:1 / /mnt/a\\040b\\011c\\012d\\134\\377 rw - ext4 /dev/x rw");
        assert_eq!(entries[0].mount_point, b"/mnt/a b\tc\nd\\\xff");
    }

    #[test]
    fn skips_malformed_lines_and_preserves_out_of_range_octal_escapes() {
        assert!(parse_mountinfo(b"not mountinfo\n1 2 - ext4").is_empty());
        let entries = parse_mountinfo(b"1 0 0:1 / /mnt/\\777 rw - ext4 /dev/x rw");
        assert_eq!(entries[0].mount_point, b"/mnt/\\777");
    }

    #[test]
    fn lookup_uses_component_prefix_and_last_duplicate_mount() {
        let entries = parse_mountinfo(
            b"1 0 0:1 / /volumes rw - ext4 /dev/root rw\n\
              2 1 0:2 / /volumes/photo rw - ext4 /dev/old rw\n\
              3 1 0:3 / /volumes/photo rw - 9p D: rw",
        );
        assert_eq!(
            lookup(&entries, b"/volumes/photo/2024").map(|entry| entry.fs_type.as_str()),
            Some("9p")
        );
        assert_eq!(
            lookup(&entries, b"/volumes/photos").map(|entry| entry.fs_type.as_str()),
            Some("ext4")
        );
    }

    #[test]
    fn event_and_wal_classification_are_intentionally_different() {
        assert!(is_polling_only("virtiofs"));
        assert!(!is_wal_unsafe("virtiofs"));
        assert!(is_polling_only("fuse.grpcfuse"));
        assert!(is_polling_only("fuse.osxfs"));
        assert!(!is_polling_only("fuseblk"));
        assert!(!is_polling_only("ext4"));
    }

    proptest! {
        #[test]
        fn arbitrary_mountinfo_never_panics(input in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let entries = parse_mountinfo(&input);
            let _ = lookup(&entries, b"/a/b");
        }
    }
}
