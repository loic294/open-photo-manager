//! The error type of the file operations.
//!
//! A failure of one file is a RETURN VALUE: callers store `code()` per item and go on with the
//! next file. Messages never carry an absolute host path (the [`security`](crate::security) error
//! types only hold volume-relative paths and the codes below hold none).

use std::io;

use axum::http::StatusCode;
use thiserror::Error;

use crate::db::DbError;
use crate::error::AppError;
use crate::security::FsError;

/// Why a file operation failed. [`FsOpError::code`] is the stable string callers store.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FsOpError {
    /// The source (or the trash entry) is not there.
    #[error("the source file does not exist")]
    SourceMissing,
    /// The source differs from what the caller expected (`Expect`) or changed while it was copied.
    /// Nothing was changed.
    #[error("the source file changed since the operation was planned")]
    SourceChanged,
    /// The path is a directory, a special file or something else than a regular file.
    #[error("not a regular file")]
    NotARegularFile,
    /// A symbolic link is the path or a component of it. Links are never followed.
    #[error("a symbolic link is in the way")]
    SymlinkRejected,
    /// The volume is known but its root is missing or unreadable. Retry when it is back.
    #[error("the volume is offline")]
    VolumeOffline,
    /// No volume has that id.
    #[error("the volume does not exist")]
    UnknownVolume,
    /// The volume is mounted read-only (every mutation is refused).
    #[error("the volume is read-only")]
    VolumeReadOnly,
    /// The location is not allowed: the volume root, the trash, a directory or link where a
    /// regular file must go, a name the scanner would ignore.
    #[error("invalid location: {0}")]
    DestinationInvalid(&'static str),
    /// A name component exceeds `name_max_len` bytes.
    #[error("the file name is too long")]
    NameTooLong,
    /// The destination does not have room for the file (checked before writing, and on `ENOSPC`).
    #[error("not enough free space on the destination")]
    NoSpace,
    /// The copy on the destination does not match the source (size or SHA-1). The destination
    /// copy is deleted and the source is untouched.
    #[error("the copy could not be verified")]
    VerifyFailed,
    /// The context was cancelled. The file the operation was on is finished or rolled back.
    #[error("the operation was cancelled")]
    Cancelled,
    /// The directory holds something that is not junk.
    #[error("the directory is not empty")]
    NotEmpty,
    /// `purge` was asked to delete something that is not inside the trash of its volume.
    #[error("the path is not inside the trash")]
    OutsideTrash,
    /// Any other i/o failure.
    #[error("i/o error: {0}")]
    Io(io::ErrorKind),
    /// The index database failed, or a hook did, or a worker thread was lost. The disk operation
    /// may have happened: the journal row stays `pending` and `reconcile` finishes it.
    #[error("internal error: {0}")]
    Internal(String),
    /// A test failpoint simulated a crash (see [`Step`](super::Step)). Never produced in
    /// production.
    #[error("stopped by a failpoint")]
    Aborted,
}

impl FsOpError {
    /// The stable code of the error, one per variant.
    pub fn code(&self) -> &'static str {
        match self {
            Self::SourceMissing => "source_missing",
            Self::SourceChanged => "source_changed",
            Self::NotARegularFile => "not_a_regular_file",
            Self::SymlinkRejected => "symlink_rejected",
            Self::VolumeOffline => "volume_offline",
            Self::UnknownVolume => "unknown_volume",
            Self::VolumeReadOnly => "volume_read_only",
            Self::DestinationInvalid(_) => "destination_invalid",
            Self::NameTooLong => "name_too_long",
            Self::NoSpace => "no_space",
            Self::VerifyFailed => "verify_failed",
            Self::Cancelled => "cancelled",
            Self::NotEmpty => "not_empty",
            Self::OutsideTrash => "outside_trash",
            Self::Io(_) => "io_error",
            Self::Internal(_) => "internal",
            Self::Aborted => "aborted",
        }
    }

    /// Converts a failure on the SOURCE side of an operation: a missing component, or one that is
    /// not a directory, means the source is not there.
    pub(crate) fn from_source(err: FsError) -> Self {
        match err {
            FsError::NotFound(_) | FsError::NotADirectory(_) => Self::SourceMissing,
            FsError::NotAFile(_) => Self::NotARegularFile,
            other => Self::from(other),
        }
    }

    /// Converts a failure on the DESTINATION side: the parent chain is created beforehand, so a
    /// missing or non-directory component means the location cannot be used.
    pub(crate) fn from_destination(err: FsError) -> Self {
        match err {
            FsError::NotFound(_) => Self::DestinationInvalid("a parent directory is missing"),
            FsError::NotADirectory(_) => {
                Self::DestinationInvalid("a parent of the destination is not a directory")
            }
            FsError::NotAFile(_) => Self::DestinationInvalid("a directory is in the way"),
            FsError::AlreadyExists(_) => Self::DestinationInvalid("the destination exists"),
            other => Self::from(other),
        }
    }

    pub(crate) fn internal(cause: impl std::fmt::Display) -> Self {
        Self::Internal(cause.to_string())
    }

    /// Whether the error is a simulated crash: cleanup must be skipped for it.
    pub(crate) fn is_abort(&self) -> bool {
        matches!(self, Self::Aborted)
    }
}

impl From<FsError> for FsOpError {
    fn from(err: FsError) -> Self {
        match err {
            FsError::NotFound(_) => Self::SourceMissing,
            FsError::SymlinkRejected(_) => Self::SymlinkRejected,
            FsError::NotADirectory(_) => Self::DestinationInvalid("a path component is not a directory"),
            FsError::NotAFile(_) => Self::NotARegularFile,
            FsError::AlreadyExists(_) => Self::DestinationInvalid("the destination exists"),
            FsError::OutsideRoot => Self::DestinationInvalid("the path is outside the volume"),
            FsError::InvalidFilename(_) => Self::NameTooLong,
            FsError::Io(kind) => Self::from(io::Error::from(kind)),
        }
    }
}

impl From<io::Error> for FsOpError {
    fn from(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => Self::NoSpace,
            kind => Self::Io(kind),
        }
    }
}

impl From<sqlx::Error> for FsOpError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.to_string())
    }
}

impl From<DbError> for FsOpError {
    fn from(err: DbError) -> Self {
        Self::Internal(err.to_string())
    }
}

impl From<FsOpError> for AppError {
    fn from(err: FsOpError) -> Self {
        let code = err.code();
        let message = err.to_string();
        match err {
            FsOpError::SourceMissing | FsOpError::UnknownVolume => {
                Self::new(StatusCode::NOT_FOUND, code, message)
            }
            FsOpError::NotARegularFile
            | FsOpError::DestinationInvalid(_)
            | FsOpError::NameTooLong => Self::new(StatusCode::BAD_REQUEST, code, message),
            FsOpError::SymlinkRejected | FsOpError::OutsideTrash => {
                Self::new(StatusCode::FORBIDDEN, code, message)
            }
            FsOpError::SourceChanged
            | FsOpError::VolumeOffline
            | FsOpError::VolumeReadOnly
            | FsOpError::Cancelled
            | FsOpError::NotEmpty => Self::new(StatusCode::CONFLICT, code, message),
            FsOpError::NoSpace => Self::new(StatusCode::INSUFFICIENT_STORAGE, code, message),
            FsOpError::VerifyFailed | FsOpError::Io(_) | FsOpError::Aborted => {
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
            }
            FsOpError::Internal(cause) => Self::internal(cause),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> apm_core::paths::RelPath {
        apm_core::paths::RelPath::new(path).unwrap()
    }

    #[test]
    fn every_variant_has_its_own_stable_code() {
        let all = [
            FsOpError::SourceMissing,
            FsOpError::SourceChanged,
            FsOpError::NotARegularFile,
            FsOpError::SymlinkRejected,
            FsOpError::VolumeOffline,
            FsOpError::UnknownVolume,
            FsOpError::VolumeReadOnly,
            FsOpError::DestinationInvalid("x"),
            FsOpError::NameTooLong,
            FsOpError::NoSpace,
            FsOpError::VerifyFailed,
            FsOpError::Cancelled,
            FsOpError::NotEmpty,
            FsOpError::OutsideTrash,
            FsOpError::Io(io::ErrorKind::Other),
            FsOpError::Internal("x".into()),
            FsOpError::Aborted,
        ];
        let codes: std::collections::BTreeSet<_> = all.iter().map(FsOpError::code).collect();
        assert_eq!(codes.len(), all.len());
        assert!(codes.contains("source_missing") && codes.contains("io_error"));
    }

    #[test]
    fn a_full_disk_is_no_space_and_other_kinds_are_io_errors() {
        assert_eq!(
            FsOpError::from(io::Error::from(io::ErrorKind::StorageFull)),
            FsOpError::NoSpace
        );
        assert_eq!(
            FsOpError::from(io::Error::from(io::ErrorKind::PermissionDenied)),
            FsOpError::Io(io::ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn source_and_destination_sides_read_missing_components_differently() {
        let missing = FsError::NotFound(rel("a/b"));
        assert_eq!(
            FsOpError::from_source(missing.clone()),
            FsOpError::SourceMissing
        );
        assert_eq!(
            FsOpError::from_destination(missing).code(),
            "destination_invalid"
        );
        assert_eq!(
            FsOpError::from_source(FsError::SymlinkRejected(rel("a"))),
            FsOpError::SymlinkRejected
        );
        assert_eq!(
            FsOpError::from_destination(FsError::InvalidFilename(rel("a"))),
            FsOpError::NameTooLong
        );
    }

    #[test]
    fn app_errors_carry_the_code_and_no_host_path() {
        let app: AppError = FsOpError::NoSpace.into();
        assert_eq!(app.status(), StatusCode::INSUFFICIENT_STORAGE);
        assert_eq!(app.code(), "no_space");
        let app: AppError = FsOpError::SymlinkRejected.into();
        assert_eq!(app.status(), StatusCode::FORBIDDEN);
        let app: AppError = FsOpError::Internal("/host/secret".into()).into();
        assert_eq!(app.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!app.message().contains("secret"));
    }
}
