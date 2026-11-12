//! Test seams that simulate a crash or a failure at a precise step of an operation.
//!
//! Production code never installs failpoints (`FsOps::new` has none); the crash tests do, through
//! [`FsOps::with_failpoints`](super::FsOps::with_failpoints). A hook is asked at every [`Step`]
//! and answers with a [`FailAction`].

use std::fmt;
use std::sync::Arc;

use super::error::FsOpError;

/// A point of an operation where a crash can be injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    /// The journal row exists, nothing on disk changed yet.
    AfterJournal,
    /// The `.opm-part` file was created in the destination directory.
    AfterPartCreate,
    /// All the bytes were copied into the part file.
    AfterCopy,
    /// The part file was flushed to the disk.
    AfterFsync,
    /// The part file's content was read back and matched the source.
    AfterVerify,
    /// The destination (or the trash entry) is in its final place. The source is gone for a
    /// rename, still there for a copy.
    AfterFinalRename,
    /// A copy-and-delete move is about to retire its source.
    BeforeUnlink,
    /// Disk work is finished; the index transaction is about to begin.
    BeforeIndexTx,
}

/// What a failpoint does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailAction {
    /// Nothing.
    Continue,
    /// The step fails with an ordinary i/o error: the operation cleans up as it does for a real
    /// failure.
    Error,
    /// A simulated crash: the operation returns [`FsOpError::Aborted`] and cleans NOTHING up, so
    /// the test sees exactly what a killed process would leave behind.
    Abort,
    /// Only meaningful at [`Step::AfterCopy`]: flips a byte of the part file, so verification
    /// must fail. Elsewhere it acts as `Continue`.
    Corrupt,
}

/// The failpoint hook: asked at every [`Step`].
#[derive(Clone)]
pub struct Failpoints(Arc<dyn Fn(Step) -> FailAction + Send + Sync>);

impl Failpoints {
    pub fn new(hook: impl Fn(Step) -> FailAction + Send + Sync + 'static) -> Self {
        Self(Arc::new(hook))
    }

    /// Aborts (simulated crash) the first time `step` is reached, then never again.
    pub fn abort_at(step: Step) -> Self {
        let fired = std::sync::atomic::AtomicBool::new(false);
        Self::new(move |reached| {
            if reached == step && !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                FailAction::Abort
            } else {
                FailAction::Continue
            }
        })
    }

    pub(crate) fn fire(&self, step: Step) -> FailAction {
        (self.0)(step)
    }

    /// Fires `step` and turns the action into the result the operation propagates.
    pub(crate) fn check(&self, step: Step) -> Result<(), FsOpError> {
        match self.fire(step) {
            FailAction::Continue | FailAction::Corrupt => Ok(()),
            FailAction::Error => Err(FsOpError::Io(std::io::ErrorKind::Other)),
            FailAction::Abort => Err(FsOpError::Aborted),
        }
    }
}

impl fmt::Debug for Failpoints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Failpoints(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_at_fires_once() {
        let points = Failpoints::abort_at(Step::AfterCopy);
        assert_eq!(points.check(Step::AfterJournal), Ok(()));
        assert_eq!(points.check(Step::AfterCopy), Err(FsOpError::Aborted));
        assert_eq!(points.check(Step::AfterCopy), Ok(()));
    }

    #[test]
    fn error_is_an_ordinary_io_failure() {
        let points = Failpoints::new(|_| FailAction::Error);
        assert_eq!(
            points.check(Step::AfterJournal),
            Err(FsOpError::Io(std::io::ErrorKind::Other))
        );
    }
}
