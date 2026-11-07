//! The in-process event bus behind `GET /api/events`, and the coalescing of file-system changes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use apm_core::dto::ServerEvent;
use apm_core::paths::VolumeId;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

/// Events a subscriber may fall behind by before its stream is closed.
pub(super) const CAPACITY: usize = 1024;

/// A `fs_change` event lists at most this many directories; a bigger batch is split in several.
pub(super) const MAX_DIRS_PER_EVENT: usize = 500;

/// Fan-out of [`ServerEvent`]s to every connected stream. There is no history: whoever subscribes
/// sees what is published afterwards.
#[derive(Clone, Debug)]
pub(super) struct EventBus {
    sender: broadcast::Sender<ServerEvent>,
}

impl EventBus {
    pub(super) fn new() -> Self {
        let (sender, _) = broadcast::channel(CAPACITY);
        Self { sender }
    }

    pub(super) fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.sender.subscribe()
    }

    /// Sends `event` to every subscriber; nobody listening is not an error.
    pub(super) fn publish(&self, event: ServerEvent) {
        drop(self.sender.send(event));
    }
}

/// Directories reported as changed, waiting to be sent as one event per volume. Reporting is a
/// plain function call (no runtime needed); [`FsChanges::run`] turns what piled up into events.
#[derive(Default)]
pub(super) struct FsChanges {
    pending: Mutex<BTreeMap<VolumeId, BTreeSet<String>>>,
    wake: Notify,
}

impl FsChanges {
    pub(super) fn push(&self, volume_id: VolumeId, dirs: impl IntoIterator<Item = String>) {
        let dirs: Vec<String> = dirs.into_iter().collect();
        if dirs.is_empty() {
            return;
        }
        self.lock().entry(volume_id).or_default().extend(dirs);
        // A permit is stored when the flusher is not waiting yet, so the wake-up is never lost.
        self.wake.notify_one();
    }

    /// Publishes everything pending: one event per volume, directories sorted and without
    /// duplicates, split in chunks of [`MAX_DIRS_PER_EVENT`].
    pub(super) fn flush(&self, bus: &EventBus) {
        let pending = std::mem::take(&mut *self.lock());
        for (volume_id, dirs) in pending {
            let dirs: Vec<String> = dirs.into_iter().collect();
            for chunk in dirs.chunks(MAX_DIRS_PER_EVENT) {
                bus.publish(ServerEvent::FsChange {
                    volume_id,
                    dirs: chunk.to_vec(),
                });
            }
        }
    }

    /// Flushes `window` after the first change of a burst, so that a scan touching thousands of
    /// directories produces a handful of events. Returns after a last flush once `stop` fires.
    pub(super) async fn run(&self, bus: &EventBus, window: Duration, stop: &CancellationToken) {
        loop {
            tokio::select! {
                () = self.wake.notified() => {}
                () = stop.cancelled() => break,
            }
            tokio::select! {
                () = tokio::time::sleep(window) => {}
                () = stop.cancelled() => break,
            }
            self.flush(bus);
        }
        self.flush(bus);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<VolumeId, BTreeSet<String>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::broadcast::error::TryRecvError;

    use super::*;

    fn dirs(events: &[ServerEvent]) -> Vec<(VolumeId, Vec<String>)> {
        events
            .iter()
            .map(|event| match event {
                ServerEvent::FsChange { volume_id, dirs } => (*volume_id, dirs.clone()),
                other => panic!("unexpected event {other:?}"),
            })
            .collect()
    }

    fn drain(rx: &mut broadcast::Receiver<ServerEvent>) -> Vec<ServerEvent> {
        let mut events = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty | TryRecvError::Closed) => return events,
                Err(TryRecvError::Lagged(missed)) => panic!("lagged by {missed}"),
            }
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn the_bus_delivers_to_every_subscriber_and_keeps_no_history() {
        let bus = EventBus::new();
        bus.publish(ServerEvent::FsChange {
            volume_id: 1,
            dirs: strings(&["before"]),
        });
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();
        bus.publish(ServerEvent::FsChange {
            volume_id: 1,
            dirs: strings(&["after"]),
        });
        for rx in [&mut first, &mut second] {
            assert_eq!(dirs(&drain(rx)), [(1, strings(&["after"]))]);
        }
    }

    #[test]
    fn a_subscriber_that_falls_behind_lags() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        for n in 0..=CAPACITY {
            bus.publish(ServerEvent::FsChange {
                volume_id: 1,
                dirs: vec![n.to_string()],
            });
        }
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Lagged(_))));
    }

    #[test]
    fn a_flush_merges_dedupes_and_sorts_per_volume() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let changes = FsChanges::default();
        changes.push(2, strings(&["b", "a"]));
        changes.push(1, strings(&["z"]));
        changes.push(2, strings(&["a", "c"]));
        changes.push(1, strings(&["z", "y"]));
        changes.flush(&bus);
        assert_eq!(
            dirs(&drain(&mut rx)),
            [(1, strings(&["y", "z"])), (2, strings(&["a", "b", "c"]))]
        );
        changes.flush(&bus);
        assert!(drain(&mut rx).is_empty(), "nothing is sent twice");
    }

    #[test]
    fn nothing_is_published_for_an_empty_report() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let changes = FsChanges::default();
        changes.push(1, Vec::new());
        changes.flush(&bus);
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn a_big_batch_is_split_in_chunks() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let changes = FsChanges::default();
        let total = MAX_DIRS_PER_EVENT * 2 + 1;
        changes.push(7, (0..total).map(|n| format!("dir/{n:05}")));
        changes.flush(&bus);

        let events = dirs(&drain(&mut rx));
        let sizes: Vec<usize> = events.iter().map(|(_, dirs)| dirs.len()).collect();
        assert_eq!(sizes, [MAX_DIRS_PER_EVENT, MAX_DIRS_PER_EVENT, 1]);
        let joined: Vec<String> = events.into_iter().flat_map(|(_, dirs)| dirs).collect();
        assert_eq!(joined.len(), total);
        assert!(joined.windows(2).all(|pair| pair[0] < pair[1]), "sorted");
    }

    #[tokio::test]
    async fn the_flusher_coalesces_a_burst_into_one_event_per_volume() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let changes = Arc::new(FsChanges::default());
        let stop = CancellationToken::new();
        let task = tokio::spawn({
            let (changes, bus, stop) = (Arc::clone(&changes), bus.clone(), stop.clone());
            async move { changes.run(&bus, Duration::from_millis(60), &stop).await }
        });

        changes.push(1, strings(&["a"]));
        changes.push(1, strings(&["b", "a"]));
        changes.push(2, strings(&["x"]));
        tokio::time::sleep(Duration::from_millis(400)).await;

        assert_eq!(
            dirs(&drain(&mut rx)),
            [(1, strings(&["a", "b"])), (2, strings(&["x"]))]
        );

        changes.push(1, strings(&["c"]));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(dirs(&drain(&mut rx)), [(1, strings(&["c"]))]);

        stop.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn changes_reported_before_the_flusher_starts_are_not_lost() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let changes = Arc::new(FsChanges::default());
        changes.push(3, strings(&["early"]));

        let stop = CancellationToken::new();
        let task = tokio::spawn({
            let (changes, bus, stop) = (Arc::clone(&changes), bus.clone(), stop.clone());
            async move { changes.run(&bus, Duration::from_millis(20), &stop).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(dirs(&drain(&mut rx)), [(3, strings(&["early"]))]);
        stop.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn stopping_flushes_what_is_pending() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let changes = Arc::new(FsChanges::default());
        let stop = CancellationToken::new();
        let task = tokio::spawn({
            let (changes, bus, stop) = (Arc::clone(&changes), bus.clone(), stop.clone());
            async move { changes.run(&bus, Duration::from_secs(30), &stop).await }
        });

        changes.push(5, strings(&["last"]));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(drain(&mut rx).is_empty(), "still inside the window");

        stop.cancel();
        task.await.unwrap();
        assert_eq!(dirs(&drain(&mut rx)), [(5, strings(&["last"]))]);
    }
}
