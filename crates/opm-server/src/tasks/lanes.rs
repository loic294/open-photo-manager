//! Lane scheduling: which queued task starts next.
//!
//! A pure data structure (no I/O, no clock, no locking), so the rules are tested exhaustively
//! below; the engine keeps it behind its own mutex together with the table of live tasks.

use std::collections::VecDeque;

use opm_core::paths::TaskId;

use super::model::Lane;

pub(super) const MAX_RUNNING: usize = 3;

/// How many tasks of each lane run at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneLimits {
    pub fs: usize,
    pub index: usize,
    pub immich: usize,
}

impl Default for LaneLimits {
    fn default() -> Self {
        Self {
            fs: Lane::Fs.default_concurrency(),
            index: Lane::Index.default_concurrency(),
            immich: Lane::Immich.default_concurrency(),
        }
    }
}

impl LaneLimits {
    /// The limit of `lane`, never below one so that a lane cannot be configured into starvation.
    pub fn get(&self, lane: Lane) -> usize {
        let limit = match lane {
            Lane::Fs => self.fs,
            Lane::Index => self.index,
            Lane::Immich => self.immich,
        };
        limit.max(1)
    }
}

struct LaneState<T> {
    queue: VecDeque<(TaskId, T)>,
    running: usize,
}

impl<T> Default for LaneState<T> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            running: 0,
        }
    }
}

/// The waiting line of every lane plus how many tasks of the lane are running.
pub(super) struct Scheduler<T> {
    limits: LaneLimits,
    fs: LaneState<T>,
    index: LaneState<T>,
    immich: LaneState<T>,
}

impl<T> Scheduler<T> {
    pub(super) fn new(limits: LaneLimits) -> Self {
        Self {
            limits,
            fs: LaneState::default(),
            index: LaneState::default(),
            immich: LaneState::default(),
        }
    }

    /// Puts a task at the back of its lane.
    pub(super) fn enqueue(&mut self, lane: Lane, id: TaskId, item: T) {
        self.lane_mut(lane).queue.push_back((id, item));
    }

    /// Starts the oldest eligible task, respecting both lane limits and the shared three slots.
    pub(super) fn start_next(&mut self) -> Option<T> {
        if Lane::ALL
            .iter()
            .map(|lane| self.lane(*lane).running)
            .sum::<usize>()
            >= MAX_RUNNING
        {
            return None;
        }
        let (_, lane) = Lane::ALL
            .into_iter()
            .filter_map(|lane| {
                let state = self.lane(lane);
                (state.running < self.limits.get(lane))
                    .then(|| state.queue.front().map(|(id, _)| (*id, lane)))
                    .flatten()
            })
            .min_by_key(|(id, _)| *id)?;
        let state = self.lane_mut(lane);
        let (_, item) = state.queue.pop_front()?;
        state.running += 1;
        Some(item)
    }

    /// A running task of `lane` ended: its slot is free again.
    pub(super) fn finish(&mut self, lane: Lane) {
        let state = self.lane_mut(lane);
        state.running = state.running.saturating_sub(1);
    }

    /// Takes a task that has not started out of its line.
    pub(super) fn remove_queued(&mut self, id: TaskId) -> Option<T> {
        for lane in Lane::ALL {
            let queue = &mut self.lane_mut(lane).queue;
            if let Some(position) = queue.iter().position(|(queued, _)| *queued == id) {
                return queue.remove(position).map(|(_, item)| item);
            }
        }
        None
    }

    /// Takes every task that has not started (lane by lane, oldest first).
    pub(super) fn drain_queued(&mut self) -> Vec<T> {
        let mut drained = Vec::new();
        for lane in Lane::ALL {
            drained.extend(self.lane_mut(lane).queue.drain(..).map(|(_, item)| item));
        }
        drained
    }

    fn lane_mut(&mut self, lane: Lane) -> &mut LaneState<T> {
        match lane {
            Lane::Fs => &mut self.fs,
            Lane::Index => &mut self.index,
            Lane::Immich => &mut self.immich,
        }
    }

    fn lane(&self, lane: Lane) -> &LaneState<T> {
        match lane {
            Lane::Fs => &self.fs,
            Lane::Index => &self.index,
            Lane::Immich => &self.immich,
        }
    }

    #[cfg(test)]
    fn queued(&self, lane: Lane) -> Vec<TaskId> {
        let state = match lane {
            Lane::Fs => &self.fs,
            Lane::Index => &self.index,
            Lane::Immich => &self.immich,
        };
        state.queue.iter().map(|(id, _)| *id).collect()
    }

    #[cfg(test)]
    fn running(&self, lane: Lane) -> usize {
        match lane {
            Lane::Fs => self.fs.running,
            Lane::Index => self.index.running,
            Lane::Immich => self.immich.running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler() -> Scheduler<&'static str> {
        Scheduler::new(LaneLimits {
            fs: 1,
            index: 1,
            immich: 2,
        })
    }

    #[test]
    fn default_limits_allow_three_in_any_lane() {
        let limits = LaneLimits::default();
        assert_eq!((limits.fs, limits.index, limits.immich), (3, 3, 3));
    }

    #[test]
    fn three_slots_are_shared_across_lanes_even_with_higher_custom_limits() {
        for lanes in [
            [Lane::Fs; 4],
            [Lane::Index; 4],
            [Lane::Immich; 4],
            [Lane::Fs, Lane::Index, Lane::Immich, Lane::Fs],
        ] {
            let mut scheduler = Scheduler::new(LaneLimits {
                fs: 10,
                index: 10,
                immich: 10,
            });
            for (id, lane) in (1..).zip(lanes) {
                scheduler.enqueue(lane, id, id);
            }
            assert_eq!(scheduler.start_next(), Some(1));
            assert_eq!(scheduler.start_next(), Some(2));
            assert_eq!(scheduler.start_next(), Some(3));
            assert_eq!(scheduler.start_next(), None);
            scheduler.finish(lanes[0]);
            assert_eq!(scheduler.start_next(), Some(4));
            assert_eq!(scheduler.start_next(), None);
        }
    }

    #[test]
    fn waiting_lanes_get_slots_in_submission_order() {
        let mut scheduler = Scheduler::new(LaneLimits::default());
        for id in 1..=3 {
            scheduler.enqueue(Lane::Fs, id, id);
            assert_eq!(scheduler.start_next(), Some(id));
        }
        scheduler.enqueue(Lane::Immich, 4, 4);
        scheduler.enqueue(Lane::Fs, 5, 5);
        scheduler.enqueue(Lane::Index, 6, 6);
        scheduler.finish(Lane::Fs);
        assert_eq!(scheduler.start_next(), Some(4));
        scheduler.finish(Lane::Fs);
        assert_eq!(scheduler.start_next(), Some(5));
        scheduler.finish(Lane::Immich);
        assert_eq!(scheduler.start_next(), Some(6));
    }

    #[test]
    fn limits_never_drop_below_one() {
        let limits = LaneLimits {
            fs: 0,
            index: 0,
            immich: 0,
        };
        for lane in Lane::ALL {
            assert_eq!(limits.get(lane), 1);
        }
        let mut scheduler = Scheduler::new(limits);
        scheduler.enqueue(Lane::Fs, 1, "a");
        assert_eq!(scheduler.start_next(), Some("a"));
    }

    #[test]
    fn a_lane_is_first_in_first_out() {
        let mut scheduler = scheduler();
        for (id, label) in [(1, "a"), (2, "b"), (3, "c")] {
            scheduler.enqueue(Lane::Fs, id, label);
        }
        assert_eq!(scheduler.start_next(), Some("a"));
        assert_eq!(scheduler.start_next(), None, "fs runs one task at a time");
        scheduler.finish(Lane::Fs);
        assert_eq!(scheduler.start_next(), Some("b"));
        scheduler.finish(Lane::Fs);
        assert_eq!(scheduler.start_next(), Some("c"));
        scheduler.finish(Lane::Fs);
        assert_eq!(scheduler.start_next(), None);
        assert_eq!(scheduler.running(Lane::Fs), 0);
    }

    #[test]
    fn the_immich_lane_runs_two_at_once() {
        let mut scheduler = scheduler();
        for (id, label) in [(1, "a"), (2, "b"), (3, "c")] {
            scheduler.enqueue(Lane::Immich, id, label);
        }
        assert_eq!(scheduler.start_next(), Some("a"));
        assert_eq!(scheduler.start_next(), Some("b"));
        assert_eq!(scheduler.start_next(), None);
        assert_eq!(scheduler.running(Lane::Immich), 2);
        assert_eq!(scheduler.queued(Lane::Immich), vec![3]);
        scheduler.finish(Lane::Immich);
        assert_eq!(scheduler.start_next(), Some("c"));
    }

    #[test]
    fn a_busy_lane_does_not_hold_up_the_others() {
        let mut scheduler = scheduler();
        scheduler.enqueue(Lane::Fs, 1, "fs-1");
        scheduler.enqueue(Lane::Fs, 2, "fs-2");
        scheduler.enqueue(Lane::Index, 3, "index-1");
        scheduler.enqueue(Lane::Immich, 4, "immich-1");

        let mut started = Vec::new();
        while let Some(item) = scheduler.start_next() {
            started.push(item);
        }
        assert_eq!(started, ["fs-1", "index-1", "immich-1"]);
        assert_eq!(scheduler.queued(Lane::Fs), vec![2]);
    }

    #[test]
    fn finishing_more_than_started_does_not_underflow() {
        let mut scheduler = scheduler();
        scheduler.finish(Lane::Index);
        scheduler.finish(Lane::Index);
        assert_eq!(scheduler.running(Lane::Index), 0);
        scheduler.enqueue(Lane::Index, 1, "a");
        scheduler.enqueue(Lane::Index, 2, "b");
        assert_eq!(scheduler.start_next(), Some("a"));
        assert_eq!(scheduler.start_next(), None, "the limit is still one");
    }

    #[test]
    fn a_queued_task_can_be_removed_from_the_middle() {
        let mut scheduler = scheduler();
        for (id, label) in [(1, "a"), (2, "b"), (3, "c")] {
            scheduler.enqueue(Lane::Fs, id, label);
        }
        assert_eq!(scheduler.remove_queued(2), Some("b"));
        assert_eq!(scheduler.remove_queued(2), None, "only once");
        assert_eq!(scheduler.remove_queued(99), None);
        assert_eq!(scheduler.queued(Lane::Fs), vec![1, 3]);
        assert_eq!(scheduler.start_next(), Some("a"));
        scheduler.finish(Lane::Fs);
        assert_eq!(scheduler.start_next(), Some("c"));
    }

    #[test]
    fn a_running_task_is_not_in_the_queue_any_more() {
        let mut scheduler = scheduler();
        scheduler.enqueue(Lane::Fs, 1, "a");
        assert_eq!(scheduler.start_next(), Some("a"));
        assert_eq!(scheduler.remove_queued(1), None);
    }

    #[test]
    fn draining_empties_every_lane_in_order() {
        let mut scheduler = scheduler();
        scheduler.enqueue(Lane::Immich, 1, "immich");
        scheduler.enqueue(Lane::Fs, 2, "fs-1");
        scheduler.enqueue(Lane::Index, 3, "index");
        scheduler.enqueue(Lane::Fs, 4, "fs-2");
        assert_eq!(
            scheduler.drain_queued(),
            ["fs-1", "fs-2", "index", "immich"]
        );
        assert_eq!(scheduler.start_next(), None);
        assert!(scheduler.drain_queued().is_empty());
    }
}
