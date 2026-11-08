import { type ReadonlySignal, signal } from '@preact/signals-core';
import type { HeldRemovalsDto } from '@/api/generated/HeldRemovalsDto';
import type { HostWarning } from '@/api/generated/HostWarning';
import type { IndexStatusDto } from '@/api/generated/IndexStatusDto';
import type { WatcherStatusDto } from '@/api/generated/WatcherStatusDto';
import { getIndexStatus } from '@/api/indexing';
import { events } from '@/api/sse';
import { isEnded } from '@/stores/tasks';

const IDLE: Readonly<IndexStatusDto> = Object.freeze({
  held_removals: [],
  active_task_id: null,
  watcher: null,
  warnings: [],
});

const current = signal<IndexStatusDto>(IDLE);

/**
 * What the index reports (`GET /api/index/status`): above all the removals its safety guard holds
 * back per volume. A scan that finds a large share of the indexed files missing (an unmounted
 * share looks exactly like that) keeps their entries and says so here. Nothing is held, and the
 * value is idle, until the first answer arrives.
 */
export const indexStatus: ReadonlySignal<IndexStatusDto> = current;

function isHeld(value: unknown): value is HeldRemovalsDto {
  if (typeof value !== 'object' || value === null) return false;
  const entry = value as Record<string, unknown>;
  return (
    typeof entry.volume_id === 'number' &&
    typeof entry.volume_name === 'string' &&
    typeof entry.files === 'number'
  );
}

function isWarning(value: unknown): value is HostWarning {
  if (typeof value !== 'object' || value === null) return false;
  const entry = value as Record<string, unknown>;
  return typeof entry.code === 'string' && typeof entry.message === 'string';
}

function isStatus(value: unknown): value is IndexStatusDto {
  if (typeof value !== 'object' || value === null) return false;
  const {
    held_removals: held,
    active_task_id: active,
    watcher,
    warnings,
  } = value as Record<string, unknown>;
  return (
    Array.isArray(held) &&
    held.every(isHeld) &&
    Array.isArray(warnings) &&
    warnings.every(isWarning) &&
    (active === null || typeof active === 'number') &&
    (watcher === null || isWatcher(watcher))
  );
}

function isWatcher(value: unknown): value is WatcherStatusDto {
  if (typeof value !== 'object' || value === null) return false;
  const entry = value as Record<string, unknown>;
  return (
    ['disabled', 'starting', 'watching', 'degraded', 'polling_only'].includes(
      String(entry.state),
    ) &&
    typeof entry.watched_dirs === 'number' &&
    typeof entry.unwatched_dirs === 'number' &&
    typeof entry.pending_dirs === 'number' &&
    typeof entry.limit_reached === 'boolean' &&
    (entry.inotify_budget === null || typeof entry.inotify_budget === 'number') &&
    (entry.last_event_at === null || typeof entry.last_event_at === 'number')
  );
}

function sameWatcher(a: WatcherStatusDto | null, b: WatcherStatusDto | null): boolean {
  if (a === null || b === null) return a === b;
  return (
    a.state === b.state &&
    a.watched_dirs === b.watched_dirs &&
    a.unwatched_dirs === b.unwatched_dirs &&
    a.limit_reached === b.limit_reached &&
    a.inotify_budget === b.inotify_budget &&
    a.pending_dirs === b.pending_dirs &&
    a.last_event_at === b.last_event_at
  );
}

function sameStatus(a: IndexStatusDto, b: IndexStatusDto): boolean {
  return (
    a.active_task_id === b.active_task_id &&
    sameWatcher(a.watcher, b.watcher) &&
    a.warnings.length === b.warnings.length &&
    a.warnings.every((warning, index) => {
      const other = b.warnings[index];
      return (
        other !== undefined && warning.code === other.code && warning.message === other.message
      );
    }) &&
    a.held_removals.length === b.held_removals.length &&
    a.held_removals.every((held, index) => {
      const other = b.held_removals[index];
      return (
        other !== undefined &&
        held.volume_id === other.volume_id &&
        held.volume_name === other.volume_name &&
        held.files === other.files
      );
    })
  );
}

async function fetchOnce(): Promise<void> {
  try {
    const status = await getIndexStatus();
    // An unchanged answer keeps its identity, so nothing re-renders for a no-op refresh.
    if (isStatus(status) && !sameStatus(current.peek(), status)) current.value = status;
  } catch {
    // Silent on purpose: the next trigger retries, and a stale notice beats a noisy error.
  }
}

let inFlight: Promise<void> | undefined;
let reloadRequested = false;

/**
 * Fetches the status into `indexStatus`. Never rejects: a failure (or an answer of the wrong
 * shape) leaves the previous status in place. Calls made while a fetch is running share it and
 * cause exactly one more afterwards, so the result is never older than the call.
 */
export function refreshIndexStatus(): Promise<void> {
  if (inFlight) {
    reloadRequested = true;
    return inFlight;
  }
  inFlight = (async () => {
    try {
      do {
        reloadRequested = false;
        await fetchOnce();
      } while (reloadRequested);
    } finally {
      inFlight = undefined;
    }
  })();
  return inFlight;
}

/**
 * Loads the status now and keeps it fresh: after the event stream reconnects (events may have been
 * missed) and whenever an `index` task ends, however it ends, because a scan is what sets or
 * clears the held removals. The shell calls this once; the returned function stops it.
 */
export function startIndexStatus(): () => void {
  void refreshIndexStatus();
  const stops = [
    events.onResync(() => void refreshIndexStatus()),
    events.on('task', ({ task }) => {
      if (task.kind === 'index' && isEnded(task)) void refreshIndexStatus();
    }),
  ];
  return () => {
    for (const stop of stops) stop();
  };
}

/** Back to idle, for tests. */
export function resetIndexStatusForTests(): void {
  current.value = IDLE;
}
