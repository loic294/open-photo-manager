//! The single native watcher and its non-blocking, bounded callback boundary.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

pub const EVENT_CAPACITY: usize = 8192;

/// Callback ingress shared with test backends. Loss requests a complete incremental repair.
#[derive(Clone)]
pub struct EventSender {
    pub(crate) sender: mpsc::Sender<notify::Result<Event>>,
    pub(crate) overflowed: Arc<AtomicBool>,
}

impl EventSender {
    pub fn send(&self, event: notify::Result<Event>) {
        match &event {
            Ok(event) if event.paths.len() > 64 => {
                self.overflowed.store(true, Ordering::Release);
            }
            _ => {
                if self.sender.try_send(event).is_err() {
                    self.overflowed.store(true, Ordering::Release);
                }
            }
        }
    }
}

/// Registration seam: all methods run on the supervisor, never in the callback.
pub trait WatchBackend: Send {
    fn watch_dir(&mut self, path: &Path) -> notify::Result<()>;
    fn watch_tree(&mut self, path: &Path) -> notify::Result<()>;
    fn unwatch(&mut self, path: &Path) -> notify::Result<()>;
}

pub struct NativeBackend(RecommendedWatcher);

impl NativeBackend {
    pub fn new(sender: EventSender) -> notify::Result<Self> {
        notify::recommended_watcher(move |event| sender.send(event)).map(Self)
    }
}

impl WatchBackend for NativeBackend {
    fn watch_dir(&mut self, path: &Path) -> notify::Result<()> {
        self.0.watch(path, RecursiveMode::NonRecursive)
    }
    fn watch_tree(&mut self, path: &Path) -> notify::Result<()> {
        self.0.watch(path, RecursiveMode::Recursive)
    }
    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        self.0.unwatch(path)
    }
}

pub(crate) fn is_limit(error: &notify::Error) -> bool {
    matches!(error.kind, notify::ErrorKind::MaxFilesWatch)
        || matches!(&error.kind, notify::ErrorKind::Io(e) if e.raw_os_error() == Some(28))
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;

    #[test]
    fn full_queue_never_blocks_and_reports_loss_once_until_drained() {
        let (sender, mut receiver) = mpsc::channel(2);
        let overflowed = Arc::new(AtomicBool::new(false));
        let ingress = EventSender {
            sender,
            overflowed: overflowed.clone(),
        };
        ingress.send(Ok(Event::new(EventKind::Other)));
        ingress.send(Ok(Event::new(EventKind::Other)));
        assert!(!overflowed.load(Ordering::Acquire));
        ingress.send(Ok(Event::new(EventKind::Other)));
        assert!(overflowed.swap(false, Ordering::AcqRel));
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_ok());
        ingress.send(Err(notify::Error::generic("lost")));
        assert!(matches!(receiver.try_recv(), Ok(Err(_))));
        assert!(!overflowed.load(Ordering::Acquire));
    }

    #[test]
    fn linux_budget_errors_are_recognized_without_treating_other_errors_as_limits() {
        assert!(is_limit(&notify::Error::new(
            notify::ErrorKind::MaxFilesWatch
        )));
        assert!(is_limit(&notify::Error::io(
            std::io::Error::from_raw_os_error(28)
        )));
        assert!(!is_limit(&notify::Error::generic("unavailable")));
    }
}
