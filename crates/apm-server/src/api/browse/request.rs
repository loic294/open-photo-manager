//! What the four browse endpoints share: turning the `volume_id` and `dir` of a request into a
//! validated [`Place`], the page limit, and the checks against the index.

use apm_core::dto::DEFAULT_PAGE_SIZE;
use apm_core::paths::{RelPath, VolumeId};

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::query::MAX_PAGE_SIZE;
use crate::query::volumes::volume_exists;

/// Where a request points once its `dir` is known to be well formed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Place {
    /// The virtual top level of the file system view: the volumes are its folders.
    VolumesRoot,
    /// A directory of a volume. Neither has been looked up in the index yet.
    Dir { volume_id: VolumeId, dir: RelPath },
}

impl Place {
    /// Checks `volume_id` and `dir` against each other and `dir` against the path rules. Every
    /// failure is a 400 that names the field; nothing is read from the database.
    pub(super) fn parse(volume_id: Option<VolumeId>, dir: &str) -> AppResult<Self> {
        let dir = parse_dir(dir)?;
        match volume_id {
            Some(volume_id) => Ok(Self::Dir { volume_id, dir }),
            None if dir.is_root() => Ok(Self::VolumesRoot),
            None => Err(AppError::bad_request(
                "bad_request",
                "dir must be empty unless volume_id is set",
            )),
        }
    }
}

/// `dir` as the index stores it: relative to the volume root, `/`-separated, without empty or `.`
/// segments, `..`, NUL bytes or backslashes. The accepted text is exactly what [`RelPath`] prints,
/// so what a client sends is what is compared with the `dirs` table.
fn parse_dir(raw: &str) -> AppResult<RelPath> {
    let invalid = |why: &str| AppError::bad_request("bad_request", format!("invalid dir: {why}"));
    if raw.contains('\\') {
        return Err(invalid("backslashes are not allowed"));
    }
    let dir = RelPath::new(raw).map_err(|err| invalid(&err.to_string()))?;
    if dir.as_str() != raw {
        return Err(invalid("empty and `.` segments are not allowed"));
    }
    Ok(dir)
}

/// The page size to ask the engine for: 0 means [`DEFAULT_PAGE_SIZE`] and anything above
/// [`MAX_PAGE_SIZE`] is clamped to it.
pub(super) fn page_limit(limit: u32) -> i64 {
    match limit {
        0 => i64::from(DEFAULT_PAGE_SIZE),
        limit => i64::from(limit).min(MAX_PAGE_SIZE),
    }
}

/// 404 `not_found` unless the index knows the volume. An offline volume is fine: browsing only
/// reads the index.
pub(super) async fn require_volume(db: &Db, volume_id: VolumeId) -> AppResult<()> {
    if volume_exists(db, volume_id).await? {
        Ok(())
    } else {
        Err(AppError::not_found(
            "not_found",
            format!("volume {volume_id} not found"),
        ))
    }
}

/// 404 `folder_not_found`: the directory is not in the index and no plan creates it. The client
/// walks up to the nearest folder that still exists.
pub(super) fn folder_not_found(volume_id: VolumeId, dir: &RelPath) -> AppError {
    AppError::not_found(
        "folder_not_found",
        format!("folder `{}` not found in volume {volume_id}", dir.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(volume_id: Option<VolumeId>, dir: &str) -> String {
        let err = Place::parse(volume_id, dir).expect_err("must be rejected");
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "bad_request");
        err.message().to_owned()
    }

    #[test]
    fn a_volume_and_a_clean_dir_are_accepted() {
        for dir in [
            "",
            "2024",
            "2024/Trip",
            "a b/ç",
            "..x",
            "x..",
            "a.b/.hidden",
        ] {
            let place = Place::parse(Some(3), dir).expect(dir);
            assert_eq!(
                place,
                Place::Dir {
                    volume_id: 3,
                    dir: RelPath::new(dir).expect(dir)
                }
            );
        }
    }

    #[test]
    fn no_volume_means_the_volumes_root() {
        assert_eq!(
            Place::parse(None, "").expect("the root"),
            Place::VolumesRoot
        );
    }

    #[test]
    fn no_volume_with_a_dir_is_rejected() {
        assert!(rejected(None, "2024").starts_with("dir must be empty"));
    }

    #[test]
    fn malformed_dirs_are_rejected_naming_the_field() {
        for dir in [
            "..", "a/../b", "../a", "/abs", "a\\b", "\\", "a\0b", "a//b", "a/", "./a", "a/./b",
            "//", "/", ".",
        ] {
            for volume_id in [None, Some(1)] {
                assert!(
                    rejected(volume_id, dir).starts_with("invalid dir: "),
                    "{dir:?}"
                );
            }
        }
    }

    #[test]
    fn the_page_limit_defaults_and_clamps() {
        assert_eq!(page_limit(0), 200);
        assert_eq!(page_limit(1), 1);
        assert_eq!(page_limit(200), 200);
        assert_eq!(page_limit(500), 500);
        assert_eq!(page_limit(501), 500);
        assert_eq!(page_limit(u32::MAX), 500);
    }
}
