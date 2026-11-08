import type { ReactiveController, ReactiveControllerHost } from 'lit';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ApiClient, RequestOptions } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FolderEntry } from '@/api/generated/FolderEntry';
import type { FoldersResponse } from '@/api/generated/FoldersResponse';
import type { TaskDto } from '@/api/generated/TaskDto';
import { createEventsClient, type EventsClient } from '@/api/sse';
import { FoldersController, type FoldersControllerOptions } from '@/controllers/folders-controller';
import { INVALIDATE_DEBOUNCE_MS, INVALIDATE_MAX_WAIT_MS } from '@/stores/paged-items';
import { folderEntry } from '@/test/folder-entry';
import { EMPTY_FILTER } from '@/util/filter';

function request(overrides: Partial<BrowseRequest> = {}): BrowseRequest {
  return { volume_id: 1, dir: 'a/b', filter: EMPTY_FILTER, plan: null, ...overrides };
}

const ROOT = request({ volume_id: null, dir: '' });
const PLAN_1 = { plan_id: 1, side: 'comparison' } as const;

function answer(...folders: FolderEntry[]): FoldersResponse {
  const total = folders.reduce((sum, folder) => sum + folder.count, 0);
  return { folders, counts: { in_folder: 5, in_subfolders: total } };
}

interface Call {
  path: string;
  body: BrowseRequest;
  signal: AbortSignal | undefined;
  settled: boolean;
  resolve(response: FoldersResponse): void;
  reject(error: unknown): void;
}

/** A server whose answers the test delivers by hand. */
function createServer(options: { honourAbort?: boolean } = {}) {
  const calls: Call[] = [];
  const honourAbort = options.honourAbort ?? true;
  const client: Pick<ApiClient, 'post'> = {
    post<T>(path: string, requestOptions: RequestOptions = {}): Promise<T> {
      return new Promise<T>((resolve, reject) => {
        const call: Call = {
          path,
          body: requestOptions.body as BrowseRequest,
          signal: requestOptions.signal,
          settled: false,
          resolve: (response) => {
            call.settled = true;
            resolve(response as T);
          },
          reject: (error) => {
            call.settled = true;
            reject(error);
          },
        };
        if (honourAbort) {
          requestOptions.signal?.addEventListener('abort', () => {
            if (!call.settled) call.reject(new DOMException('Aborted', 'AbortError'));
          });
        }
        calls.push(call);
      });
    },
  };
  return {
    client,
    calls,
    last(): Call {
      const call = calls.at(-1);
      if (!call) throw new Error('no request was sent');
      return call;
    },
  };
}

/** An events bus that counts its subscriptions and can fake a resync. */
function createBus() {
  const real = createEventsClient();
  const resync = new Set<() => void>();
  let typed = 0;
  const events: Pick<EventsClient, 'on' | 'onResync'> = {
    on: ((type, handler) => {
      typed += 1;
      const stop = real.on(type, handler);
      return () => {
        typed -= 1;
        stop();
      };
    }) as EventsClient['on'],
    onResync: (handler) => {
      resync.add(handler);
      return () => {
        resync.delete(handler);
      };
    },
  };
  return {
    events,
    subscriptions: () => typed + resync.size,
    fsChange: (volume_id: number, ...dirs: string[]) =>
      real.emit({ type: 'fs_change', volume_id, dirs }),
    task: (kind: TaskDto['kind'], state: TaskDto['state']) =>
      real.emit({ type: 'task', task: taskDto(kind, state) }),
    resync: () => {
      for (const handler of [...resync]) handler();
    },
  };
}

function taskDto(kind: TaskDto['kind'], state: TaskDto['state']): TaskDto {
  return {
    id: 1,
    kind,
    title: 'task',
    state,
    items_done: 0,
    items_total: null,
    bytes_done: 0,
    bytes_total: null,
    speed: null,
    eta_secs: null,
    error: null,
    started_at: null,
    finished_at: null,
  };
}

class FakeHost implements ReactiveControllerHost {
  readonly controllers: ReactiveController[] = [];
  readonly updateComplete = Promise.resolve(true);
  updates = 0;

  addController(controller: ReactiveController): void {
    this.controllers.push(controller);
  }

  removeController(controller: ReactiveController): void {
    this.controllers.splice(this.controllers.indexOf(controller), 1);
  }

  requestUpdate(): void {
    this.updates += 1;
  }
}

/** Lets the promises behind a delivered answer settle. */
async function tick() {
  for (let i = 0; i < 6; i++) await Promise.resolve();
}

const controllers: FoldersController[] = [];

function setup(options: FoldersControllerOptions = {}, config: { honourAbort?: boolean } = {}) {
  const host = new FakeHost();
  const server = createServer(config);
  const bus = createBus();
  const controller = new FoldersController(host, {
    client: server.client,
    events: bus.events,
    ...options,
  });
  controllers.push(controller);
  controller.hostConnected();
  return { host, server, bus, controller };
}

/** Loads a request and delivers its answer. */
async function loaded(
  context: ReturnType<typeof setup>,
  req: BrowseRequest = request(),
  response: FoldersResponse = answer(folderEntry({ name: 'trip', count: 3 })),
) {
  context.controller.setRequest(req);
  context.server.last().resolve(response);
  await tick();
}

afterEach(() => {
  for (const controller of controllers) controller.hostDisconnected();
  controllers.length = 0;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('registration', () => {
  it('registers itself with the host', () => {
    const { host, controller } = setup();
    expect(host.controllers).toEqual([controller]);
  });

  it('is idle and empty before it has a request', () => {
    const { controller, server } = setup();
    expect(controller.status).toBe('idle');
    expect(controller.folders).toEqual([]);
    expect(controller.counts).toBeNull();
    expect(controller.error).toBeNull();
    expect(controller.refreshing).toBe(false);
    expect(controller.missing).toBe(false);
    expect(controller.planMissing).toBe(false);
    expect(server.calls).toHaveLength(0);
  });

  it('returns the same empty array every time so a host does not see a change', () => {
    const { controller } = setup();
    expect(controller.folders).toBe(controller.folders);
  });
});

describe('loading a listing', () => {
  it('asks the folders endpoint for exactly the request fields', async () => {
    const { controller, server } = setup();
    const extra = { ...request({ plan: PLAN_1 }), view: 'tree' };
    controller.setRequest(extra);
    expect(server.calls).toHaveLength(1);
    expect(server.last().path).toBe('/api/browse/folders');
    expect(server.last().body).toEqual({
      volume_id: 1,
      dir: 'a/b',
      filter: EMPTY_FILTER,
      plan: PLAN_1,
    });
    expect(Object.keys(server.last().body).sort()).toEqual(['dir', 'filter', 'plan', 'volume_id']);
  });

  it('is loading until the answer arrives, then ready with the folders and the counts', async () => {
    const { controller, server, host } = setup();
    controller.setRequest(request());
    expect(controller.status).toBe('loading');
    expect(controller.folders).toEqual([]);
    expect(controller.counts).toBeNull();
    expect(controller.refreshing).toBe(false);
    const before = host.updates;

    const trip = folderEntry({ name: 'trip', count: 3 });
    server.last().resolve({ folders: [trip], counts: { in_folder: 7, in_subfolders: 3 } });
    await tick();

    expect(controller.status).toBe('ready');
    expect(controller.folders).toEqual([trip]);
    expect(controller.counts).toEqual({ in_folder: 7, in_subfolders: 3 });
    expect(controller.refreshing).toBe(false);
    expect(host.updates).toBeGreaterThan(before);
  });

  it('asks the host to render when the request changes and when the answer arrives', async () => {
    const { controller, server, host } = setup();
    controller.setRequest(request());
    expect(host.updates).toBeGreaterThan(0);
    const afterRequest = host.updates;
    server.last().resolve(answer());
    await tick();
    expect(host.updates).toBeGreaterThan(afterRequest);
  });

  it('does nothing for a request it already has, however its keys are ordered', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server, host } = context;
    const updates = host.updates;

    controller.setRequest(request());
    controller.setRequest({
      plan: null,
      filter: { selection_id: null, tags: [], flags: [], ratings: [], cameras: [] },
      dir: 'a/b',
      volume_id: 1,
    });

    expect(server.calls).toHaveLength(1);
    expect(controller.status).toBe('ready');
    expect(controller.folders).toHaveLength(1);
    expect(host.updates).toBe(updates);
  });

  it('works for the volumes root: no volume, no directory, the volumes as folders', async () => {
    const { controller, server } = setup();
    controller.setRequest(ROOT);
    expect(server.last().body).toEqual({
      volume_id: null,
      dir: '',
      filter: EMPTY_FILTER,
      plan: null,
    });

    const photos = folderEntry({ name: 'photos', path: '', volume_id: 2, count: 40 });
    server.last().resolve({ folders: [photos], counts: { in_folder: 0, in_subfolders: 40 } });
    await tick();

    expect(controller.status).toBe('ready');
    expect(controller.folders).toEqual([photos]);
    expect(controller.counts).toEqual({ in_folder: 0, in_subfolders: 40 });
  });

  it('does not fetch while the host is not connected, and fetches once it is', () => {
    const host = new FakeHost();
    const server = createServer();
    const bus = createBus();
    const controller = new FoldersController(host, { client: server.client, events: bus.events });
    controllers.push(controller);

    controller.setRequest(request());
    expect(server.calls).toHaveLength(0);
    expect(controller.status).toBe('loading');

    controller.hostConnected();
    expect(server.calls).toHaveLength(1);
  });
});

describe('changing the listing', () => {
  it('aborts the request in flight and drops what was shown', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server } = context;

    controller.setRequest(request({ dir: 'a/c' }));
    expect(server.calls).toHaveLength(2);
    expect(controller.status).toBe('loading');
    expect(controller.folders).toEqual([]);
    expect(controller.counts).toBeNull();
    expect(controller.refreshing).toBe(false);

    controller.setRequest(request({ dir: 'a/d' }));
    expect(server.calls[1]?.signal?.aborted).toBe(true);
    expect(server.calls[2]?.signal?.aborted).toBe(false);
  });

  it.each([
    ['another directory', request({ dir: 'a/c' })],
    ['another volume', request({ volume_id: 2 })],
    ['another filter', request({ filter: { ...EMPTY_FILTER, ratings: [5] } })],
  ])('treats %s as a new listing', async (_name, next) => {
    const context = setup();
    await loaded(context);
    context.controller.setRequest(next);
    expect(context.controller.status).toBe('loading');
    expect(context.server.calls).toHaveLength(2);
  });

  it('drops an answer that arrives after a newer request, even from a server that ignores aborts', async () => {
    const { controller, server } = setup({}, { honourAbort: false });
    controller.setRequest(request({ dir: 'a' }));
    controller.setRequest(request({ dir: 'b' }));
    const [first, second] = server.calls;
    if (!first || !second) throw new Error('two requests expected');

    second.resolve(answer(folderEntry({ name: 'from-b' })));
    await tick();
    first.resolve(answer(folderEntry({ name: 'from-a' })));
    await tick();

    expect(controller.folders.map((folder) => folder.name)).toEqual(['from-b']);
  });

  it('drops an answer for a listing that was left, and does not show it for the new one', async () => {
    const { controller, server } = setup({}, { honourAbort: false });
    controller.setRequest(request({ dir: 'a' }));
    controller.setRequest(request({ dir: 'b' }));
    server.calls[0]?.resolve(answer(folderEntry({ name: 'from-a' })));
    await tick();

    expect(controller.status).toBe('loading');
    expect(controller.folders).toEqual([]);
    expect(controller.counts).toBeNull();
  });

  it('drops an old failure too', async () => {
    const { controller, server } = setup({}, { honourAbort: false });
    controller.setRequest(request({ dir: 'a' }));
    controller.setRequest(request({ dir: 'b' }));
    server.calls[0]?.reject(new ApiError({ status: 500, code: 'internal', message: 'boom' }));
    await tick();
    expect(controller.status).toBe('loading');
    expect(controller.error).toBeNull();
  });

  it('forgets everything for a null request and stops the work', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server, bus } = context;
    controller.setRequest(request({ dir: 'x' }));
    const pending = server.last();
    bus.fsChange(1, 'x');

    controller.setRequest(null);
    expect(pending.signal?.aborted).toBe(true);
    expect(controller.status).toBe('idle');
    expect(controller.folders).toEqual([]);
    expect(controller.counts).toBeNull();
    expect(controller.refreshing).toBe(false);
  });

  it('does not refetch after a null request cancelled a pending event', async () => {
    vi.useFakeTimers();
    const context = setup();
    await loaded(context);
    const { controller, server, bus } = context;
    bus.fsChange(1, 'a/b');
    controller.setRequest(null);
    vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
    expect(server.calls).toHaveLength(1);
  });
});

describe('changing only the plan', () => {
  const planned = answer(folderEntry({ name: 'trip', count: 3 }));
  const replanned = answer(folderEntry({ name: 'trip', count: 3, overlay: { planned: true } }));

  it('keeps the rows on screen and refetches over them', async () => {
    const context = setup();
    await loaded(context, request({ plan: PLAN_1 }), planned);
    const { controller, server } = context;

    controller.setRequest(request({ plan: { plan_id: 2, side: 'comparison' } }));
    expect(server.calls).toHaveLength(2);
    expect(server.last().body.plan).toEqual({ plan_id: 2, side: 'comparison' });
    expect(controller.status).toBe('ready');
    expect(controller.refreshing).toBe(true);
    expect(controller.folders).toEqual(planned.folders);
    expect(controller.counts).toEqual(planned.counts);

    server.last().resolve(replanned);
    await tick();
    expect(controller.refreshing).toBe(false);
    expect(controller.folders).toEqual(replanned.folders);
  });

  it('refetches when the plan appears, goes away or changes side', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server } = context;
    controller.setRequest(request({ plan: PLAN_1 }));
    expect(server.calls).toHaveLength(2);
    controller.setRequest(request({ plan: { plan_id: 1, side: 'source' } }));
    expect(server.calls).toHaveLength(3);
    controller.setRequest(request());
    expect(server.calls).toHaveLength(4);
    expect(controller.status).toBe('ready');
  });

  it('forgets that the old plan was gone', async () => {
    const context = setup();
    await loaded(context, request({ plan: PLAN_1 }));
    const { controller, server } = context;
    controller.invalidate();
    server.last().reject(new ApiError({ status: 404, code: 'plan_not_found', message: 'gone' }));
    await tick();
    expect(controller.planMissing).toBe(true);

    controller.setRequest(request({ plan: { plan_id: 2, side: 'comparison' } }));
    expect(controller.planMissing).toBe(false);
  });
});

describe('refreshing', () => {
  it('keeps the old data while a refetch runs and swaps it when the answer is in', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server, host } = context;
    const before = controller.folders;

    controller.invalidate();
    expect(server.calls).toHaveLength(2);
    expect(controller.status).toBe('ready');
    expect(controller.refreshing).toBe(true);
    expect(controller.folders).toBe(before);
    expect(controller.counts).not.toBeNull();
    const updates = host.updates;

    const next = answer(folderEntry({ name: 'trip', count: 9 }), folderEntry({ name: 'new' }));
    server.last().resolve(next);
    await tick();
    expect(controller.refreshing).toBe(false);
    expect(controller.folders).toEqual(next.folders);
    expect(host.updates).toBeGreaterThan(updates);
  });

  it('replaces a refetch that is still running with a newer one', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server } = context;
    controller.invalidate();
    const first = server.last();
    controller.invalidate();
    expect(first.signal?.aborted).toBe(true);
    expect(server.calls).toHaveLength(3);

    server.last().resolve(answer(folderEntry({ name: 'newest' })));
    await tick();
    expect(controller.folders.map((folder) => folder.name)).toEqual(['newest']);
    expect(controller.refreshing).toBe(false);
  });

  it('does not treat the abort of a replaced refetch as a failure', async () => {
    const context = setup();
    await loaded(context);
    const { controller } = context;
    controller.invalidate();
    controller.invalidate();
    await tick();
    expect(controller.status).toBe('ready');
    expect(controller.error).toBeNull();
    expect(controller.refreshing).toBe(true);
  });

  it('ends the refresh quietly when the request is cancelled from outside', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server } = context;
    controller.invalidate();
    server.last().reject(new DOMException('Aborted', 'AbortError'));
    await tick();
    expect(controller.status).toBe('ready');
    expect(controller.error).toBeNull();
    expect(controller.refreshing).toBe(false);
    expect(controller.folders).toHaveLength(1);
  });
});

describe('fs_change', () => {
  beforeEachFakeTimers();

  it.each([
    ['the directory itself', request({ dir: 'a/b' }), 1, ['a/b']],
    ['a directory below it', request({ dir: 'a/b' }), 1, ['a/b/c/d']],
    ['one of several dirs', request({ dir: 'a/b' }), 1, ['x', 'a/b/c']],
    ['any dir, for the volume root', request({ dir: '' }), 1, ['anywhere/deep']],
    ['any volume, for the volumes root', ROOT, 7, ['anywhere']],
    ['any dir and volume, with a plan', request({ dir: 'a/b', plan: PLAN_1 }), 9, ['elsewhere']],
  ])('refetches for %s', async (_name, req, volume, dirs) => {
    const context = setup();
    await loaded(context, req);
    context.bus.fsChange(volume, ...dirs);
    vi.advanceTimersByTime(INVALIDATE_DEBOUNCE_MS);
    expect(context.server.calls).toHaveLength(2);
  });

  it.each([
    ['another volume', request({ dir: 'a/b' }), 2, ['a/b']],
    ['a sibling directory', request({ dir: 'a/b' }), 1, ['a/c']],
    ['a parent directory', request({ dir: 'a/b' }), 1, ['a']],
    ['a directory that only shares the prefix', request({ dir: 'a/b' }), 1, ['a/bc']],
    ['a case variant', request({ dir: 'a/b' }), 1, ['A/B']],
    ['no directory at all', request({ dir: 'a/b' }), 1, []],
  ])('ignores %s', async (_name, req, volume, dirs) => {
    const context = setup();
    await loaded(context, req);
    context.bus.fsChange(volume, ...dirs);
    vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
    expect(context.server.calls).toHaveLength(1);
  });

  it('does nothing without a request', () => {
    const { bus, server } = setup();
    bus.fsChange(1, 'a/b');
    vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
    expect(server.calls).toHaveLength(0);
  });

  it('waits for the burst to end: one refetch after the debounce time', async () => {
    const context = setup();
    await loaded(context);
    const { bus, server } = context;
    bus.fsChange(1, 'a/b');
    vi.advanceTimersByTime(INVALIDATE_DEBOUNCE_MS - 1);
    bus.fsChange(1, 'a/b/c');
    vi.advanceTimersByTime(INVALIDATE_DEBOUNCE_MS - 1);
    bus.fsChange(1, 'a/b/d');
    expect(server.calls).toHaveLength(1);

    vi.advanceTimersByTime(INVALIDATE_DEBOUNCE_MS - 1);
    expect(server.calls).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(server.calls).toHaveLength(2);
    vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
    expect(server.calls).toHaveLength(2);
  });

  it('refetches at the latest after the maximum wait, however long the burst lasts', async () => {
    const context = setup();
    await loaded(context);
    const { bus, server } = context;
    const step = INVALIDATE_DEBOUNCE_MS - 50;
    let elapsed = 0;
    while (elapsed + step < INVALIDATE_MAX_WAIT_MS) {
      bus.fsChange(1, 'a/b');
      vi.advanceTimersByTime(step);
      elapsed += step;
    }
    bus.fsChange(1, 'a/b');
    expect(server.calls).toHaveLength(1);

    vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS - elapsed - 1);
    expect(server.calls).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(server.calls).toHaveLength(2);
  });

  it('takes the debounce times from the options', async () => {
    const context = setup({ debounceMs: 10, maxWaitMs: 30 });
    await loaded(context);
    context.bus.fsChange(1, 'a/b');
    vi.advanceTimersByTime(9);
    expect(context.server.calls).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(context.server.calls).toHaveLength(2);
  });

  it('keeps the rows while the refetch runs', async () => {
    const context = setup();
    await loaded(context);
    context.bus.fsChange(1, 'a/b');
    vi.advanceTimersByTime(INVALIDATE_DEBOUNCE_MS);
    expect(context.controller.status).toBe('ready');
    expect(context.controller.refreshing).toBe(true);
    expect(context.controller.folders).toHaveLength(1);
  });
});

describe('resync and imports', () => {
  beforeEachFakeTimers();

  it('refetches at once after a resync, and drops a pending debounced refetch', async () => {
    const context = setup();
    await loaded(context);
    const { bus, server } = context;
    bus.fsChange(1, 'a/b');
    bus.resync();
    expect(server.calls).toHaveLength(2);
    vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
    expect(server.calls).toHaveLength(2);
  });

  it('ignores a resync without a request', () => {
    const { bus, server } = setup();
    bus.resync();
    expect(server.calls).toHaveLength(0);
  });

  it.each(['done', 'failed', 'cancelled', 'interrupted'] as const)(
    'refetches when an immich_sync task is %s',
    async (state) => {
      const context = setup();
      await loaded(context);
      context.bus.task('immich_sync', state);
      expect(context.server.calls).toHaveLength(1);
      vi.advanceTimersByTime(INVALIDATE_DEBOUNCE_MS);
      expect(context.server.calls).toHaveLength(2);
    },
  );

  it.each(['queued', 'running'] as const)(
    'ignores an immich_sync task that is %s',
    async (state) => {
      const context = setup();
      await loaded(context);
      context.bus.task('immich_sync', state);
      vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
      expect(context.server.calls).toHaveLength(1);
    },
  );

  it.each(['index', 'fs_move', 'plan_execute', 'immich_cleanup'] as const)(
    'ignores a finished %s task',
    async (kind) => {
      const context = setup();
      await loaded(context);
      context.bus.task(kind, 'done');
      vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
      expect(context.server.calls).toHaveLength(1);
    },
  );
});

describe('folder_not_found', () => {
  const notFound = () =>
    new ApiError({ status: 404, code: 'folder_not_found', message: 'not found' });

  it('is an answer, not an error: missing is set and nothing is shown', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server.last().reject(notFound());
    await tick();

    expect(controller.missing).toBe(true);
    expect(controller.status).toBe('ready');
    expect(controller.error).toBeNull();
    expect(controller.folders).toEqual([]);
    expect(controller.counts).toBeNull();
    expect(controller.refreshing).toBe(false);
  });

  it('drops the data of a folder that disappeared while it was shown', async () => {
    const context = setup();
    await loaded(context);
    context.controller.invalidate();
    context.server.last().reject(notFound());
    await tick();

    expect(context.controller.missing).toBe(true);
    expect(context.controller.folders).toEqual([]);
    expect(context.controller.counts).toBeNull();
  });

  it('is forgotten when the listing changes', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server.last().reject(notFound());
    await tick();

    controller.setRequest(request({ dir: 'a' }));
    expect(controller.missing).toBe(false);
    expect(controller.status).toBe('loading');
  });

  it('is forgotten after a successful answer', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server.last().reject(notFound());
    await tick();

    controller.invalidate();
    server.last().resolve(answer(folderEntry({ name: 'back' })));
    await tick();
    expect(controller.missing).toBe(false);
    expect(controller.folders).toHaveLength(1);
  });
});

describe('plan_not_found', () => {
  const gone = () => new ApiError({ status: 404, code: 'plan_not_found', message: 'gone' });

  it('keeps the old data, stays quiet and sets planMissing', async () => {
    const context = setup();
    await loaded(context, request({ plan: PLAN_1 }));
    const { controller, server } = context;
    const rows = controller.folders;

    controller.invalidate();
    server.last().reject(gone());
    await tick();

    expect(controller.planMissing).toBe(true);
    expect(controller.status).toBe('ready');
    expect(controller.error).toBeNull();
    expect(controller.folders).toBe(rows);
    expect(controller.counts).not.toBeNull();
    expect(controller.refreshing).toBe(false);
    expect(controller.missing).toBe(false);
  });

  it('keeps showing the loading state when there was nothing to show yet', async () => {
    const { controller, server } = setup();
    controller.setRequest(request({ plan: PLAN_1 }));
    server.last().reject(gone());
    await tick();

    expect(controller.planMissing).toBe(true);
    expect(controller.status).toBe('loading');
    expect(controller.error).toBeNull();
  });

  it('clears an earlier failure: the server did answer', async () => {
    const context = setup();
    await loaded(context, request({ plan: PLAN_1 }));
    const { controller, server } = context;
    controller.invalidate();
    server.last().reject(new ApiError({ status: 500, code: 'internal', message: 'boom' }));
    await tick();
    expect(controller.status).toBe('error');

    controller.invalidate();
    server.last().reject(gone());
    await tick();
    expect(controller.status).toBe('ready');
    expect(controller.planMissing).toBe(true);
  });

  it('is cleared by a successful answer', async () => {
    const context = setup();
    await loaded(context, request({ plan: PLAN_1 }));
    const { controller, server } = context;
    controller.invalidate();
    server.last().reject(gone());
    await tick();

    controller.invalidate();
    server.last().resolve(answer());
    await tick();
    expect(controller.planMissing).toBe(false);
  });
});

describe('errors and retry', () => {
  const boom = () => new ApiError({ status: 500, code: 'internal', message: 'Disk on fire' });

  it('turns any other failure into an error state with its message', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server.last().reject(boom());
    await tick();

    expect(controller.status).toBe('error');
    expect(controller.error).toBe('Disk on fire');
    expect(controller.folders).toEqual([]);
    expect(controller.counts).toBeNull();
    expect(controller.missing).toBe(false);
    expect(controller.planMissing).toBe(false);
  });

  it('reports a network failure', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server
      .last()
      .reject(new ApiError({ status: 0, code: 'network', message: 'Could not reach the server' }));
    await tick();
    expect(controller.status).toBe('error');
    expect(controller.error).toBe('Could not reach the server');
  });

  it('reports a failure that is not an ApiError', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server.last().reject(new TypeError('bad things'));
    await tick();
    expect(controller.status).toBe('error');
    expect(controller.error).toBe('bad things');
  });

  it('retry clears the message and loads again', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server.last().reject(boom());
    await tick();

    controller.retry();
    expect(controller.status).toBe('loading');
    expect(controller.error).toBeNull();
    expect(server.calls).toHaveLength(2);

    server.last().resolve(answer(folderEntry({ name: 'ok' })));
    await tick();
    expect(controller.status).toBe('ready');
    expect(controller.folders.map((folder) => folder.name)).toEqual(['ok']);
  });

  it('retry can fail again', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server.last().reject(boom());
    await tick();
    controller.retry();
    server.last().reject(boom());
    await tick();
    expect(controller.status).toBe('error');
    expect(controller.error).toBe('Disk on fire');
  });

  it('a failed refetch keeps the data it had, and retry shows it while loading again', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server } = context;
    const rows = controller.folders;

    controller.invalidate();
    server.last().reject(boom());
    await tick();
    expect(controller.status).toBe('error');
    expect(controller.error).toBe('Disk on fire');
    expect(controller.folders).toBe(rows);
    expect(controller.counts).not.toBeNull();

    controller.retry();
    expect(controller.status).toBe('ready');
    expect(controller.refreshing).toBe(true);
    expect(controller.error).toBeNull();
    expect(controller.folders).toBe(rows);
  });

  it('keeps the message while an automatic refetch runs, and clears it on success', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server } = context;
    controller.invalidate();
    server.last().reject(boom());
    await tick();

    controller.invalidate();
    expect(controller.status).toBe('error');
    server.last().resolve(answer(folderEntry({ name: 'fine' })));
    await tick();
    expect(controller.status).toBe('ready');
    expect(controller.error).toBeNull();
  });

  it('a new listing clears the message of the old one', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    server.last().reject(boom());
    await tick();
    controller.setRequest(request({ dir: 'other' }));
    expect(controller.status).toBe('loading');
    expect(controller.error).toBeNull();
  });

  it('retry without a request does nothing', () => {
    const { controller, server } = setup();
    controller.retry();
    expect(server.calls).toHaveLength(0);
    expect(controller.status).toBe('idle');
  });
});

describe('connection', () => {
  it('subscribes to fs_change, task and resync while connected, and to nothing after', () => {
    const { controller, bus } = setup();
    expect(bus.subscriptions()).toBe(3);
    controller.hostDisconnected();
    expect(bus.subscriptions()).toBe(0);
  });

  it('subscribes only once however often it is told it is connected', () => {
    const { controller, bus } = setup();
    controller.hostConnected();
    controller.hostConnected();
    expect(bus.subscriptions()).toBe(3);
  });

  it('aborts what is in flight and ignores events after it disconnected', async () => {
    vi.useFakeTimers();
    const context = setup();
    await loaded(context);
    const { controller, server, bus } = context;
    controller.invalidate();
    const pending = server.last();

    controller.hostDisconnected();
    expect(pending.signal?.aborted).toBe(true);
    await tick();
    expect(controller.status).toBe('ready');
    expect(controller.refreshing).toBe(false);

    bus.fsChange(1, 'a/b');
    bus.resync();
    vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
    expect(server.calls).toHaveLength(2);
  });

  it('cancels a pending debounced refetch when it disconnects', async () => {
    vi.useFakeTimers();
    const context = setup();
    await loaded(context);
    const { controller, server, bus } = context;
    bus.fsChange(1, 'a/b');
    controller.hostDisconnected();
    vi.advanceTimersByTime(INVALIDATE_MAX_WAIT_MS);
    expect(server.calls).toHaveLength(1);
  });

  it('does not fetch for a request set while disconnected', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server } = context;
    controller.hostDisconnected();
    controller.setRequest(request({ dir: 'elsewhere' }));
    expect(server.calls).toHaveLength(1);
    expect(controller.status).toBe('loading');
  });

  it('refetches once when it connects again, keeping the rows meanwhile', async () => {
    const context = setup();
    await loaded(context);
    const { controller, server } = context;
    const rows = controller.folders;
    controller.hostDisconnected();

    controller.hostConnected();
    expect(server.calls).toHaveLength(2);
    expect(controller.status).toBe('ready');
    expect(controller.refreshing).toBe(true);
    expect(controller.folders).toBe(rows);

    controller.setRequest(request());
    expect(server.calls).toHaveLength(2);
  });

  it('loads from scratch when it connects again before its first answer', async () => {
    const { controller, server } = setup();
    controller.setRequest(request());
    controller.hostDisconnected();
    expect(controller.status).toBe('loading');

    controller.hostConnected();
    expect(server.calls).toHaveLength(2);
    server.last().resolve(answer(folderEntry({ name: 'late' })));
    await tick();
    expect(controller.folders.map((folder) => folder.name)).toEqual(['late']);
  });
});

/** Fake timers for a whole `describe`, installed after the setup of each test. */
function beforeEachFakeTimers() {
  beforeEach(() => {
    vi.useFakeTimers();
  });
}
