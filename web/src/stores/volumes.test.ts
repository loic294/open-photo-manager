import { effect } from '@preact/signals-core';
import { afterEach, describe, expect, it, type MockInstance, vi } from 'vitest';
import { ApiError } from '@/api/errors';
import type { ServerEvent } from '@/api/generated/ServerEvent';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TaskKind } from '@/api/generated/TaskKind';
import type { VolumeDto } from '@/api/generated/VolumeDto';
import { SERVER_RETRY_MS } from '@/api/sse';
import { FakeEventSource } from '@/test/fake-event-source';

type Volumes = typeof import('@/stores/volumes');
type Get = MockInstance<typeof import('@/api/client')['api']['get']>;

interface Loaded {
  store: Volumes;
  get: Get;
  events: typeof import('@/api/sse')['events'];
}

const photos: VolumeDto = { id: 1, name: 'photos', writable: true, online: true };
const backup: VolumeDto = { id: 2, name: 'backup', writable: false, online: true };

let current: Loaded | undefined;

async function setup(): Promise<Loaded> {
  vi.resetModules();
  const [store, client, sse] = await Promise.all([
    import('@/stores/volumes'),
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

// An index task by default; override what a test cares about.
function taskEvent(overrides: Partial<TaskDto> = {}): ServerEvent {
  return {
    type: 'task',
    task: {
      id: 1,
      kind: 'index',
      title: 'Index photos',
      state: 'running',
      items_done: 0,
      items_total: 100,
      bytes_done: 0,
      bytes_total: null,
      speed: null,
      eta_secs: null,
      error: null,
      started_at: 1,
      finished_at: null,
      ...overrides,
    },
  };
}

// Lets a load that was just started, and the reload it may have coalesced, finish.
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

afterEach(() => {
  current?.events.stop();
  current = undefined;
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  FakeEventSource.reset();
});

describe('loadVolumes', () => {
  it('starts idle and empty, without fetching', async () => {
    const { store, get } = await setup();

    expect(store.volumesStatus.value).toBe('idle');
    expect(store.volumes.value).toEqual([]);
    expect(store.volumesError.value).toBeUndefined();
    expect(get).not.toHaveBeenCalled();
  });

  it('fetches GET /api/volumes and keeps the list in server order', async () => {
    const { store, get } = await setup();
    get.mockResolvedValue([photos, backup]);

    await store.loadVolumes();

    expect(get).toHaveBeenCalledTimes(1);
    expect(get.mock.calls[0]?.[0]).toBe('/api/volumes');
    expect(store.volumes.value).toEqual([photos, backup]);
    expect(store.volumesStatus.value).toBe('ready');
    expect(store.volumesError.value).toBeUndefined();
  });

  it('is loading during the first load', async () => {
    const { store, get } = await setup();
    const request = deferred<VolumeDto[]>();
    get.mockReturnValueOnce(request.promise);

    const loading = store.loadVolumes();
    expect(store.volumesStatus.value).toBe('loading');

    request.resolve([photos]);
    await loading;
    expect(store.volumesStatus.value).toBe('ready');
  });

  it('stays ready while it refreshes, so the list never flickers away', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce([photos]);
    await store.loadVolumes();
    const request = deferred<VolumeDto[]>();
    get.mockReturnValueOnce(request.promise);

    const refreshing = store.loadVolumes();

    expect(store.volumesStatus.value).toBe('ready');
    expect(store.volumes.value).toEqual([photos]);
    request.resolve([photos, backup]);
    await refreshing;
    expect(store.volumes.value).toEqual([photos, backup]);
  });

  it('runs one load at a time, and one more afterwards so the result is never older than the call', async () => {
    const { store, get } = await setup();
    const early = deferred<VolumeDto[]>();
    const late = deferred<VolumeDto[]>();
    get.mockReturnValueOnce(early.promise).mockReturnValueOnce(late.promise);

    const a = store.loadVolumes();
    const b = store.loadVolumes();
    const c = store.loadVolumes();
    expect(get).toHaveBeenCalledTimes(1);

    early.resolve([photos]);
    await vi.waitFor(() => expect(get).toHaveBeenCalledTimes(2));
    expect(store.volumes.value).toEqual([photos]);

    late.resolve([photos, backup]);
    await Promise.all([a, b, c]);

    expect(get).toHaveBeenCalledTimes(2);
    expect(store.volumes.value).toEqual([photos, backup]);
  });

  it('can be called again after a load finished', async () => {
    const { store, get } = await setup();
    get.mockResolvedValue([photos]);

    await store.loadVolumes();
    await store.loadVolumes();

    expect(get).toHaveBeenCalledTimes(2);
  });

  it('never rejects: a failed first load is reported through the store', async () => {
    const { store, get } = await setup();
    get.mockRejectedValueOnce(boom('Volumes are unavailable'));

    await expect(store.loadVolumes()).resolves.toBeUndefined();

    expect(store.volumesStatus.value).toBe('error');
    expect(store.volumesError.value).toBe('Volumes are unavailable');
    expect(store.volumes.value).toEqual([]);
  });

  it('retries after a failed first load and clears the error', async () => {
    const { store, get } = await setup();
    get.mockRejectedValueOnce(boom()).mockResolvedValueOnce([photos]);

    await store.loadVolumes();
    const retry = store.loadVolumes();
    expect(store.volumesStatus.value).toBe('loading');
    await retry;

    expect(store.volumesStatus.value).toBe('ready');
    expect(store.volumesError.value).toBeUndefined();
    expect(store.volumes.value).toEqual([photos]);
  });

  it('keeps the old list when a refresh fails, and says why', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce([photos, backup]).mockRejectedValueOnce(boom('offline'));

    await store.loadVolumes();
    await store.loadVolumes();

    expect(store.volumesStatus.value).toBe('ready');
    expect(store.volumes.value).toEqual([photos, backup]);
    expect(store.volumesError.value).toBe('offline');

    get.mockResolvedValueOnce([photos, backup]);
    await store.loadVolumes();
    expect(store.volumesError.value).toBeUndefined();
  });

  it('uses a generic message when the failure carries none', async () => {
    const { store, get } = await setup();
    get.mockRejectedValueOnce(undefined);

    await store.loadVolumes();

    expect(store.volumesError.value).toBe('Could not load the volumes');
  });

  it('treats a payload that is not a list as a failure', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce({ volumes: [] });

    await store.loadVolumes();

    expect(store.volumesStatus.value).toBe('error');
    expect(store.volumesError.value).toContain('Unexpected response');
    expect(store.volumes.value).toEqual([]);
  });

  it('accepts a server with no volumes', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce([]);

    await store.loadVolumes();

    expect(store.volumesStatus.value).toBe('ready');
    expect(store.volumes.value).toEqual([]);
  });

  it('keeps the very same list when a refresh brings nothing new', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce([photos, backup]);
    await store.loadVolumes();
    const before = store.volumes.value;
    const runs = vi.fn();
    const stop = effect(() => {
      runs(store.volumes.value);
    });

    get.mockResolvedValueOnce([{ ...photos }, { ...backup }]);
    await store.loadVolumes();
    stop();

    expect(store.volumes.value).toBe(before);
    expect(runs).toHaveBeenCalledTimes(1);
  });

  it.each([
    ['goes offline', { ...photos, online: false }],
    ['is renamed', { ...photos, name: 'pictures' }],
    ['becomes read-only', { ...photos, writable: false }],
  ])('replaces the list when a volume %s', async (_what, changed) => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce([photos]).mockResolvedValueOnce([changed]);

    await store.loadVolumes();
    await store.loadVolumes();

    expect(store.volumes.value).toEqual([changed]);
  });

  it('replaces the list when a volume appears or disappears', async () => {
    const { store, get } = await setup();
    get
      .mockResolvedValueOnce([photos])
      .mockResolvedValueOnce([photos, backup])
      .mockResolvedValueOnce([backup]);

    await store.loadVolumes();
    await store.loadVolumes();
    expect(store.volumes.value).toEqual([photos, backup]);
    await store.loadVolumes();
    expect(store.volumes.value).toEqual([backup]);
  });
});

describe('lookups', () => {
  it('finds a volume by name, and its id', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce([photos, backup]);
    await store.loadVolumes();

    expect(store.volumeByName('backup')).toEqual(backup);
    expect(store.volumeIdByName('photos')).toBe(1);
    expect(store.volumeIdByName('backup')).toBe(2);
  });

  it('finds nothing for an unknown name or no name', async () => {
    const { store, get } = await setup();
    get.mockResolvedValueOnce([photos]);
    await store.loadVolumes();

    expect(store.volumeByName('nope')).toBeUndefined();
    expect(store.volumeByName(null)).toBeUndefined();
    expect(store.volumeByName(undefined)).toBeUndefined();
    expect(store.volumeIdByName('nope')).toBeUndefined();
    expect(store.volumeIdByName(null)).toBeUndefined();
  });

  it('matches names exactly, without normalising them', async () => {
    const { store, get } = await setup();
    const nfd = 'Fe\u0301te';
    get.mockResolvedValueOnce([{ id: 9, name: nfd, writable: true, online: true }]);
    await store.loadVolumes();

    expect(store.volumeIdByName(nfd)).toBe(9);
    expect(store.volumeIdByName('F\u00e9te')).toBeUndefined();
  });

  it('is reactive: a render that looks a volume up updates when the list arrives', async () => {
    const { store, get } = await setup();
    const seen: Array<number | undefined> = [];
    const stop = effect(() => {
      seen.push(store.volumeIdByName('photos'));
    });
    get.mockResolvedValueOnce([photos]);

    await store.loadVolumes();
    stop();

    expect(seen).toEqual([undefined, 1]);
  });
});

describe('startVolumes', () => {
  // The store has loaded once, so `get` has been called exactly once.
  async function started() {
    const loaded = await setup();
    loaded.get.mockResolvedValue([photos]);
    const stop = loaded.store.startVolumes();
    await vi.waitFor(() => expect(loaded.store.volumesStatus.value).toBe('ready'));
    return { ...loaded, stop };
  }

  it('loads straight away', async () => {
    const { store, get } = await setup();
    get.mockResolvedValue([photos]);

    const stop = store.startVolumes();
    await vi.waitFor(() => expect(store.volumesStatus.value).toBe('ready'));
    stop();

    expect(get).toHaveBeenCalledTimes(1);
  });

  it('refreshes after the event stream reconnects, but not for the first connection', async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0);
    vi.stubGlobal('EventSource', FakeEventSource);
    const { store, get, events } = await setup();
    get.mockResolvedValue([photos]);
    const stop = store.startVolumes();
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

  it('refreshes when an index task appears and every time its state changes', async () => {
    const { get, events, stop } = await started();

    events.emit(taskEvent({ state: 'queued' }));
    await settle();
    expect(get).toHaveBeenCalledTimes(2);

    events.emit(taskEvent({ state: 'running' }));
    await settle();
    expect(get).toHaveBeenCalledTimes(3);

    events.emit(taskEvent({ state: 'done', finished_at: 2 }));
    await settle();
    stop();

    expect(get).toHaveBeenCalledTimes(4);
  });

  it.each(['queued', 'running'] as const)(
    'does not refresh for progress-only updates of a %s index task',
    async (state) => {
      const { get, events, stop } = await started();
      events.emit(taskEvent({ state }));
      await settle();
      expect(get).toHaveBeenCalledTimes(2);

      for (const itemsDone of [1, 2, 3]) {
        events.emit(taskEvent({ state, items_done: itemsDone, speed: 10, eta_secs: 5 }));
      }
      await settle();
      stop();

      expect(get).toHaveBeenCalledTimes(2);
    },
  );

  it('tracks the state of each index task by its id', async () => {
    const { get, events, stop } = await started();

    events.emit(taskEvent({ id: 1, state: 'running' }));
    await settle();
    events.emit(taskEvent({ id: 2, state: 'running' }));
    await settle();
    expect(get).toHaveBeenCalledTimes(3);

    events.emit(taskEvent({ id: 1, state: 'running', items_done: 5 }));
    events.emit(taskEvent({ id: 2, state: 'running', items_done: 7 }));
    await settle();
    expect(get).toHaveBeenCalledTimes(3);

    events.emit(taskEvent({ id: 1, state: 'done' }));
    await settle();
    expect(get).toHaveBeenCalledTimes(4);

    events.emit(taskEvent({ id: 2, state: 'running', items_done: 9 }));
    await settle();
    stop();

    expect(get).toHaveBeenCalledTimes(4);
  });

  it.each(['done', 'failed', 'cancelled', 'interrupted'] as const)(
    'forgets an index task once it is %s, so the ids cannot pile up',
    async (state) => {
      const { get, events, stop } = await started();
      events.emit(taskEvent({ state: 'running' }));
      await settle();
      events.emit(taskEvent({ state }));
      await settle();
      expect(get).toHaveBeenCalledTimes(3);

      // A forgotten id is a new one: its next event counts as a first sighting.
      events.emit(taskEvent({ state }));
      await settle();
      stop();

      expect(get).toHaveBeenCalledTimes(4);
    },
  );

  it('ignores events that have nothing to do with volumes', async () => {
    const { get, events, stop } = await started();
    const otherKinds: readonly TaskKind[] = [
      'fs_move',
      'fs_trash',
      'fs_rmdir',
      'plan_execute',
      'immich_sync',
      'immich_cleanup',
      'trash_empty',
    ];

    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['2024'] });
    for (const kind of otherKinds) {
      events.emit(taskEvent({ kind, state: 'queued' }));
      events.emit(taskEvent({ kind, state: 'running' }));
      events.emit(taskEvent({ kind, state: 'done' }));
    }
    await settle();
    stop();

    expect(get).toHaveBeenCalledTimes(1);
  });

  it('stops refreshing once the returned function was called', async () => {
    vi.useFakeTimers();
    vi.spyOn(Math, 'random').mockReturnValue(0);
    vi.stubGlobal('EventSource', FakeEventSource);
    const { store, get, events } = await setup();
    get.mockResolvedValue([photos]);
    const stop = store.startVolumes();
    await vi.waitFor(() => expect(store.volumesStatus.value).toBe('ready'));
    events.start();
    FakeEventSource.last().open();
    events.emit(taskEvent({ state: 'running' }));
    await vi.waitFor(() => expect(get).toHaveBeenCalledTimes(2));

    stop();
    events.emit(taskEvent({ state: 'done' }));
    FakeEventSource.last().error();
    vi.advanceTimersByTime(SERVER_RETRY_MS * 2);
    FakeEventSource.last().open();
    await Promise.resolve();

    expect(get).toHaveBeenCalledTimes(2);
  });
});
