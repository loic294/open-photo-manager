import { afterEach, describe, expect, it, type Mock, vi } from 'vitest';
import { type ApiClient, createApiClient } from '@/api/client';
import { isAbortError, isApiError } from '@/api/errors';
import {
  contentUrl,
  fetchFileDetail,
  fetchIds,
  fetchItems,
  ITEMS_PAGE_SIZE,
  previewUrl,
  THUMB_SM_PX,
  thumbSizeFor,
  thumbUrl,
} from '@/api/files';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';

const request: BrowseRequest = {
  volume_id: 2,
  dir: 'trip/day 1',
  filter: { cameras: ['Canon R5'], ratings: [], flags: [], tags: [], selection_id: null },
  plan: { plan_id: 4, side: 'source' },
};

type FakeClient<T> = Pick<ApiClient, 'post'> & {
  post: Mock<(path: string, options?: unknown) => Promise<T>>;
};

function fakeClient<T>(response: T): FakeClient<T> {
  return {
    post: vi.fn<(path: string, options?: unknown) => Promise<T>>(async () => response),
  } as unknown as FakeClient<T>;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('fetchItems', () => {
  it('posts the request, the sort and the page window to the items endpoint', async () => {
    const page = { items: [], total: 0, offset: 400 };
    const client = fakeClient(page);

    const result = await fetchItems(request, 'date', 'desc', 400, 200, { client });

    expect(result).toBe(page);
    expect(client.post).toHaveBeenCalledTimes(1);
    expect(client.post).toHaveBeenCalledWith('/api/browse/items', {
      body: {
        volume_id: 2,
        dir: 'trip/day 1',
        filter: request.filter,
        plan: request.plan,
        sort: 'date',
        order: 'desc',
        offset: 400,
        limit: 200,
      },
      signal: undefined,
    });
  });

  it('sends the named fields only', async () => {
    const client = fakeClient({ items: [], total: 0, offset: 0 });
    const extra = { ...request, selected: [1, 2], view: 'grid' };

    await fetchItems(extra, 'name', 'asc', 0, 50, { client });

    const [, options] = client.post.mock.calls[0] as [string, { body: Record<string, unknown> }];
    expect(Object.keys(options.body).sort()).toEqual(
      ['dir', 'filter', 'limit', 'offset', 'order', 'plan', 'sort', 'volume_id'].sort(),
    );
  });

  it('keeps a null volume and a null plan', async () => {
    const client = fakeClient({ items: [], total: 0, offset: 0 });
    await fetchItems({ ...request, volume_id: null, plan: null }, 'size', 'asc', 0, 10, { client });
    const [, options] = client.post.mock.calls[0] as [string, { body: Record<string, unknown> }];
    expect(options.body.volume_id).toBeNull();
    expect(options.body.plan).toBeNull();
  });

  it('passes the abort signal through', async () => {
    const client = fakeClient({ items: [], total: 0, offset: 0 });
    const controller = new AbortController();
    await fetchItems(request, 'name', 'asc', 0, 200, { client, signal: controller.signal });
    expect(client.post).toHaveBeenCalledWith(
      '/api/browse/items',
      expect.objectContaining({ signal: controller.signal }),
    );
  });

  it('goes through the shared api client by default, with the CSRF header', async () => {
    const fetchMock = vi.fn<typeof fetch>(
      async () => new Response(JSON.stringify({ items: [], total: 7, offset: 0 })),
    );
    vi.stubGlobal('fetch', fetchMock);

    const page = await fetchItems(request, 'name', 'asc', 0, 200);

    expect(page.total).toBe(7);
    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url)).toBe('/api/browse/items');
    expect(init?.method).toBe('POST');
    expect(new Headers(init?.headers).get('x-apm-request')).toBe('1');
    expect(JSON.parse(String(init?.body))).toMatchObject({ dir: 'trip/day 1', limit: 200 });
  });

  it('rejects with an AbortError when the request is aborted', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(
        (_url, init) =>
          new Promise((_resolve, reject) => {
            init?.signal?.addEventListener('abort', () =>
              reject(new DOMException('aborted', 'AbortError')),
            );
          }),
      ),
    );
    const controller = new AbortController();
    const pending = fetchItems(request, 'name', 'asc', 0, 200, { signal: controller.signal });
    controller.abort();
    await expect(pending).rejects.toSatisfy(isAbortError);
  });

  it('works with an isolated client', async () => {
    const fetchMock = vi.fn<typeof fetch>(
      async () => new Response(JSON.stringify({ items: [], total: 1, offset: 0 })),
    );
    const client = createApiClient({ fetch: fetchMock });
    await expect(fetchItems(request, 'name', 'asc', 0, 200, { client })).resolves.toMatchObject({
      total: 1,
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

describe('fetchIds', () => {
  it('posts to the ids endpoint and returns the id list', async () => {
    const client = fakeClient({ ids: [5, 3, 9] });

    const ids = await fetchIds(request, 'size', 'desc', { client });

    expect(ids).toEqual([5, 3, 9]);
    expect(client.post).toHaveBeenCalledWith('/api/browse/ids', {
      body: {
        volume_id: 2,
        dir: 'trip/day 1',
        filter: request.filter,
        plan: request.plan,
        sort: 'size',
        order: 'desc',
        offset: 0,
        limit: ITEMS_PAGE_SIZE,
      },
      signal: undefined,
    });
  });

  it('passes the abort signal through', async () => {
    const client = fakeClient({ ids: [] });
    const controller = new AbortController();
    await fetchIds(request, 'name', 'asc', { client, signal: controller.signal });
    expect(client.post).toHaveBeenCalledWith(
      '/api/browse/ids',
      expect.objectContaining({ signal: controller.signal }),
    );
  });
});

describe('thumbSizeFor', () => {
  it('uses the small rendition while it is sharp enough', () => {
    expect(thumbSizeFor(160, 1)).toBe('sm');
    expect(thumbSizeFor(160, 2)).toBe('sm');
    expect(thumbSizeFor(THUMB_SM_PX, 1)).toBe('sm');
    expect(thumbSizeFor(96, 3)).toBe('sm');
  });

  it('switches to the large rendition above 320 device pixels', () => {
    expect(thumbSizeFor(161, 2)).toBe('lg');
    expect(thumbSizeFor(320, 2)).toBe('lg');
    expect(thumbSizeFor(321, 1)).toBe('lg');
    expect(thumbSizeFor(112, 3)).toBe('lg');
  });

  it('reads the window pixel ratio by default', () => {
    vi.stubGlobal('devicePixelRatio', 2);
    expect(thumbSizeFor(160)).toBe('sm');
    expect(thumbSizeFor(200)).toBe('lg');
    vi.stubGlobal('devicePixelRatio', 1);
    expect(thumbSizeFor(200)).toBe('sm');
  });

  it('falls back to a ratio of 1 when the ratio is unusable', () => {
    expect(thumbSizeFor(300, 0)).toBe('sm');
    expect(thumbSizeFor(300, Number.NaN)).toBe('sm');
    vi.stubGlobal('devicePixelRatio', 0);
    expect(thumbSizeFor(300)).toBe('sm');
  });
});

describe('thumbUrl', () => {
  it('puts the id, the size class and the mtime in the URL', () => {
    expect(thumbUrl({ id: 42, mtime: 1_700_000_000 }, 'sm')).toBe(
      '/api/files/42/thumb?size=sm&v=1700000000',
    );
    expect(thumbUrl({ id: 7, mtime: 5 }, 'lg')).toBe('/api/files/7/thumb?size=lg&v=5');
  });

  it('changes with the mtime, which is the browser cache key', () => {
    expect(thumbUrl({ id: 1, mtime: 1 }, 'sm')).not.toBe(thumbUrl({ id: 1, mtime: 2 }, 'sm'));
  });
});

describe('previewUrl and contentUrl', () => {
  it('build the media routes without a cache key', () => {
    expect(previewUrl(42)).toBe('/api/files/42/preview');
    expect(contentUrl(42)).toBe('/api/files/42/content');
    expect(contentUrl(42, {})).toBe('/api/files/42/content');
    expect(contentUrl(42, { download: false })).toBe('/api/files/42/content');
  });

  it('asks the server for an attachment when downloading', () => {
    expect(contentUrl(7, { download: true })).toBe('/api/files/7/content?download=1');
  });
});

describe('fetchFileDetail', () => {
  const detail = {
    item: { id: 42, name: 'IMG_1.jpg' },
    hash: null,
    mime: 'image/jpeg',
    btime: null,
    tags: [],
    sidecars: [],
  };

  it('gets the file from the shared api client', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => new Response(JSON.stringify(detail)));
    vi.stubGlobal('fetch', fetchMock);

    await expect(fetchFileDetail(42)).resolves.toEqual(detail);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url)).toBe('/api/files/42');
    expect(init?.method).toBe('GET');
  });

  it('passes the abort signal through and rejects with an AbortError', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(
        (_url, init) =>
          new Promise((_resolve, reject) => {
            init?.signal?.addEventListener('abort', () =>
              reject(new DOMException('aborted', 'AbortError')),
            );
          }),
      ),
    );
    const controller = new AbortController();
    const pending = fetchFileDetail(42, controller.signal);
    controller.abort();
    await expect(pending).rejects.toSatisfy(isAbortError);
  });

  it('rejects an unknown file with the server error code', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(
        async () =>
          new Response(JSON.stringify({ error: { code: 'not_found', message: 'gone' } }), {
            status: 404,
          }),
      ),
    );
    const failure = await fetchFileDetail(9).catch((error: unknown) => error);
    expect(isApiError(failure)).toBe(true);
    expect(failure).toMatchObject({ status: 404, code: 'not_found' });
  });
});
