import { afterEach, describe, expect, it, vi } from 'vitest';
import { CSRF_HEADER } from '@/api/client';
import { isApiError } from '@/api/errors';
import type { IndexStatusDto } from '@/api/generated/IndexStatusDto';
import type { RescanRequest } from '@/api/generated/RescanRequest';
import { getIndexStatus, rescan } from '@/api/indexing';
import { makeTask } from '@/test/task-fixtures';

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

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('rescan', () => {
  const request: RescanRequest = {
    targets: [{ volume_id: 3, dir: 'trips/2024', recursive: true }],
    force: true,
    confirm_removals: false,
  };

  it('posts the request to the rescan endpoint with the CSRF header, and returns the task', async () => {
    const task = makeTask({ id: 9, kind: 'index', state: 'queued', title: 'Re-indexing 1 folder' });
    const fetchMock = stubFetch(202, task);

    await expect(rescan(request)).resolves.toEqual(task);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url)).toBe('/api/index/rescan');
    expect(init?.method).toBe('POST');
    expect(new Headers(init?.headers).get(CSRF_HEADER)).toBe('1');
    expect(JSON.parse(String(init?.body))).toEqual(request);
  });

  it('rejects with the ApiError of the server', async () => {
    stubFetch(409, { error: { code: 'volume_offline', message: 'Volume photos is offline' } });

    const failure = await rescan(request).catch((error: unknown) => error);

    expect(isApiError(failure)).toBe(true);
    expect(failure).toMatchObject({
      status: 409,
      code: 'volume_offline',
      message: 'Volume photos is offline',
    });
  });

  it('can be aborted', async () => {
    const controller = new AbortController();
    const fetchMock = vi.fn<typeof fetch>(
      (_url, init) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener('abort', () =>
            reject(new DOMException('aborted', 'AbortError')),
          );
        }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const pending = rescan(request, controller.signal);
    controller.abort();

    await expect(pending).rejects.toMatchObject({ name: 'AbortError' });
  });
});

describe('getIndexStatus', () => {
  it('reads the status endpoint without a body', async () => {
    const status: IndexStatusDto = {
      held_removals: [{ volume_id: 1, volume_name: 'photos', files: 12_345 }],
      active_task_id: 4,
      watcher: null,
      warnings: [],
    };
    const fetchMock = stubFetch(200, status);

    await expect(getIndexStatus()).resolves.toEqual(status);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url)).toBe('/api/index/status');
    expect(init?.method).toBe('GET');
    expect(init?.body ?? undefined).toBeUndefined();
  });

  it('rejects with the ApiError of the server', async () => {
    stubFetch(500, { error: { code: 'internal', message: 'Something went wrong' } });

    await expect(getIndexStatus()).rejects.toMatchObject({ status: 500, code: 'internal' });
  });
});
