import { effect } from '@preact/signals-core';
import { afterEach, describe, expect, it, type MockInstance, vi } from 'vitest';
import { ApiError } from '@/api/errors';
import type { IndexStatusDto } from '@/api/generated/IndexStatusDto';
import type { TaskState } from '@/api/generated/TaskState';
import { SERVER_RETRY_MS } from '@/api/sse';
import { FakeEventSource } from '@/test/fake-event-source';
import { taskEvent } from '@/test/task-fixtures';

type Store = typeof import('@/stores/index-status');
type Get = MockInstance<typeof import('@/api/client')['api']['get']>;

interface Loaded {
  store: Store;
  get: Get;
  events: typeof import('@/api/sse')['events'];
}

const idle: IndexStatusDto = { held_removals: [], active_task_id: null, watcher: null };
const held: IndexStatusDto = {
  held_removals: [{ volume_id: 1, volume_name: 'photos', files: 12_345 }],
  active_task_id: null,
  watcher: null,
};

let current: Loaded | undefined;

// A fresh store (and event client) per test: the module keeps its state at the top level.
async function setup(): Promise<Loaded> {
  vi.resetModules();
  const [store, client, sse] = await Promise.all([
    import('@/stores/index-status'),
    import('@/api/client'),
    import('@/api/sse'),
  ]);
  current = { store, get: vi.spyOn(client.api, 'get'), events: sse.events };
  return current;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function boom(message = 'boom') {
  return new ApiError({ status: 500, code: 'internal', message });
}

// Lets a refresh that was just started, and the one it may have queued, finish.
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

afterEach(() => {
  current?.events.stop();
  current = undefined;
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  FakeEventSource.reset();
});

describe('refreshIndexStatus', () => {
  it('starts idle, without fetching', async () => {
    const { store, get } = await setup();

    expect(store.indexStatus.value).toEqual(idle);
    expect(get).not.toHaveBeenCalled();
  });

  it('fetches GET /api/index/status into the store', async () => {
    const { store, get } = await setup();
    get.mockResolvedValue(held);

    await store.refreshIndexStatus();

    expect(get).toHaveBeenCalledTimes(1);
    expect(get.mock.calls[0]?.[0]).toBe('/api/index/status');
    expect(store.indexStatus.value).toEqual(held);
  });

  it('shows the removals that clear again as soon as the server stops reporting them', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce(held).mockResolvedValueOnce(idle);

    await store.refreshIndexStatus();
    expect(store.indexStatus.value.held_removals).toHaveLength(1);
    await store.refreshIndexStatus();

    expect(store.indexStatus.value).toEqual(idle);
  });

  it('never rejects, and a failed refresh keeps the previous status', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce(held).mockRejectedValueOnce(boom('offline'));
    await store.refreshIndexStatus();

    await expect(store.refreshIndexStatus()).resolves.toBeUndefined();

    expect(store.indexStatus.value).toEqual(held);
  });

  it.each([
    ['not an object', 'nope'],
    ['a list', []],
    ['without held_removals', { active_task_id: null }],
    ['with a malformed entry', { held_removals: [{ volume_id: 1 }], active_task_id: null }],
    ['with a bad active task', { held_removals: [], active_task_id: 'x' }],
    ['with a bad watcher', { ...held, watcher: { state: 'watched' } }],
  ])('ignores an answer of the wrong shape (%s)', async (_what, answer) => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce(held).mockResolvedValueOnce(answer);
    await store.refreshIndexStatus();

    await store.refreshIndexStatus();

    expect(store.indexStatus.value).toEqual(held);
  });

  it('keeps the very same status when a refresh brings nothing new', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce(held);
    await store.refreshIndexStatus();
    const before = store.indexStatus.value;
    const runs = vi.fn();
    const stop = effect(() => {
      runs(store.indexStatus.value);
    });

    get.mockResolvedValueOnce(structuredClone(held));
    await store.refreshIndexStatus();
    stop();

    expect(store.indexStatus.value).toBe(before);
    expect(runs).toHaveBeenCalledTimes(1);
  });

  it.each([
    [
      'another count',
      { ...held, held_removals: [{ volume_id: 1, volume_name: 'photos', files: 7 }] },
    ],
    [
      'another volume',
      { ...held, held_removals: [{ volume_id: 2, volume_name: 'photos', files: 12_345 }] },
    ],
    [
      'another name',
      { ...held, held_removals: [{ volume_id: 1, volume_name: 'pics', files: 12_345 }] },
    ],
    ['another active task', { ...held, active_task_id: 8 }],
    [
      'watcher health',
      {
        ...held,
        watcher: {
          state: 'degraded',
          watched_dirs: 4,
          unwatched_dirs: 2,
          limit_reached: true,
          inotify_budget: 4,
          pending_dirs: 1,
          last_event_at: 1700000000,
        },
      },
    ],
  ])('replaces the status when it brings %s', async (_what, changed) => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce(held).mockResolvedValueOnce(changed);

    await store.refreshIndexStatus();
    await store.refreshIndexStatus();

    expect(store.indexStatus.value).toEqual(changed);
  });

  it('runs one fetch at a time, and one more afterwards so the result is never older than the call', async () => {
    const { store, get } = await setup();
    const early = deferred<IndexStatusDto>();
    const late = deferred<IndexStatusDto>();
    get.mockReturnValueOnce(early.promise).mockReturnValueOnce(late.promise);

    const a = store.refreshIndexStatus();
    const b = store.refreshIndexStatus();
    const c = store.refreshIndexStatus();
    expect(get).toHaveBeenCalledTimes(1);

    early.resolve(idle);
    await vi.waitFor(() => expect(get).toHaveBeenCalledTimes(2));
    expect(store.indexStatus.value).toEqual(idle);

    late.resolve(held);
    await Promise.all([a, b, c]);

    expect(get).toHaveBeenCalledTimes(2);
    expect(store.indexStatus.value).toEqual(held);
  });

  it('can be called again after a refresh finished', async () => {
    const { store, get } = await setup();
    get.mockResolvedValue(idle);

    await store.refreshIndexStatus();
    await store.refreshIndexStatus();

    expect(get).toHaveBeenCalledTimes(2);
  });
});

describe('startIndexStatus', () => {
  async function started() {
    const loaded = await setup();
    loaded.get.mockResolvedValue(held);
    const stop = loaded.store.startIndexStatus();
    await vi.waitFor(() => expect(loaded.store.indexStatus.value).toEqual(held));
    return { ...loaded, stop };
  }

  it('loads straight away', async () => {
    const { get, stop } = await started();
    stop();

    expect(get).toHaveBeenCalledTimes(1);
  });

  it('refreshes after the event stream reconnects, but not for the first connection', async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0);
    vi.stubGlobal('EventSource', FakeEventSource);
    const { store, get, events } = await setup();
    get.mockResolvedValue(idle);
    const stop = store.startIndexStatus();
    await vi.waitFor(() => expect(get).toHaveBeenCalledTimes(1));

    events.start();
    FakeEventSource.last().open();
    await Promise.resolve();
    expect(get).toHaveBeenCalledTimes(1);

    FakeEventSource.last().error();
    vi.advanceTimersByTime(SERVER_RETRY_MS * 2);
    FakeEventSource.last().open();
    await vi.waitFor(() => expect(get).toHaveBeenCalledTimes(2));
    stop();
  });

  it.each(['done', 'failed', 'cancelled', 'interrupted'] as const satisfies readonly TaskState[])(
    'refreshes when an index task ends as %s',
    async (state) => {
      const { get, events, stop } = await started();

      events.emit(taskEvent({ kind: 'index', state, finished_at: 2 }));
      await settle();
      stop();

      expect(get).toHaveBeenCalledTimes(2);
    },
  );

  it.each(['queued', 'running'] as const)(
    'does not refresh while an index task is %s',
    async (state) => {
      const { get, events, stop } = await started();

      events.emit(taskEvent({ kind: 'index', state }));
      await settle();
      stop();

      expect(get).toHaveBeenCalledTimes(1);
    },
  );

  it('ignores tasks of other kinds, however they end', async () => {
    const { get, events, stop } = await started();

    events.emit(taskEvent({ kind: 'fs_move', state: 'done', finished_at: 2 }));
    events.emit(taskEvent({ kind: 'immich_sync', state: 'failed', finished_at: 2 }));
    await settle();
    stop();

    expect(get).toHaveBeenCalledTimes(1);
  });

  it('shows what the status says after the scan that cleared it', async () => {
    const { store, get, events, stop } = await started();
    get.mockResolvedValue(idle);

    events.emit(taskEvent({ kind: 'index', state: 'done', finished_at: 2 }));
    await vi.waitFor(() => expect(store.indexStatus.value).toEqual(idle));
    stop();
  });

  it('stops refreshing once stopped', async () => {
    const { get, events, stop } = await started();
    stop();

    events.emit(taskEvent({ kind: 'index', state: 'done', finished_at: 2 }));
    await settle();

    expect(get).toHaveBeenCalledTimes(1);
  });
});
