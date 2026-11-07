import { effect } from '@preact/signals-core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ApiClient, RequestOptions } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FileItem } from '@/api/generated/FileItem';
import type { ItemsRequest } from '@/api/generated/ItemsRequest';
import type { ItemsResponse } from '@/api/generated/ItemsResponse';
import { createEventsClient, type EventsClient } from '@/api/sse';
import { PagedItems, type PagedItemsOptions } from '@/stores/paged-items';

const FRAME = 16;
const PAGE = 200;

const request: BrowseRequest = {
  volume_id: 1,
  dir: 'a/b',
  filter: { cameras: [], ratings: [], flags: [], tags: [], selection_id: null },
  plan: null,
};

/** Ids encode where an item came from: `generation * 1_000_000 + index`. */
function fileItem(index: number, generation: number): FileItem {
  return {
    id: generation * 1_000_000 + index,
    volume_id: 1,
    dir: 'a/b',
    name: `f${index}.jpg`,
    kind: 'image',
    size: 100,
    capture_ts: null,
    capture_src: null,
    camera: null,
    rating: 0,
    flag: 0,
    width: null,
    height: null,
    duration_ms: null,
    mtime: 1,
    in_immich: false,
    overlay: { matched: false, planned: false, will_delete: false },
  };
}

interface Call {
  offset: number;
  limit: number;
  body: ItemsRequest;
  signal: AbortSignal | undefined;
  settled: boolean;
  resolve(response: ItemsResponse): void;
  reject(error: unknown): void;
}

/** A server whose answers the test delivers by hand, from its current `total` and `generation`. */
function createServer(options: { total?: number; honourAbort?: boolean } = {}) {
  const calls: Call[] = [];
  const state = { total: options.total ?? 1000, generation: 0 };
  const honourAbort = options.honourAbort ?? true;

  const client: Pick<ApiClient, 'post'> = {
    post<T>(_path: string, requestOptions: RequestOptions = {}): Promise<T> {
      return new Promise<T>((resolve, reject) => {
        const body = requestOptions.body as ItemsRequest;
        const call: Call = {
          offset: body.offset,
          limit: body.limit,
          body,
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

  function find(target: number | Call): Call {
    const call =
      typeof target === 'number' ? calls.find((c) => c.offset === target && !c.settled) : target;
    if (!call) throw new Error(`no pending request for offset ${String(target)}`);
    return call;
  }

  return {
    client,
    calls,
    state,
    offsets: () => calls.map((call) => call.offset),
    pendingOffsets: () => calls.filter((call) => !call.settled).map((call) => call.offset),
    call: find,
    respond(target: number | Call) {
      const call = find(target);
      const end = Math.min(call.offset + call.limit, state.total);
      const items: FileItem[] = [];
      for (let index = call.offset; index < end; index++) {
        items.push(fileItem(index, state.generation));
      }
      call.resolve({ items, total: state.total, offset: call.offset });
    },
    fail(target: number | Call, error: unknown) {
      find(target).reject(error);
    },
  };
}
type Server = ReturnType<typeof createServer>;

/** An events bus that counts its subscriptions and can fake a resync. */
function createBus() {
  const real = createEventsClient();
  const resync = new Set<() => void>();
  let fsChange = 0;
  const events: Pick<EventsClient, 'on' | 'onResync'> = {
    on: ((type, handler) => {
      fsChange += 1;
      const stop = real.on(type, handler);
      return () => {
        fsChange -= 1;
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
    subscriptions: () => fsChange + resync.size,
    fsChange: (volume_id: number, ...dirs: string[]) =>
      real.emit({ type: 'fs_change', volume_id, dirs }),
    resync: () => {
      for (const handler of [...resync]) handler();
    },
  };
}

async function tick() {
  for (let i = 0; i < 6; i++) await Promise.resolve();
}

function frame() {
  vi.advanceTimersByTime(FRAME);
}

let server: Server;
let bus: ReturnType<typeof createBus>;
const created: PagedItems[] = [];
const stops: Array<() => void> = [];

function make(options: PagedItemsOptions = {}): PagedItems {
  const paged = new PagedItems({ client: server.client, events: bus.events, ...options });
  created.push(paged);
  return paged;
}

/** Starts a listing and delivers its first page. */
async function load(paged: PagedItems, req: BrowseRequest = request) {
  paged.reset(req, 'name', 'asc');
  server.respond(0);
  await tick();
}

/** Reads one index, lets the frame flush and delivers the page that holds it. */
async function loadPageOf(paged: PagedItems, index: number) {
  paged.get(index);
  frame();
  server.respond(Math.floor(index / PAGE) * PAGE);
  await tick();
}

/** Marks pages as on screen: read them, then let the frame flush. */
function show(paged: PagedItems, ...indexes: number[]) {
  for (const index of indexes) paged.get(index);
  frame();
}

function countRuns(read: () => unknown) {
  const state = { runs: 0 };
  stops.push(
    effect(() => {
      read();
      state.runs += 1;
    }),
  );
  return state;
}

beforeEach(() => {
  vi.useFakeTimers();
  server = createServer();
  bus = createBus();
});

afterEach(() => {
  for (const paged of created) paged.dispose();
  created.length = 0;
  for (const stop of stops) stop();
  stops.length = 0;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('PagedItems: the first page', () => {
  it('knows no total until the first page is in, and fetches it at once', async () => {
    const paged = make();
    expect(paged.total.value).toBeNull();
    expect(paged.loading.value).toBe(false);

    paged.reset(request, 'date', 'desc');

    expect(server.calls).toHaveLength(1);
    expect(server.calls[0]?.body).toEqual({
      volume_id: 1,
      dir: 'a/b',
      filter: request.filter,
      plan: null,
      sort: 'date',
      order: 'desc',
      offset: 0,
      limit: PAGE,
    });
    expect(paged.total.value).toBeNull();
    expect(paged.loading.value).toBe(true);
    expect(paged.get(0)).toBeUndefined();

    server.respond(0);
    await tick();

    expect(paged.total.value).toBe(1000);
    expect(paged.loading.value).toBe(false);
    expect(paged.error.value).toBeNull();
    expect(paged.get(0)?.id).toBe(0);
    expect(paged.get(199)?.id).toBe(199);
  });

  it('bumps version when a page arrives', async () => {
    const paged = make();
    const before = paged.version.value;
    await load(paged);
    expect(paged.version.value).toBeGreaterThan(before);
  });

  it('shows an empty listing as total 0', async () => {
    server.state.total = 0;
    const paged = make();
    await load(paged);
    expect(paged.total.value).toBe(0);
    expect(paged.get(0)).toBeUndefined();
    frame();
    expect(server.calls).toHaveLength(1);
  });

  it('does not fetch before reset', () => {
    const paged = make();
    paged.get(0);
    paged.invalidate();
    paged.retry();
    frame();
    expect(server.calls).toHaveLength(0);
  });
});

describe('PagedItems: reset', () => {
  it('does nothing when the request is the same by value, key order aside', async () => {
    const paged = make();
    await load(paged);
    const version = paged.version.value;

    const started = paged.reset(
      {
        plan: null,
        filter: { selection_id: null, tags: [], flags: [], ratings: [], cameras: [] },
        dir: 'a/b',
        volume_id: 1,
      },
      'name',
      'asc',
    );

    expect(started).toBe(false);
    expect(server.calls).toHaveLength(1);
    expect(paged.total.value).toBe(1000);
    expect(paged.version.value).toBe(version);
  });

  it.each([
    ['volume', { ...request, volume_id: 2 }, 'name', 'asc'],
    ['directory', { ...request, dir: 'a' }, 'name', 'asc'],
    ['filter', { ...request, filter: { ...request.filter, ratings: [5] } }, 'name', 'asc'],
    ['plan', { ...request, plan: { plan_id: 1, side: 'source' } }, 'name', 'asc'],
    ['sort key', request, 'size', 'asc'],
    ['sort order', request, 'name', 'desc'],
  ] as const)('starts over when the %s changes', async (_what, next, sort, order) => {
    const paged = make();
    await load(paged);
    const version = paged.version.value;

    const started = paged.reset(next as BrowseRequest, sort, order);

    expect(started).toBe(true);
    expect(paged.total.value).toBeNull();
    expect(paged.version.value).toBeGreaterThan(version);
    expect(server.calls).toHaveLength(2);
    expect(server.calls[1]?.offset).toBe(0);
    expect(server.calls[1]?.body.sort).toBe(sort);
    expect(server.calls[1]?.body.order).toBe(order);
    expect(paged.get(0)).toBeUndefined();
  });

  it('aborts what is in flight and drops the pages of the old listing', async () => {
    const paged = make();
    await load(paged);
    await loadPageOf(paged, 200);
    paged.get(400);
    frame();
    const slow = server.call(400);

    paged.reset({ ...request, dir: 'other' }, 'name', 'asc');

    expect(slow.signal?.aborted).toBe(true);
    expect(paged.error.value).toBeNull();
    server.respond(0);
    await tick();
    expect(paged.get(0)?.id).toBe(0);
    expect(paged.get(200)).toBeUndefined();
  });

  it('ignores the late answer of an old listing even when the transport ignores aborts', async () => {
    server = createServer({ honourAbort: false, total: 50 });
    const paged = make();
    paged.reset(request, 'name', 'asc');
    const old = server.call(0);

    paged.reset({ ...request, dir: 'other' }, 'name', 'asc');
    server.state.total = 77;
    server.respond(old);
    await tick();
    expect(paged.total.value).toBeNull();

    server.respond(server.calls[1] as Call);
    await tick();
    expect(paged.total.value).toBe(77);
  });

  it('clears an error', async () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    server.fail(0, new Error('boom'));
    await tick();
    expect(paged.error.value).toBe('boom');

    paged.reset({ ...request, dir: 'other' }, 'name', 'asc');
    expect(paged.error.value).toBeNull();
  });
});

describe('PagedItems: get', () => {
  it('returns undefined for a page that is not loaded, and fetches it on the next frame', async () => {
    const paged = make();
    await load(paged);

    expect(paged.get(250)).toBeUndefined();
    expect(server.calls).toHaveLength(1);

    frame();
    expect(server.calls).toHaveLength(2);
    expect(server.calls[1]?.offset).toBe(PAGE);
    expect(server.calls[1]?.limit).toBe(PAGE);

    server.respond(PAGE);
    await tick();
    expect(paged.get(250)?.id).toBe(250);
    expect(paged.get(200)?.id).toBe(200);
    expect(paged.get(399)?.id).toBe(399);
  });

  it('batches the reads of a frame: one request per page however many cells ask', async () => {
    const paged = make();
    await load(paged);

    for (let index = 200; index < 600; index++) paged.get(index);
    paged.get(5);
    expect(server.calls).toHaveLength(1);

    frame();
    expect(server.offsets()).toEqual([0, 200, 400]);
  });

  it('does not ask again for a page that is on its way', async () => {
    const paged = make();
    await load(paged);

    paged.get(200);
    frame();
    paged.get(201);
    paged.get(399);
    frame();
    frame();

    expect(server.offsets()).toEqual([0, 200]);
  });

  it('serves a cached page without touching the network', async () => {
    const paged = make();
    await load(paged);
    for (let round = 0; round < 3; round++) {
      for (let index = 0; index < PAGE; index++) paged.get(index);
      frame();
    }
    expect(server.calls).toHaveLength(1);
  });

  it('handles the short last page', async () => {
    server.state.total = 450;
    const paged = make();
    await load(paged);
    await loadPageOf(paged, 449);

    expect(server.call).toBeDefined();
    expect(server.calls[1]?.offset).toBe(400);
    expect(paged.get(449)?.id).toBe(449);
    expect(paged.get(450)).toBeUndefined();
  });

  it('ignores indexes outside the listing and does not fetch for them', async () => {
    const paged = make();
    await load(paged);

    expect(paged.get(-1)).toBeUndefined();
    expect(paged.get(1000)).toBeUndefined();
    expect(paged.get(5000)).toBeUndefined();
    expect(paged.get(1.5)).toBeUndefined();
    expect(paged.get(Number.NaN)).toBeUndefined();
    frame();

    expect(server.calls).toHaveLength(1);
  });

  it('schedules nothing while the total is unknown', () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    expect(paged.get(0)).toBeUndefined();
    expect(paged.get(400)).toBeUndefined();
    frame();
    expect(server.calls).toHaveLength(1);
  });

  it('never writes a signal, so it can run inside render', async () => {
    const paged = make();
    await load(paged);

    const watcher = countRuns(() => {
      paged.total.value;
      paged.version.value;
      paged.error.value;
      paged.loading.value;
      for (let index = 0; index < 600; index += 3) paged.get(index);
    });

    expect(watcher.runs).toBe(1);
    frame();
    // The flush starts requests (`loading`), which is not `get` writing during the render.
    expect(server.calls.length).toBeGreaterThan(1);
  });
});

describe('PagedItems: the cache', () => {
  it('keeps the most recently read pages and evicts the least recent', async () => {
    const paged = make({ maxPages: 3 });
    await load(paged);
    await loadPageOf(paged, 200);
    await loadPageOf(paged, 400);
    expect(paged.get(0)?.id).toBe(0);

    await loadPageOf(paged, 600);

    expect(paged.get(0)?.id).toBe(0);
    expect(paged.get(400)?.id).toBe(400);
    expect(paged.get(600)?.id).toBe(600);
    expect(paged.get(200)).toBeUndefined();
    frame();
    expect(server.calls.at(-1)?.offset).toBe(200);
  });

  it('never evicts the page that just arrived, and does not bump version for an eviction', async () => {
    const paged = make({ maxPages: 1 });
    await load(paged);
    const watcher = countRuns(() => paged.version.value);
    const runs = watcher.runs;

    await loadPageOf(paged, 200);

    expect(paged.get(200)?.id).toBe(200);
    expect(paged.get(0)).toBeUndefined();
    expect(watcher.runs).toBe(runs + 1);
  });

  it('refetches an evicted page when it is read again', async () => {
    const paged = make({ maxPages: 2 });
    await load(paged);
    await loadPageOf(paged, 200);
    await loadPageOf(paged, 400);
    const before = server.calls.length;

    paged.get(0);
    frame();

    expect(server.calls).toHaveLength(before + 1);
    expect(server.calls.at(-1)?.offset).toBe(0);
  });

  it('uses the page size it is given', async () => {
    const paged = make({ pageSize: 50 });
    paged.reset(request, 'name', 'asc');
    expect(server.calls[0]?.limit).toBe(50);
    server.respond(0);
    await tick();

    paged.get(120);
    frame();
    expect(server.calls[1]?.offset).toBe(100);
    expect(server.calls[1]?.limit).toBe(50);
  });

  it('falls back to the default page size for a nonsense value', () => {
    const paged = make({ pageSize: 0, maxPages: Number.NaN });
    paged.reset(request, 'name', 'asc');
    expect(server.calls[0]?.limit).toBe(PAGE);
  });
});

describe('PagedItems: pages that scroll away', () => {
  it('cancels the request of a page nobody reads any more', async () => {
    const paged = make();
    await load(paged);

    paged.get(200);
    frame();
    const first = server.call(200);
    expect(first.signal?.aborted).toBe(false);

    paged.get(600);
    frame();

    expect(first.signal?.aborted).toBe(true);
    expect(server.pendingOffsets()).toEqual([600]);
    expect(paged.error.value).toBeNull();
    expect(paged.loading.value).toBe(true);
  });

  it('leaves one request alive after a long fast scroll', async () => {
    server.state.total = 20_000;
    const paged = make();
    await load(paged);

    for (let page = 1; page <= 30; page++) {
      paged.get(page * PAGE);
      frame();
    }

    expect(server.calls).toHaveLength(31);
    expect(server.calls.filter((call) => !call.signal?.aborted)).toHaveLength(2);
    expect(server.pendingOffsets()).toEqual([30 * PAGE]);
  });

  it('fetches a cancelled page again when it is read again', async () => {
    const paged = make();
    await load(paged);
    paged.get(200);
    frame();
    paged.get(600);
    frame();

    paged.get(200);
    frame();

    expect(server.offsets()).toEqual([0, 200, 600, 200]);
    expect(paged.error.value).toBeNull();
  });

  it('keeps the requests of every page that is still on screen', async () => {
    const paged = make();
    await load(paged);

    show(paged, 200, 400);
    show(paged, 400, 600);

    expect(server.pendingOffsets()).toEqual([400, 600]);
  });
});

describe('PagedItems: loading', () => {
  it('is on while a page that is not cached is fetched', async () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    expect(paged.loading.value).toBe(true);
    server.respond(0);
    await tick();
    expect(paged.loading.value).toBe(false);

    paged.get(200);
    frame();
    expect(paged.loading.value).toBe(true);
    server.respond(200);
    await tick();
    expect(paged.loading.value).toBe(false);
  });

  it('is off while shown data is revalidated', async () => {
    const paged = make();
    await load(paged);
    show(paged, 0);

    paged.invalidate();

    expect(server.pendingOffsets()).toEqual([0]);
    expect(paged.loading.value).toBe(false);
  });

  it('turns off when a failed fetch settles', async () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    server.fail(0, new Error('boom'));
    await tick();
    expect(paged.loading.value).toBe(false);
  });
});

describe('PagedItems: failures', () => {
  it('reports the error, does not retry by itself, and retries on demand', async () => {
    const paged = make();
    await load(paged);
    paged.get(200);
    frame();

    server.fail(200, new ApiError({ status: 503, code: 'unavailable', message: 'Server busy' }));
    await tick();

    expect(paged.error.value).toBe('Server busy');
    expect(paged.loading.value).toBe(false);
    expect(paged.get(200)).toBeUndefined();
    frame();
    paged.get(201);
    frame();
    expect(server.calls).toHaveLength(2);

    paged.retry();
    expect(paged.error.value).toBeNull();
    expect(server.calls).toHaveLength(3);
    expect(server.calls[2]?.offset).toBe(200);

    server.respond(200);
    await tick();
    expect(paged.error.value).toBeNull();
    expect(paged.get(200)?.id).toBe(200);
  });

  it('retries a first page that never arrived', async () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    server.fail(0, new Error('offline'));
    await tick();
    expect(paged.error.value).toBe('offline');
    expect(paged.total.value).toBeNull();

    paged.retry();
    expect(server.calls).toHaveLength(2);
    server.respond(0);
    await tick();

    expect(paged.error.value).toBeNull();
    expect(paged.total.value).toBe(1000);
  });

  it('keeps the error when the retry fails too', async () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    server.fail(0, new Error('first'));
    await tick();
    paged.retry();
    server.fail(0, new Error('second'));
    await tick();
    expect(paged.error.value).toBe('second');
  });

  it('uses a generic message for an error without one', async () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    server.fail(0, { weird: true });
    await tick();
    expect(paged.error.value).toBe('Could not load the files');
  });

  it('does not clear the error of one page because another page arrived', async () => {
    const paged = make();
    await load(paged);
    show(paged, 200, 400);
    server.fail(200, new Error('page one failed'));
    server.respond(400);
    await tick();

    expect(paged.error.value).toBe('page one failed');
    expect(paged.get(400)?.id).toBe(400);
  });

  it('does not report a cancelled request as an error', async () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    paged.reset({ ...request, dir: 'other' }, 'name', 'asc');
    await tick();
    expect(paged.error.value).toBeNull();
  });

  it('does not retry a failed page that is no longer on screen', async () => {
    const paged = make();
    await load(paged);
    show(paged, 200);
    server.fail(200, new Error('nope'));
    await tick();
    show(paged, 600);
    const before = server.calls.length;

    paged.retry();

    expect(paged.error.value).toBeNull();
    expect(server.calls).toHaveLength(before);
  });
});

describe('PagedItems: invalidate', () => {
  it('keeps showing the old items and swaps in the new ones in one version change', async () => {
    const paged = make();
    await load(paged);
    await loadPageOf(paged, 200);
    show(paged, 0, 200);
    const watcher = countRuns(() => paged.version.value);
    const runs = watcher.runs;

    server.state.generation = 1;
    paged.invalidate();

    expect(server.pendingOffsets()).toEqual([0, 200]);
    expect(paged.get(0)?.id).toBe(0);
    expect(paged.get(200)?.id).toBe(200);

    server.respond(0);
    await tick();
    expect(paged.get(0)?.id).toBe(0);
    expect(watcher.runs).toBe(runs);

    server.respond(200);
    await tick();
    expect(paged.get(0)?.id).toBe(1_000_000);
    expect(paged.get(200)?.id).toBe(1_000_200);
    expect(watcher.runs).toBe(runs + 1);
  });

  it('never shows an empty frame while it refetches', async () => {
    const paged = make();
    await load(paged);
    show(paged, 0);
    const frames: Array<number | undefined> = [];

    paged.invalidate();
    frames.push(paged.get(0)?.id);
    server.state.generation = 1;
    server.respond(0);
    await tick();
    frames.push(paged.get(0)?.id);

    expect(frames).toEqual([0, 1_000_000]);
    expect(paged.total.value).toBe(1000);
  });

  it('refetches the pages that are on screen, not the others, until they are read', async () => {
    const paged = make();
    await load(paged);
    await loadPageOf(paged, 600);
    show(paged, 600);

    paged.invalidate();
    expect(server.pendingOffsets()).toEqual([600]);

    server.state.generation = 1;
    server.respond(600);
    await tick();
    expect(paged.get(600)?.id).toBe(1_000_600);

    // Page 0 was cached but not on screen: still old, refetched when read, old items meanwhile.
    expect(server.pendingOffsets()).toEqual([]);
    expect(paged.get(0)?.id).toBe(0);
    frame();
    expect(server.pendingOffsets()).toEqual([0]);
    server.respond(0);
    await tick();
    expect(paged.get(0)?.id).toBe(1_000_000);
  });

  it('follows a changed total: drops the pages beyond it, makes the others stale', async () => {
    const paged = make();
    await load(paged);
    await loadPageOf(paged, 200);
    await loadPageOf(paged, 800);
    show(paged, 0);

    server.state.total = 250;
    server.state.generation = 1;
    paged.invalidate();
    server.respond(0);
    await tick();

    expect(paged.total.value).toBe(250);
    expect(paged.get(800)).toBeUndefined();
    expect(paged.get(300)).toBeUndefined();
    expect(paged.get(0)?.id).toBe(1_000_000);
    // Page 1 is kept, but it may have shifted: shown as is and refetched.
    expect(paged.get(210)?.id).toBe(210);
    frame();
    expect(server.pendingOffsets()).toEqual([200]);
  });

  it('shrinks to an empty listing', async () => {
    const paged = make();
    await load(paged);
    show(paged, 0);

    server.state.total = 0;
    paged.invalidate();
    server.respond(0);
    await tick();

    expect(paged.total.value).toBe(0);
    expect(paged.get(0)).toBeUndefined();
    expect(paged.error.value).toBeNull();
  });

  it('grows: the new items are fetched when read', async () => {
    server.state.total = 150;
    const paged = make();
    await load(paged);
    show(paged, 0);

    server.state.total = 500;
    paged.invalidate();
    server.respond(0);
    await tick();

    expect(paged.total.value).toBe(500);
    expect(paged.get(300)).toBeUndefined();
    frame();
    expect(server.pendingOffsets()).toEqual([200, 400].slice(0, 0).concat([200]));
  });

  it('keeps the old items when the refetch fails, and applies the pages that did arrive', async () => {
    const paged = make();
    await load(paged);
    await loadPageOf(paged, 200);
    show(paged, 0, 200);

    server.state.generation = 1;
    paged.invalidate();
    server.respond(0);
    server.fail(200, new ApiError({ status: 500, code: 'internal', message: 'Boom' }));
    await tick();

    expect(paged.error.value).toBe('Boom');
    expect(paged.get(0)?.id).toBe(1_000_000);
    expect(paged.get(200)?.id).toBe(200);

    paged.get(200);
    frame();
    expect(server.pendingOffsets()).toEqual([]);

    paged.retry();
    expect(server.pendingOffsets()).toEqual([200]);
    server.respond(200);
    await tick();
    expect(paged.error.value).toBeNull();
    expect(paged.get(200)?.id).toBe(1_000_200);
  });

  it('does not bump version when every refetch failed', async () => {
    const paged = make();
    await load(paged);
    show(paged, 0);
    const watcher = countRuns(() => paged.version.value);
    const runs = watcher.runs;

    paged.invalidate();
    server.fail(0, new Error('down'));
    await tick();

    expect(watcher.runs).toBe(runs);
    expect(paged.error.value).toBe('down');
    expect(paged.get(0)?.id).toBe(0);
  });

  it('drops the answers of requests it supersedes', async () => {
    server = createServer({ honourAbort: false });
    const paged = make();
    await load(paged);
    show(paged, 0);

    paged.invalidate();
    const first = server.call(0);
    paged.invalidate();
    server.state.generation = 1;
    server.respond(first);
    await tick();
    expect(paged.get(0)?.id).toBe(0);

    server.respond(server.calls[2] as Call);
    await tick();
    expect(paged.get(0)?.id).toBe(1_000_000);
  });

  it('refetches the first page when it never arrived', async () => {
    const paged = make();
    paged.reset(request, 'name', 'asc');
    server.fail(0, new Error('down'));
    await tick();

    paged.invalidate();
    expect(server.calls).toHaveLength(2);
    server.respond(0);
    await tick();

    expect(paged.total.value).toBe(1000);
    expect(paged.error.value).toBeNull();
  });

  it('lets a failed page try again', async () => {
    const paged = make();
    await load(paged);
    show(paged, 200);
    server.fail(200, new Error('nope'));
    await tick();

    paged.invalidate();
    server.respond(200);
    await tick();

    expect(paged.error.value).toBeNull();
    expect(paged.get(200)?.id).toBe(200);
  });
});

describe('PagedItems: fs_change events', () => {
  async function shown(req: BrowseRequest = request) {
    const paged = make();
    await load(paged, req);
    show(paged, 0);
    return paged;
  }

  it.each([
    ['its own directory', 1, ['a/b'], true],
    ['one of several directories', 1, ['x', 'a/b', 'y'], true],
    ['a sibling directory', 1, ['a/c'], false],
    ['its parent', 1, ['a'], false],
    ['a child directory', 1, ['a/b/c'], false],
    ['a directory that only shares the prefix', 1, ['a/bc'], false],
    ['the same directory on another volume', 2, ['a/b'], false],
    ['no directory at all', 1, [], false],
  ] as const)('on %s: refetch is %s', async (_what, volume, dirs, refetches) => {
    await shown();
    const before = server.calls.length;

    bus.fsChange(volume, ...dirs);
    vi.advanceTimersByTime(200);

    expect(server.calls.length - before).toBe(refetches ? 1 : 0);
  });

  it('listens to every volume when the request has none', async () => {
    await shown({ ...request, volume_id: null });
    const before = server.calls.length;
    bus.fsChange(7, 'a/b');
    vi.advanceTimersByTime(200);
    expect(server.calls).toHaveLength(before + 1);
  });

  it('refetches for any change while a plan is active', async () => {
    await shown({ ...request, plan: { plan_id: 3, side: 'source' } });
    const before = server.calls.length;
    bus.fsChange(9, 'somewhere/else');
    vi.advanceTimersByTime(200);
    expect(server.calls).toHaveLength(before + 1);
  });

  it('matches the volume root only for the root', async () => {
    await shown({ ...request, dir: '' });
    const before = server.calls.length;

    bus.fsChange(1, 'a');
    vi.advanceTimersByTime(200);
    expect(server.calls).toHaveLength(before);

    bus.fsChange(1, '');
    vi.advanceTimersByTime(200);
    expect(server.calls).toHaveLength(before + 1);
  });

  it('waits for a quiet period and refetches once after a burst', async () => {
    await shown();
    const before = server.calls.length;

    for (let i = 0; i < 5; i++) {
      bus.fsChange(1, 'a/b');
      vi.advanceTimersByTime(50);
    }
    expect(server.calls).toHaveLength(before);

    vi.advanceTimersByTime(149);
    expect(server.calls).toHaveLength(before);
    vi.advanceTimersByTime(1);
    expect(server.calls).toHaveLength(before + 1);

    vi.advanceTimersByTime(5000);
    expect(server.calls).toHaveLength(before + 1);
  });

  it('cannot be postponed for ever by a steady stream of events', async () => {
    await shown();
    const before = server.calls.length;

    for (let elapsed = 0; elapsed < 1900; elapsed += 150) {
      bus.fsChange(1, 'a/b');
      vi.advanceTimersByTime(150);
    }
    expect(server.calls).toHaveLength(before);

    bus.fsChange(1, 'a/b');
    vi.advanceTimersByTime(150);
    expect(server.calls).toHaveLength(before + 1);
  });

  it('uses the debounce it is given', async () => {
    const paged = make({ debounceMs: 20, maxWaitMs: 100 });
    await load(paged);
    show(paged, 0);
    const before = server.calls.length;

    bus.fsChange(1, 'a/b');
    vi.advanceTimersByTime(20);

    expect(server.calls).toHaveLength(before + 1);
  });

  it('refetches at once after a resync of the event stream', async () => {
    await shown();
    const before = server.calls.length;

    bus.resync();

    expect(server.calls).toHaveLength(before + 1);
    expect(server.calls.at(-1)?.offset).toBe(0);
  });

  it('forgets a pending refetch when the listing changes', async () => {
    const paged = await shown();
    bus.fsChange(1, 'a/b');
    const before = server.calls.length;

    paged.reset({ ...request, dir: 'other' }, 'name', 'asc');
    vi.advanceTimersByTime(5000);

    expect(server.calls).toHaveLength(before + 1);
  });

  it('judges events against the current request', async () => {
    const paged = await shown();
    paged.reset({ ...request, dir: 'other' }, 'name', 'asc');
    server.respond(server.calls.at(-1) as Call);
    await tick();
    show(paged, 0);
    const before = server.calls.length;

    bus.fsChange(1, 'a/b');
    vi.advanceTimersByTime(200);
    expect(server.calls).toHaveLength(before);

    bus.fsChange(1, 'other');
    vi.advanceTimersByTime(200);
    expect(server.calls).toHaveLength(before + 1);
  });
});

describe('PagedItems: dispose', () => {
  it('cancels what is in flight and everything scheduled, and unsubscribes', async () => {
    const paged = make();
    await load(paged);
    paged.get(200);
    frame();
    const flying = server.call(200);
    paged.get(400);
    bus.fsChange(1, 'a/b');
    expect(bus.subscriptions()).toBe(2);

    paged.dispose();

    expect(flying.signal?.aborted).toBe(true);
    expect(bus.subscriptions()).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    const calls = server.calls.length;
    vi.advanceTimersByTime(10_000);
    expect(server.calls).toHaveLength(calls);
  });

  it('makes everything inert', async () => {
    const paged = make();
    await load(paged);
    paged.dispose();

    expect(paged.get(0)).toBeUndefined();
    expect(paged.reset({ ...request, dir: 'other' }, 'name', 'asc')).toBe(false);
    paged.invalidate();
    paged.retry();
    bus.resync();
    frame();

    expect(server.calls).toHaveLength(1);
    expect(paged.total.value).toBe(1000);
  });

  it('ignores answers that arrive afterwards', async () => {
    server = createServer({ honourAbort: false });
    const paged = make();
    paged.reset(request, 'name', 'asc');
    const pending = server.call(0);

    paged.dispose();
    server.respond(pending);
    await tick();

    expect(paged.total.value).toBeNull();
    expect(paged.error.value).toBeNull();
  });

  it('can be called twice', () => {
    const paged = make();
    paged.dispose();
    expect(() => paged.dispose()).not.toThrow();
  });
});

describe('PagedItems: the shared defaults', () => {
  it('uses the shared events bus unless one is given', async () => {
    const { events } = await import('@/api/sse');
    const onSpy = vi.spyOn(events, 'on');
    const resyncSpy = vi.spyOn(events, 'onResync');

    const paged = new PagedItems({ client: server.client });
    created.push(paged);

    expect(onSpy).toHaveBeenCalledWith('fs_change', expect.any(Function));
    expect(resyncSpy).toHaveBeenCalledTimes(1);
    paged.dispose();
  });
});
