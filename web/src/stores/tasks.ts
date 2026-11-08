import { batch, computed, type ReadonlySignal, signal } from '@preact/signals-core';
import { errorMessage, isApiError } from '@/api/errors';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TaskState } from '@/api/generated/TaskState';
import { events } from '@/api/sse';
import { cancelTask, listTasks } from '@/api/tasks';
import { toast } from '@/components/toast';
import { persistedSignal, type Validator } from '@/stores/persisted';

/**
 * The background tasks (indexing, file operations, plan execution, Immich) as the user sees them.
 * `startTasks()` (the shell calls it once) loads `GET /api/tasks`, then follows the `task` events
 * of the event stream; `tasks` is what the task indicator lists. Nothing polls.
 *
 * What stays visible:
 * - `queued` and `running` tasks: always.
 * - `done` and `cancelled` tasks: `FINISHED_LINGER_MS` after they ended, so the user sees the
 *   result, then they disappear. The first list never brings these back.
 * - `failed` and `interrupted` tasks: until `dismiss(id)` or `dismissFinished()`. The first list
 *   brings back the ones that ended less than `STALE_FAILURE_SECS` ago and were not dismissed;
 *   dismissed ids are remembered (`DISMISSED_LIMIT` of them) so a reload does not bring them back.
 */

/** How long a task that ended well (`done`, `cancelled`) stays listed after it ended. */
export const FINISHED_LINGER_MS = 5_000;

/** The first list only brings back failures that ended less than this long ago. */
export const STALE_FAILURE_SECS = 24 * 60 * 60;

/** How many dismissed task ids are remembered; the oldest is forgotten first. */
export const DISMISSED_LIMIT = 200;

const ENDED = 2;

// A record, so a state added to the backend enum fails the type check until it is ranked here.
// There is no replay and no per-task version on the wire: an event that arrives late must never
// move a task backwards, so a lower rank never overwrites a higher one.
const RANK: Readonly<Record<TaskState, 0 | 1 | 2>> = {
  queued: 0,
  running: 1,
  done: ENDED,
  failed: ENDED,
  cancelled: ENDED,
  interrupted: ENDED,
};

/** Waiting for its turn or running: it can still be cancelled. */
export function isActive(task: TaskDto): boolean {
  return RANK[task.state] < ENDED;
}

/** Over, whatever the outcome. */
export function isEnded(task: TaskDto): boolean {
  return RANK[task.state] === ENDED;
}

/** Ended badly (a failure, or a restart of the server that cut it short). */
export function isFailure(task: TaskDto): boolean {
  return task.state === 'failed' || task.state === 'interrupted';
}

function clamp01(value: number): number {
  return Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0;
}

/**
 * How far a task is, 0 to 1, or `null` when it declared no total (show an indeterminate bar). A
 * task with a byte total counts bytes, else a task with an item total counts items; the two are
 * never added together. A total of zero is 0 until the task is `done`, then 1.
 */
export function taskFraction(task: TaskDto): number | null {
  const bytes = task.bytes_total !== null;
  const total = bytes ? task.bytes_total : task.items_total;
  if (total === null) return null;
  if (!(total > 0)) return task.state === 'done' ? 1 : 0;
  return clamp01((bytes ? task.bytes_done : task.items_done) / total);
}

export interface TaskAggregate {
  running: number;
  queued: number;
  /** Failed and interrupted tasks that were not dismissed. */
  failed: number;
  /**
   * The mean of `taskFraction` over the running tasks that have a total (0 to 1), or `null` when
   * none has one yet: show an indeterminate bar.
   */
  fraction: number | null;
}

type Source = 'event' | 'snapshot';

const byId = signal<ReadonlyMap<number, TaskDto>>(new Map());
const lingering = signal<ReadonlySet<number>>(new Set());
const cancellingIds = signal<ReadonlySet<number>>(new Set());

const isIdList: Validator<number[]> = (value): value is number[] =>
  Array.isArray(value) && value.every((id) => Number.isInteger(id));

const dismissed = persistedSignal<number[]>('tasks.dismissed', [], isIdList);

const lingerTimers = new Map<number, number>();
const finishedHandlers = new Set<(task: TaskDto) => void>();

function byStart(a: TaskDto, b: TaskDto): number {
  return (
    (a.started_at ?? Number.MAX_SAFE_INTEGER) - (b.started_at ?? Number.MAX_SAFE_INTEGER) ||
    a.id - b.id
  );
}

function newestFirst(a: TaskDto, b: TaskDto): number {
  return (b.finished_at ?? 0) - (a.finished_at ?? 0) || b.id - a.id;
}

/**
 * The tasks to show: running ones first (the oldest start first), then the queued ones (in queue
 * order), then the finished ones that are still visible (the latest first).
 */
export const tasks: ReadonlySignal<readonly TaskDto[]> = computed(() => {
  const hidden = new Set(dismissed.value);
  const kept = lingering.value;
  const running: TaskDto[] = [];
  const queued: TaskDto[] = [];
  const finished: TaskDto[] = [];
  for (const task of byId.value.values()) {
    if (task.state === 'running') running.push(task);
    else if (task.state === 'queued') queued.push(task);
    else if (isFailure(task) ? !hidden.has(task.id) : kept.has(task.id)) finished.push(task);
  }
  running.sort(byStart);
  queued.sort((a, b) => a.id - b.id);
  finished.sort(newestFirst);
  return [...running, ...queued, ...finished];
});

/** What the indicator's trigger shows: the counts and the overall progress of `tasks`. */
export const aggregate: ReadonlySignal<TaskAggregate> = computed(() => {
  let running = 0;
  let queued = 0;
  let failed = 0;
  let sum = 0;
  let known = 0;
  for (const task of tasks.value) {
    if (task.state === 'running') {
      running += 1;
      const fraction = taskFraction(task);
      if (fraction !== null) {
        sum += fraction;
        known += 1;
      }
    } else if (task.state === 'queued') {
      queued += 1;
    } else if (isFailure(task)) {
      failed += 1;
    }
  }
  return { running, queued, failed, fraction: known > 0 ? sum / known : null };
});

/**
 * The ids of the tasks the user asked to cancel and that have not ended yet: show "Cancelling...".
 * The flag stays until the task ends (a running task keeps sending progress while its runner winds
 * down) or the request fails.
 */
export const cancelling: ReadonlySignal<ReadonlySet<number>> = cancellingIds;

function isTaskLike(value: unknown): value is TaskDto {
  if (typeof value !== 'object' || value === null) return false;
  const { id, state } = value as { id?: unknown; state?: unknown };
  return typeof id === 'number' && typeof state === 'string' && Object.hasOwn(RANK, state);
}

function endedRecently(task: TaskDto): boolean {
  return task.finished_at === null || Date.now() / 1000 - task.finished_at <= STALE_FAILURE_SECS;
}

function stopLinger(id: number): void {
  const handle = lingerTimers.get(id);
  if (handle === undefined) return;
  window.clearTimeout(handle);
  lingerTimers.delete(id);
}

/** Drops tasks from the store (not from the server) with their timers and flags. */
function forget(ids: readonly number[]): void {
  batch(() => {
    const nextTasks = new Map(byId.peek());
    const nextLingering = new Set(lingering.peek());
    const nextCancelling = new Set(cancellingIds.peek());
    for (const id of ids) {
      stopLinger(id);
      nextTasks.delete(id);
      nextLingering.delete(id);
      nextCancelling.delete(id);
    }
    byId.value = nextTasks;
    lingering.value = nextLingering;
    cancellingIds.value = nextCancelling;
  });
}

function startLinger(id: number): void {
  if (lingerTimers.has(id)) return;
  lingerTimers.set(
    id,
    window.setTimeout(() => forget([id]), FINISHED_LINGER_MS),
  );
  lingering.value = new Set(lingering.peek()).add(id);
}

function notifyFinished(task: TaskDto): void {
  for (const handler of [...finishedHandlers]) {
    try {
      handler(task);
    } catch (error) {
      console.error('[tasks] a finished-task handler threw', error);
    }
  }
}

/**
 * Merges one task into the store. `event` is a `task` event of the stream. `snapshot` is an answer
 * of the API (the list, a cancel): unlike an event it cannot show that a task that is not in the
 * store ended just now, so it only adds one that is active or a recent failure.
 */
function ingest(task: unknown, source: Source): void {
  if (!isTaskLike(task)) return;
  const previous = byId.peek().get(task.id);
  const ended = isEnded(task);
  if (previous && RANK[previous.state] > RANK[task.state]) return;
  if (!previous && ended) {
    if (source === 'snapshot' && !(isFailure(task) && endedRecently(task))) return;
    if (isFailure(task) && dismissed.peek().includes(task.id)) return;
  }
  const justEnded = ended && (previous ? !isEnded(previous) : source === 'event');
  batch(() => {
    byId.value = new Map(byId.peek()).set(task.id, task);
    if (!ended) return;
    if (cancellingIds.peek().has(task.id)) {
      const next = new Set(cancellingIds.peek());
      next.delete(task.id);
      cancellingIds.value = next;
    }
    if (isFailure(task)) {
      stopLinger(task.id);
      if (lingering.peek().has(task.id)) {
        const next = new Set(lingering.peek());
        next.delete(task.id);
        lingering.value = next;
      }
    } else {
      startLinger(task.id);
    }
  });
  if (justEnded) notifyFinished(task);
}

let inFlight: Promise<void> | undefined;
let reloadRequested = false;

async function fetchOnce(): Promise<void> {
  try {
    const list = await listTasks();
    if (!Array.isArray(list)) return;
    batch(() => {
      for (const task of list) ingest(task, 'snapshot');
    });
  } catch {
    // Offline or restarting: the events, or the resync after the reconnect, bring the tasks.
  }
}

/**
 * Fetches `GET /api/tasks` and merges it into the store, which keeps what it already knows (a
 * state is never moved backwards). Never rejects. Calls made while a load is running share it and
 * cause exactly one more load afterwards, so the result is never older than the call.
 */
export function loadTasks(): Promise<void> {
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
 * Loads the tasks now and keeps them current: every `task` event is merged, and the list is
 * fetched again whenever the event stream reconnects (events were possibly missed). The shell
 * calls this once and keeps the stream itself running; the returned function stops it.
 */
export function startTasks(): () => void {
  void loadTasks();
  const stops = [
    events.onResync(() => void loadTasks()),
    events.on('task', ({ task }) => ingest(task, 'event')),
  ];
  return () => {
    for (const stop of stops) stop();
  };
}

/**
 * Calls `handler` for every task that ends while the page is open (a task that was already over
 * when the list arrived does not count). Returns the function that unsubscribes. The indicator
 * announces these to screen readers.
 */
export function onTaskFinished(handler: (task: TaskDto) => void): () => void {
  finishedHandlers.add(handler);
  return () => {
    finishedHandlers.delete(handler);
  };
}

function setCancelling(id: number, on: boolean): void {
  const next = new Set(cancellingIds.peek());
  if (on) next.add(id);
  else next.delete(id);
  cancellingIds.value = next;
}

/**
 * Asks the server to cancel a queued or running task (`POST /api/tasks/{id}/cancel`). A queued task
 * is cancelled at once; a running one stops when its runner returns, and `cancelling` says so in
 * the meantime. Never rejects: a failure becomes an error toast, except that a task which is
 * already over (409) or unknown (404) is ignored silently and the list is fetched again.
 */
export async function cancel(id: number): Promise<void> {
  const task = byId.peek().get(id);
  if (cancellingIds.peek().has(id) || (task && isEnded(task))) return;
  setCancelling(id, true);
  try {
    ingest(await cancelTask(id), 'snapshot');
  } catch (failure) {
    setCancelling(id, false);
    if (isApiError(failure)) {
      const unknown = failure.status === 404 || failure.code === 'not_found';
      const over = failure.status === 409 || failure.code === 'conflict';
      if (unknown || over) {
        if (unknown) forget([id]);
        void loadTasks();
        return;
      }
    }
    toast.error(errorMessage(failure, 'Could not cancel the task.'));
  }
}

function remember(ids: readonly number[]): void {
  if (ids.length === 0) return;
  const kept = dismissed.peek().filter((id) => !ids.includes(id));
  dismissed.value = [...kept, ...ids].slice(-DISMISSED_LIMIT);
}

/**
 * Removes a finished task from the list. A failed or interrupted one is remembered, so a reload
 * does not bring it back. Does nothing for a queued or running task.
 */
export function dismiss(id: number): void {
  const task = byId.peek().get(id);
  if (!task || !isEnded(task)) return;
  batch(() => {
    if (isFailure(task)) remember([id]);
    forget([id]);
  });
}

/** Removes every finished task from the list (see `dismiss`). */
export function dismissFinished(): void {
  const finished = [...byId.peek().values()].filter(isEnded);
  batch(() => {
    remember(finished.filter(isFailure).map((task) => task.id));
    forget(finished.map((task) => task.id));
  });
}

/** Back to the initial state: no task, no timer, no flag, nothing dismissed, no handler. */
export function resetTasksForTests(): void {
  for (const handle of lingerTimers.values()) window.clearTimeout(handle);
  lingerTimers.clear();
  finishedHandlers.clear();
  inFlight = undefined;
  reloadRequested = false;
  batch(() => {
    byId.value = new Map();
    lingering.value = new Set();
    cancellingIds.value = new Set();
    dismissed.value = [];
  });
}
