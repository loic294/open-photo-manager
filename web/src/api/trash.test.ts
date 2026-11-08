import { afterEach, describe, expect, it, vi } from 'vitest';
import { CSRF_HEADER } from '@/api/client';
import { isApiError } from '@/api/errors';
import type { TrashBatchesQuery } from '@/api/generated/TrashBatchesQuery';
import type { TrashBatchesResponse } from '@/api/generated/TrashBatchesResponse';
import type { TrashEmptyRequest } from '@/api/generated/TrashEmptyRequest';
import type { TrashListQuery } from '@/api/generated/TrashListQuery';
import type { TrashListResponse } from '@/api/generated/TrashListResponse';
import type { TrashRestoreRequest } from '@/api/generated/TrashRestoreRequest';
import type { TrashSummary } from '@/api/generated/TrashSummary';
import { emptyTrash, listTrash, listTrashBatches, restoreTrash, syncTrash } from '@/api/trash';
import { makeTask } from '@/test/task-fixtures';

const SUMMARY: TrashSummary = {
  items: 3,
  bytes: 3_000,
  max_id: 9,
  volumes: [{ volume_id: 1, name: 'photos', items: 3, bytes: 3_000, online: true, writable: true }],
  retention_days: 30,
  conflict_policy: 'smart',
};

function stubFetch(status: number, body: unknown) {
  const fetchMock = vi.fn<typeof fetch>(
    async () =>
      new Response(JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      }),
  );
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

function searchOf(url: unknown): Record<string, string> {
  return Object.fromEntries(new URL(String(url), 'http://apm.test').searchParams);
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('listTrash', () => {
  const query: TrashListQuery = {
    volume_id: 2,
    batch: '20240101-101010-t7',
    q: 'IMG 100%',
    sort: 'size',
    order: 'asc',
    offset: 50,
    limit: 25,
  };

  it('reads the trash endpoint with every filter in the query string and no body', async () => {
    const page: TrashListResponse = { items: [], total: 0, summary: SUMMARY };
    const fetchMock = stubFetch(200, page);

    await expect(listTrash(query)).resolves.toEqual(page);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url).startsWith('/api/trash?')).toBe(true);
    expect(searchOf(url)).toEqual({
      volume_id: '2',
      batch: '20240101-101010-t7',
      q: 'IMG 100%',
      sort: 'size',
      order: 'asc',
      offset: '50',
      limit: '25',
    });
    expect(init?.method).toBe('GET');
    expect(init?.body ?? undefined).toBeUndefined();
  });

  it('leaves out the filters that are null, so the server keeps its defaults', async () => {
    const fetchMock = stubFetch(200, { items: [], total: 0, summary: SUMMARY });

    await listTrash({
      volume_id: null,
      batch: null,
      q: null,
      sort: 'trashed_at',
      order: null,
      offset: 0,
      limit: 50,
    });

    const [url] = fetchMock.mock.calls[0] ?? [];
    expect(searchOf(url)).toEqual({ sort: 'trashed_at', offset: '0', limit: '50' });
  });

  it('rejects with the ApiError of the server', async () => {
    stubFetch(422, {
      error: { code: 'validation_failed', message: 'Invalid request', fields: [] },
    });

    const failure = await listTrash({ ...query, limit: 0 }).catch((error: unknown) => error);

    expect(isApiError(failure)).toBe(true);
    expect(failure).toMatchObject({ status: 422, code: 'validation_failed' });
  });

  it('can be aborted', async () => {
    const controller = new AbortController();
    vi.stubGlobal(
      'fetch',
      vi.fn<typeof fetch>(
        (_url, init) =>
          new Promise<Response>((_resolve, reject) => {
            init?.signal?.addEventListener('abort', () =>
              reject(new DOMException('aborted', 'AbortError')),
            );
          }),
      ),
    );

    const pending = listTrash(query, controller.signal);
    controller.abort();

    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
  });
});

describe('listTrashBatches', () => {
  it('reads the batches endpoint with the volume and the page', async () => {
    const batches: TrashBatchesResponse = {
      batches: [
        {
          volume_id: 1,
          volume: 'photos',
          batch: '20240101-101010-t7',
          task_id: 7,
          trashed_at: 1_700_000_000,
          items: 3,
          bytes: 3_000,
          reasons: ['user'],
        },
      ],
      total: 1,
      summary: SUMMARY,
    };
    const fetchMock = stubFetch(200, batches);
    const query: TrashBatchesQuery = { volume_id: 1, offset: 20, limit: 10 };

    await expect(listTrashBatches(query)).resolves.toEqual(batches);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url).startsWith('/api/trash/batches?')).toBe(true);
    expect(searchOf(url)).toEqual({ volume_id: '1', offset: '20', limit: '10' });
    expect(init?.method).toBe('GET');
  });

  it('omits a null volume', async () => {
    const fetchMock = stubFetch(200, { batches: [], total: 0, summary: SUMMARY });

    await listTrashBatches({ volume_id: null, offset: 0, limit: 50 });

    const [url] = fetchMock.mock.calls[0] ?? [];
    expect(searchOf(url)).toEqual({ offset: '0', limit: '50' });
  });
});

describe('restoreTrash', () => {
  it('posts a list of ids with the CSRF header and returns the task', async () => {
    const task = makeTask({ id: 12, kind: 'fs_move', state: 'queued', title: 'Restoring 2 items' });
    const fetchMock = stubFetch(202, task);
    const request: TrashRestoreRequest = {
      target: { kind: 'ids', ids: [4, 5] },
      conflict_policy: null,
    };

    await expect(restoreTrash(request)).resolves.toEqual(task);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url)).toBe('/api/trash/restore');
    expect(init?.method).toBe('POST');
    expect(new Headers(init?.headers).get(CSRF_HEADER)).toBe('1');
    expect(JSON.parse(String(init?.body))).toEqual(request);
  });

  it('posts a whole batch', async () => {
    const fetchMock = stubFetch(202, makeTask({ id: 13, kind: 'fs_move' }));
    const request: TrashRestoreRequest = {
      target: { kind: 'batch', volume_id: 1, batch: '20240101-101010-t7' },
      conflict_policy: 'rename',
    };

    await restoreTrash(request);

    const [, init] = fetchMock.mock.calls[0] ?? [];
    expect(JSON.parse(String(init?.body))).toEqual(request);
  });

  it('rejects with the ApiError of the server', async () => {
    stubFetch(409, { error: { code: 'volume_offline', message: 'Volume photos is offline' } });

    const failure = await restoreTrash({
      target: { kind: 'ids', ids: [1] },
      conflict_policy: null,
    }).catch((error: unknown) => error);

    expect(failure).toMatchObject({ status: 409, code: 'volume_offline' });
  });
});

describe('emptyTrash', () => {
  it('posts the volume and the up_to_id promise with the CSRF header', async () => {
    const task = makeTask({ id: 14, kind: 'trash_empty', title: 'Emptying the trash' });
    const fetchMock = stubFetch(202, task);
    const request: TrashEmptyRequest = { volume_id: 2, up_to_id: 9 };

    await expect(emptyTrash(request)).resolves.toEqual(task);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url)).toBe('/api/trash/empty');
    expect(init?.method).toBe('POST');
    expect(new Headers(init?.headers).get(CSRF_HEADER)).toBe('1');
    expect(JSON.parse(String(init?.body))).toEqual({ volume_id: 2, up_to_id: 9 });
  });

  it('rejects with trash_already_empty when nothing matches', async () => {
    stubFetch(409, { error: { code: 'trash_already_empty', message: 'The trash is empty' } });

    await expect(emptyTrash({ volume_id: null, up_to_id: 1 })).rejects.toMatchObject({
      status: 409,
      code: 'trash_already_empty',
    });
  });
});

describe('syncTrash', () => {
  it('posts without a body and returns the task', async () => {
    const task = makeTask({ id: 15, kind: 'fs_trash', title: 'Checking the trash' });
    const fetchMock = stubFetch(202, task);

    await expect(syncTrash()).resolves.toEqual(task);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url)).toBe('/api/trash/sync');
    expect(init?.method).toBe('POST');
    expect(new Headers(init?.headers).get(CSRF_HEADER)).toBe('1');
    expect(init?.body ?? undefined).toBeUndefined();
  });
});
