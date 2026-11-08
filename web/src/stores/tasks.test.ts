import { afterEach, beforeEach, describe, expect, it, type MockInstance, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { TaskDto } from '@/api/generated/TaskDto';
import { events, SERVER_RETRY_MS } from '@/api/sse';
import { STORAGE_PREFIX } from '@/stores/persisted';
import {
  aggregate,
  cancel,
  cancelling,
  DISMISSED_LIMIT,
  dismiss,
  dismissFinished,
  FINISHED_LINGER_MS,
  loadTasks,
  onTaskFinished,
  resetTasksForTests,
  STALE_FAILURE_SECS,
  startTasks,
  taskFraction,
  tasks,
} from '@/stores/tasks';
import { clearToasts, toasts } from '@/stores/toasts';
import { FakeEventSource } from '@/test/fake-event-source';
import { makeTask, taskEvent } from '@/test/task-fixtures';

const NOW_MS = Date.UTC(2026, 4, 12, 12, 0, 0);
const NOW_SECS = NOW_MS / 1000;
const DISMISSED_KEY = `${STORAGE_PREFIX}tasks.dismissed`;

let get: MockInstance<typeof api.get>;
let post: MockInstance<typeof api.post>;
let stop: (() => void) | undefined;

// Lets a load that was just started finish (a macrotask later every promise has settled).
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

const ids = () => tasks.value.map((task) => task.id);

function conflict(status: number, code: string) {
  return new ApiError({ status, code, message: `The server said ${code}` });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function storedDismissed(): unknown {
  return JSON.parse(localStorage.getItem(DISMISSED_KEY) ?? 'null');
}

/** Starts the store the way the shell does; `list` is what `GET /api/tasks` answers. */
async function start(list: readonly TaskDto[] = []) {
  get.mockResolvedValue([...list]);
  stop = startTasks();
  await settle();
}

beforeEach(() => {
  resetTasksForTests();
  localStorage.clear();
  clearToasts();
  vi.setSystemTime(NOW_MS);
  FakeEventSource.reset();
  vi.stubGlobal('EventSource', FakeEventSource);
  get = vi.spyOn(api, 'get').mockResolvedValue([]);
  post = vi.spyOn(api, 'post');
});

afterEach(() => {
  stop?.();
  stop = undefined;
  events.stop();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  resetTasksForTests();
  clearToasts();
});

describe('taskFraction', () => {
  it('counts bytes when the task declared a byte total', () => {
    const task = makeTask({ bytes_done: 50, bytes_total: 200, items_done: 9, items_total: 10 });

    expect(taskFraction(task)).toBe(0.25);
  });

  it('counts items when only an item total is declared', () => {
    expect(taskFraction(makeTask({ items_done: 30, items_total: 120, bytes_done: 5000 }))).toBe(
      0.25,
    );
  });

  it('is null while no total is known', () => {
    expect(taskFraction(makeTask({ items_done: 40, bytes_done: 4096 }))).toBeNull();
  });

  it('stays between 0 and 1', () => {
    expect(taskFraction(makeTask({ items_done: 12, items_total: 10 }))).toBe(1);
    expect(taskFraction(makeTask({ items_done: -3, items_total: 10 }))).toBe(0);
    expect(taskFraction(makeTask({ bytes_done: Number.NaN, bytes_total: 10 }))).toBe(0);
  });

  it.each([
    ['running', 0],
    ['queued', 0],
    ['failed', 0],
    ['done', 1],
  ] as const)('reads a zero total of a %s task as %d', (state, expected) => {
    expect(taskFraction(makeTask({ state, items_total: 0 }))).toBe(expected);
    expect(taskFraction(makeTask({ state, bytes_total: 0 }))).toBe(expected);
  });

  it('never adds bytes and items together', () => {
    const task = makeTask({ bytes_done: 10, bytes_total: 100, items_done: 10, items_total: 10 });

    expect(taskFraction(task)).toBe(0.1);
  });
});

describe('merging events', () => {
  it('follows a task from queued to done and always shows its latest title', async () => {
    await start();

    events.emit(taskEvent({ id: 4, state: 'queued', title: 'Move', started_at: null }));
    expect(tasks.value).toMatchObject([{ id: 4, state: 'queued', title: 'Move' }]);

    events.emit(taskEvent({ id: 4, state: 'running', title: 'Move 10 files', items_total: 10 }));
    events.emit(taskEvent({ id: 4, state: 'running', title: 'Move 9 files', items_done: 4 }));
    expect(tasks.value).toMatchObject([
      { id: 4, state: 'running', title: 'Move 9 files', items_done: 4 },
    ]);

    events.emit(taskEvent({ id: 4, state: 'done', title: 'Moved 9 files', finished_at: 2_000 }));
    expect(tasks.value).toMatchObject([{ id: 4, state: 'done', title: 'Moved 9 files' }]);
  });

  it('lets the counters of a running task go down, a new phase restarts them', async () => {
    await start();

    events.emit(taskEvent({ items_done: 80, items_total: 100 }));
    events.emit(taskEvent({ items_done: 5, items_total: 40 }));

    expect(tasks.value[0]).toMatchObject({ items_done: 5, items_total: 40 });
  });

  it('never moves a task back to an earlier state', async () => {
    await start();

    events.emit(taskEvent({ id: 1, state: 'running', items_done: 7 }));
    events.emit(taskEvent({ id: 1, state: 'queued', items_done: 0, started_at: null }));
    expect(tasks.value[0]).toMatchObject({ state: 'running', items_done: 7 });

    events.emit(taskEvent({ id: 2, state: 'failed', error: 'disk full', finished_at: 9 }));
    events.emit(taskEvent({ id: 2, state: 'running' }));
    expect(tasks.value.find((task) => task.id === 2)).toMatchObject({ state: 'failed' });
  });

  it('shows a task whose end is the first thing it hears of', async () => {
    await start();

    events.emit(taskEvent({ id: 3, state: 'done', finished_at: 5 }));

    expect(ids()).toEqual([3]);
  });

  it('ignores an event without a usable id or state', async () => {
    await start();
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});

    events.emit({ type: 'task', task: { id: 'x', state: 'running' } as unknown as TaskDto });
    events.emit({ type: 'task', task: { id: 1, state: 'exploded' } as unknown as TaskDto });
    events.emit({ type: 'task', task: {} as TaskDto });

    expect(ids()).toEqual([]);
    expect(error).not.toHaveBeenCalled();
  });
});

describe('order', () => {
  it('lists running tasks first, then queued ones, then finished ones', async () => {
    await start();

    events.emit(taskEvent({ id: 5, state: 'queued', started_at: null }));
    events.emit(taskEvent({ id: 3, state: 'queued', started_at: null }));
    events.emit(taskEvent({ id: 9, state: 'running', started_at: 2_000 }));
    events.emit(taskEvent({ id: 8, state: 'running', started_at: 1_000 }));
    events.emit(taskEvent({ id: 2, state: 'failed', finished_at: 100 }));
    events.emit(taskEvent({ id: 4, state: 'failed', finished_at: 300 }));
    events.emit(taskEvent({ id: 6, state: 'interrupted', finished_at: 300 }));

    expect(ids()).toEqual([8, 9, 3, 5, 6, 4, 2]);
  });

  it('breaks ties between running tasks by id and puts one without a start last', async () => {
    await start();

    events.emit(taskEvent({ id: 12, state: 'running', started_at: null }));
    events.emit(taskEvent({ id: 11, state: 'running', started_at: 1_000 }));
    events.emit(taskEvent({ id: 10, state: 'running', started_at: 1_000 }));

    expect(ids()).toEqual([10, 11, 12]);
  });
});

describe('visibility', () => {
  beforeEach(async () => {
    await start();
    vi.useFakeTimers();
    vi.setSystemTime(NOW_MS);
  });

  it.each(['done', 'cancelled'] as const)(
    'keeps a %s task for %d ms after it ended, then forgets it',
    (state) => {
      events.emit(taskEvent({ state: 'running' }));
      events.emit(taskEvent({ state, finished_at: NOW_SECS }));
      expect(ids()).toEqual([1]);

      vi.advanceTimersByTime(FINISHED_LINGER_MS - 1);
      expect(ids()).toEqual([1]);

      vi.advanceTimersByTime(1);
      expect(ids()).toEqual([]);
    },
  );

  it.each(['failed', 'interrupted'] as const)('keeps a %s task until it is dismissed', (state) => {
    events.emit(taskEvent({ state: 'running' }));
    events.emit(taskEvent({ state, error: 'it broke', finished_at: NOW_SECS }));

    vi.advanceTimersByTime(30 * 24 * 3600 * 1000);

    expect(ids()).toEqual([1]);
  });

  it('does not restart the countdown when the end is reported again', () => {
    events.emit(taskEvent({ state: 'done', finished_at: NOW_SECS }));
    vi.advanceTimersByTime(3_000);

    events.emit(taskEvent({ state: 'done', finished_at: NOW_SECS }));
    vi.advanceTimersByTime(FINISHED_LINGER_MS - 3_000);

    expect(ids()).toEqual([]);
  });

  it('does not bring back a finished task the list still reports', async () => {
    events.emit(taskEvent({ state: 'done', finished_at: NOW_SECS }));
    vi.advanceTimersByTime(FINISHED_LINGER_MS);
    get.mockResolvedValue([makeTask({ state: 'done', finished_at: NOW_SECS })]);

    await loadTasks();

    expect(ids()).toEqual([]);
  });

  it('stops every countdown when the store is reset', () => {
    events.emit(taskEvent({ state: 'done', finished_at: NOW_SECS }));

    resetTasksForTests();

    expect(vi.getTimerCount()).toBe(0);
  });
});

describe('loadTasks', () => {
  it('fetches GET /api/tasks', async () => {
    await loadTasks();

    expect(get).toHaveBeenCalledOnce();
    expect(get.mock.calls[0]?.[0]).toBe('/api/tasks');
  });

  it('lists the active tasks and the failures of the last 24 hours, nothing else', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW_MS);
    get.mockResolvedValue([
      makeTask({ id: 1, state: 'running' }),
      makeTask({ id: 2, state: 'queued', started_at: null }),
      makeTask({ id: 3, state: 'done', finished_at: NOW_SECS - 60 }),
      makeTask({ id: 4, state: 'cancelled', finished_at: NOW_SECS - 60 }),
      makeTask({ id: 5, state: 'failed', error: 'disk full', finished_at: NOW_SECS - 3_600 }),
      makeTask({ id: 6, state: 'interrupted', finished_at: NOW_SECS - 2 * 3_600 }),
      makeTask({ id: 7, state: 'failed', finished_at: NOW_SECS - STALE_FAILURE_SECS - 1 }),
      makeTask({ id: 8, state: 'failed', finished_at: NOW_SECS - STALE_FAILURE_SECS }),
    ]);

    await loadTasks();

    expect(ids()).toEqual([1, 2, 5, 6, 8]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('does not list a failure that was dismissed', async () => {
    const failed = makeTask({ id: 5, state: 'failed', finished_at: NOW_SECS });
    get.mockResolvedValue([failed]);
    await loadTasks();
    dismiss(5);

    await loadTasks();

    expect(ids()).toEqual([]);
  });

  it('keeps what an event already said when the list is older', async () => {
    await start();
    events.emit(taskEvent({ id: 1, state: 'running', items_done: 50, items_total: 100 }));
    events.emit(taskEvent({ id: 2, state: 'done', finished_at: NOW_SECS }));
    get.mockResolvedValue([
      makeTask({ id: 1, state: 'queued', started_at: null }),
      makeTask({ id: 2, state: 'running' }),
    ]);

    await loadTasks();

    expect(tasks.value).toMatchObject([
      { id: 1, state: 'running', items_done: 50 },
      { id: 2, state: 'done' },
    ]);
  });

  it('keeps a running task the list does not know yet', async () => {
    await start();
    events.emit(taskEvent({ id: 1, state: 'running' }));
    get.mockResolvedValue([]);

    await loadTasks();

    expect(ids()).toEqual([1]);
  });

  it('takes the latest title and counters of a task it already knows', async () => {
    await start();
    events.emit(taskEvent({ id: 1, state: 'running', title: 'Old', items_done: 1 }));
    get.mockResolvedValue([makeTask({ id: 1, state: 'running', title: 'New', items_done: 9 })]);

    await loadTasks();

    expect(tasks.value[0]).toMatchObject({ title: 'New', items_done: 9 });
  });

  it('tolerates an answer that is not a list', async () => {
    get.mockResolvedValue({ tasks: [makeTask()] });

    await expect(loadTasks()).resolves.toBeUndefined();

    expect(ids()).toEqual([]);
  });

  it('never rejects and stays quiet when the request fails', async () => {
    get.mockRejectedValue(conflict(503, 'shutting_down'));

    await expect(loadTasks()).resolves.toBeUndefined();

    expect(toasts.value).toEqual([]);
    expect(ids()).toEqual([]);
  });

  it('runs one load at a time, and one more afterwards so the result is never older', async () => {
    const first = deferred<TaskDto[]>();
    const second = deferred<TaskDto[]>();
    get.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    const calls = [loadTasks(), loadTasks(), loadTasks()];
    expect(get).toHaveBeenCalledTimes(1);

    first.resolve([makeTask({ id: 1 })]);
    await vi.waitFor(() => expect(get).toHaveBeenCalledTimes(2));
    second.resolve([makeTask({ id: 1 }), makeTask({ id: 2, started_at: 2_000 })]);
    await Promise.all(calls);

    expect(get).toHaveBeenCalledTimes(2);
    expect(ids()).toEqual([1, 2]);
  });
});

describe('startTasks', () => {
  it('loads at once', async () => {
    await start([makeTask({ id: 1 })]);

    expect(get).toHaveBeenCalledOnce();
    expect(ids()).toEqual([1]);
  });

  it('merges the task events of the stream', async () => {
    await start();

    events.emit(taskEvent({ id: 6, state: 'running' }));

    expect(ids()).toEqual([6]);
  });

  describe('with a connected event stream', () => {
    // The stream is opened before the store listens: a stream that was stopped before (the shared
    // client is not reset between tests) reports its next connection as a reconnect.
    beforeEach(() => {
      vi.useFakeTimers();
      vi.spyOn(Math, 'random').mockReturnValue(0);
      events.start();
      FakeEventSource.last().open();
    });

    async function reconnect() {
      FakeEventSource.last().error();
      await vi.advanceTimersByTimeAsync(SERVER_RETRY_MS * 2);
      FakeEventSource.last().open();
      await vi.advanceTimersByTimeAsync(0);
    }

    it('fetches again after the stream reconnects, events were possibly missed', async () => {
      stop = startTasks();
      await vi.advanceTimersByTimeAsync(0);
      expect(get).toHaveBeenCalledTimes(1);

      await reconnect();

      expect(get).toHaveBeenCalledTimes(2);
    });

    it('stops listening to the events and the reconnects when it is stopped', async () => {
      const stopListening = startTasks();
      await vi.advanceTimersByTimeAsync(0);

      stopListening();
      events.emit(taskEvent({ id: 6, state: 'running' }));
      await reconnect();

      expect(ids()).toEqual([]);
      expect(get).toHaveBeenCalledTimes(1);
    });
  });
});

describe('aggregate', () => {
  it('is empty when there is no task', () => {
    expect(aggregate.value).toEqual({ running: 0, queued: 0, failed: 0, fraction: null });
  });

  it('counts running, queued and failed tasks, not the ones that ended well', async () => {
    await start();

    events.emit(taskEvent({ id: 1, state: 'running' }));
    events.emit(taskEvent({ id: 2, state: 'running' }));
    events.emit(taskEvent({ id: 3, state: 'queued', started_at: null }));
    events.emit(taskEvent({ id: 4, state: 'failed', finished_at: 5 }));
    events.emit(taskEvent({ id: 5, state: 'interrupted', finished_at: 5 }));
    events.emit(taskEvent({ id: 6, state: 'done', finished_at: 5 }));
    events.emit(taskEvent({ id: 7, state: 'cancelled', finished_at: 5 }));

    expect(aggregate.value).toMatchObject({ running: 2, queued: 1, failed: 2 });
  });

  it('averages the progress of the running tasks that have a total', async () => {
    await start();

    events.emit(taskEvent({ id: 1, bytes_done: 50, bytes_total: 100 }));
    events.emit(taskEvent({ id: 2, items_done: 25, items_total: 100 }));
    events.emit(taskEvent({ id: 3, items_done: 999 }));

    expect(aggregate.value.fraction).toBe(0.375);
  });

  it('has no fraction while no running task has a total', async () => {
    await start();

    events.emit(taskEvent({ id: 1, items_done: 20 }));
    events.emit(taskEvent({ id: 2, state: 'queued', items_total: 10, started_at: null }));
    events.emit(taskEvent({ id: 3, state: 'failed', items_done: 10, items_total: 10 }));

    expect(aggregate.value.fraction).toBeNull();
  });

  it('ignores the progress of queued and finished tasks', async () => {
    await start();

    events.emit(taskEvent({ id: 1, items_done: 10, items_total: 100 }));
    events.emit(taskEvent({ id: 2, state: 'queued', items_done: 100, items_total: 100 }));
    events.emit(taskEvent({ id: 3, state: 'done', items_done: 100, items_total: 100 }));

    expect(aggregate.value.fraction).toBe(0.1);
  });
});

describe('dismiss', () => {
  it('removes a failed task and remembers it', async () => {
    await start();
    events.emit(taskEvent({ id: 4, state: 'failed', finished_at: 5 }));

    dismiss(4);

    expect(ids()).toEqual([]);
    expect(storedDismissed()).toEqual([4]);
  });

  it('does not show a dismissed failure again, whatever reports it', async () => {
    await start();
    events.emit(taskEvent({ id: 4, state: 'failed', finished_at: NOW_SECS }));
    dismiss(4);

    events.emit(taskEvent({ id: 4, state: 'failed', finished_at: NOW_SECS }));
    get.mockResolvedValue([makeTask({ id: 4, state: 'failed', finished_at: NOW_SECS })]);
    await loadTasks();

    expect(ids()).toEqual([]);
  });

  it('removes a task that ended well without remembering it', async () => {
    await start();
    events.emit(taskEvent({ id: 4, state: 'done', finished_at: 5 }));

    dismiss(4);

    expect(ids()).toEqual([]);
    expect(storedDismissed()).toBeNull();
  });

  it('does nothing for a queued or a running task, nor for an unknown one', async () => {
    await start();
    events.emit(taskEvent({ id: 1, state: 'running' }));
    events.emit(taskEvent({ id: 2, state: 'queued', started_at: null }));

    dismiss(1);
    dismiss(2);
    dismiss(99);

    expect(ids()).toEqual([1, 2]);
    expect(storedDismissed()).toBeNull();
  });

  it('forgets the oldest dismissed ids beyond the limit', async () => {
    await start();
    const total = DISMISSED_LIMIT + 5;
    for (let id = 1; id <= total; id += 1) {
      events.emit(taskEvent({ id, state: 'failed', finished_at: id }));
      dismiss(id);
    }

    const stored = storedDismissed() as number[];

    expect(stored).toHaveLength(DISMISSED_LIMIT);
    expect(stored[0]).toBe(6);
    expect(stored.at(-1)).toBe(total);
  });

  it('dismissFinished removes every finished task and keeps the active ones', async () => {
    await start();
    events.emit(taskEvent({ id: 1, state: 'running' }));
    events.emit(taskEvent({ id: 2, state: 'queued', started_at: null }));
    events.emit(taskEvent({ id: 3, state: 'failed', finished_at: 5 }));
    events.emit(taskEvent({ id: 4, state: 'interrupted', finished_at: 6 }));
    events.emit(taskEvent({ id: 5, state: 'done', finished_at: 7 }));
    events.emit(taskEvent({ id: 6, state: 'cancelled', finished_at: 8 }));

    dismissFinished();

    expect(ids()).toEqual([1, 2]);
    expect(storedDismissed()).toEqual([3, 4]);
  });

  it('remembers the dismissed failures of the previous visit', async () => {
    localStorage.setItem(DISMISSED_KEY, JSON.stringify([4]));
    vi.resetModules();
    const [fresh, client] = await Promise.all([import('@/stores/tasks'), import('@/api/client')]);
    vi.spyOn(client.api, 'get').mockResolvedValue([
      makeTask({ id: 4, state: 'failed', finished_at: NOW_SECS }),
      makeTask({ id: 5, state: 'failed', finished_at: NOW_SECS }),
    ]);

    await fresh.loadTasks();

    expect(fresh.tasks.value.map((task) => task.id)).toEqual([5]);
  });

  it('starts with nothing dismissed when the stored list is damaged', async () => {
    localStorage.setItem(DISMISSED_KEY, JSON.stringify({ 4: true }));
    vi.resetModules();
    const [fresh, client] = await Promise.all([import('@/stores/tasks'), import('@/api/client')]);
    vi.spyOn(client.api, 'get').mockResolvedValue([
      makeTask({ id: 4, state: 'failed', finished_at: NOW_SECS }),
    ]);

    await fresh.loadTasks();

    expect(fresh.tasks.value.map((task) => task.id)).toEqual([4]);
  });
});

describe('cancel', () => {
  it('cancels a queued task at once', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'queued', started_at: null }));
    post.mockResolvedValueOnce(
      makeTask({ id: 7, state: 'cancelled', started_at: null, finished_at: NOW_SECS }),
    );

    const request = cancel(7);
    expect(cancelling.value.has(7)).toBe(true);
    await request;

    expect(post).toHaveBeenCalledExactlyOnceWith('/api/tasks/7/cancel');
    expect(tasks.value).toMatchObject([{ id: 7, state: 'cancelled' }]);
    expect(cancelling.value.has(7)).toBe(false);
  });

  it('keeps saying it is cancelling until a running task has ended', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'running', items_done: 1, items_total: 10 }));
    post.mockResolvedValueOnce(makeTask({ id: 7, state: 'running', items_done: 2 }));

    await cancel(7);
    expect(cancelling.value.has(7)).toBe(true);

    events.emit(taskEvent({ id: 7, state: 'running', items_done: 3, items_total: 10 }));
    expect(cancelling.value.has(7)).toBe(true);

    events.emit(taskEvent({ id: 7, state: 'cancelled', items_done: 3, finished_at: NOW_SECS }));
    expect(cancelling.value.has(7)).toBe(false);
    expect(tasks.value[0]).toMatchObject({ state: 'cancelled' });
  });

  it('asks only once while the request is pending', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'running' }));
    const answer = deferred<TaskDto>();
    post.mockReturnValueOnce(answer.promise);

    const first = cancel(7);
    const second = cancel(7);
    answer.resolve(makeTask({ id: 7, state: 'running' }));
    await Promise.all([first, second]);

    expect(post).toHaveBeenCalledOnce();
  });

  it('does not ask for a task that has ended', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'done', finished_at: NOW_SECS }));

    await cancel(7);

    expect(post).not.toHaveBeenCalled();
  });

  it('ignores a conflict, the task ended in another way, and fetches the list again', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'running' }));
    post.mockRejectedValueOnce(conflict(409, 'conflict'));
    get.mockResolvedValue([makeTask({ id: 7, state: 'failed', finished_at: NOW_SECS })]);
    vi.useFakeTimers();
    vi.setSystemTime(NOW_MS);

    await cancel(7);
    await vi.advanceTimersByTimeAsync(0);

    expect(toasts.value).toEqual([]);
    expect(cancelling.value.has(7)).toBe(false);
    expect(get).toHaveBeenCalledTimes(2);
    expect(tasks.value).toMatchObject([{ id: 7, state: 'failed' }]);
  });

  it('ignores an unknown task (404), forgets it and fetches the list again', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'running' }));
    post.mockRejectedValueOnce(conflict(404, 'not_found'));

    await cancel(7);
    await settle();

    expect(toasts.value).toEqual([]);
    expect(ids()).toEqual([]);
    expect(cancelling.value.has(7)).toBe(false);
    expect(get).toHaveBeenCalledTimes(2);
  });

  it('tells the user about any other failure and keeps the task', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'running' }));
    post.mockRejectedValueOnce(conflict(503, 'shutting_down'));

    await cancel(7);

    expect(toasts.value).toMatchObject([
      { kind: 'error', message: 'The server said shutting_down' },
    ]);
    expect(cancelling.value.has(7)).toBe(false);
    expect(ids()).toEqual([7]);
    expect(get).toHaveBeenCalledOnce();
  });

  it('falls back to a plain message when the failure has none', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'running' }));
    post.mockRejectedValueOnce(undefined);

    await cancel(7);

    expect(toasts.value).toMatchObject([{ kind: 'error', message: 'Could not cancel the task.' }]);
  });
});

describe('onTaskFinished', () => {
  it('reports a task that ends while the page is open, once', async () => {
    await start();
    const handler = vi.fn();
    onTaskFinished(handler);

    events.emit(taskEvent({ id: 1, state: 'queued', started_at: null }));
    events.emit(taskEvent({ id: 1, state: 'running' }));
    expect(handler).not.toHaveBeenCalled();

    events.emit(taskEvent({ id: 1, state: 'failed', error: 'disk full', finished_at: NOW_SECS }));
    events.emit(taskEvent({ id: 1, state: 'failed', error: 'disk full', finished_at: NOW_SECS }));

    expect(handler).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ id: 1, state: 'failed' }),
    );
  });

  it('reports a queued task the server cancelled at once', async () => {
    await start();
    events.emit(taskEvent({ id: 7, state: 'queued', started_at: null }));
    post.mockResolvedValueOnce(makeTask({ id: 7, state: 'cancelled', finished_at: NOW_SECS }));
    const handler = vi.fn();
    onTaskFinished(handler);

    await cancel(7);

    expect(handler).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ id: 7, state: 'cancelled' }),
    );
  });

  it('does not report the failures that were already there when the list arrived', async () => {
    const handler = vi.fn();
    onTaskFinished(handler);

    await start([makeTask({ id: 1, state: 'failed', finished_at: NOW_SECS })]);

    expect(ids()).toEqual([1]);
    expect(handler).not.toHaveBeenCalled();
  });

  it('stops reporting once unsubscribed, and survives a handler that throws', async () => {
    await start();
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    const broken = vi.fn(() => {
      throw new Error('boom');
    });
    const working = vi.fn();
    onTaskFinished(broken);
    const unsubscribe = onTaskFinished(working);

    events.emit(taskEvent({ id: 1, state: 'done', finished_at: NOW_SECS }));
    unsubscribe();
    events.emit(taskEvent({ id: 2, state: 'done', finished_at: NOW_SECS }));

    expect(working).toHaveBeenCalledOnce();
    expect(broken).toHaveBeenCalledTimes(2);
    expect(error).toHaveBeenCalled();
  });
});
