//! The wake-up line between the hook and the scheduler.
//!
//! The hook (`hook`) must exist before the file operations do, and the scheduler (`service`) needs
//! services that are built after them, so the two meet through this small shared value, created
//! first and handed to both.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tokio::sync::Notify;

use crate::db::now_secs;

/// Cheap to clone; every clone is the same line.
#[derive(Clone, Default)]
pub struct OutboxSignal {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    notify: Notify,
    /// Epoch seconds of the latest enqueue; 0 before the first.
    last_enqueue: AtomicI64,
}

impl OutboxSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called by the hook after it queued a change: stamps the moment (the scheduler lets a burst
    /// of operations pass before it starts) and wakes the scheduler.
    ///
    /// The hook calls it BEFORE its transaction commits. That is harmless: the scheduler sleeps
    /// until the quiet window after this stamp has passed, which is long after the commit.
    pub(super) fn enqueued(&self) {
        self.inner.last_enqueue.store(now_secs(), Ordering::Relaxed);
        self.inner.notify.notify_one();
    }

    /// Wakes the scheduler without a new change (a retry was requested, the connection changed).
    pub(super) fn wake(&self) {
        self.inner.notify.notify_one();
    }

    /// The moment of the latest enqueue, `None` before the first.
    pub(super) fn last_enqueue(&self) -> Option<i64> {
        let stamp = self.inner.last_enqueue.load(Ordering::Relaxed);
        (stamp > 0).then_some(stamp)
    }

    /// Resolves when the line was woken since the last call (a wake-up that arrives while nobody
    /// waits is kept, so none is lost).
    pub(super) async fn woken(&self) {
        self.inner.notify.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn a_wake_up_before_the_wait_is_not_lost() {
        let signal = OutboxSignal::new();
        assert_eq!(signal.last_enqueue(), None);
        signal.enqueued();
        assert!(signal.last_enqueue().is_some());
        tokio::time::timeout(Duration::from_secs(5), signal.woken())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn clones_share_the_line() {
        let signal = OutboxSignal::new();
        let other = signal.clone();
        other.wake();
        tokio::time::timeout(Duration::from_secs(5), signal.woken())
            .await
            .unwrap();
        assert!(
            signal.last_enqueue().is_none(),
            "a plain wake stamps nothing"
        );
        other.enqueued();
        assert_eq!(signal.last_enqueue(), other.last_enqueue());
    }
}
