import { afterEach, describe, expect, it, type Mock, vi } from 'vitest';
import { type ApiClient, api } from '@/api/client';
import { isAbortError } from '@/api/errors';
import { fetchFacets, mergeFacets } from '@/api/facets';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { Facets } from '@/api/generated/Facets';

const request: BrowseRequest = {
  volume_id: 2,
  dir: 'trip/day 1',
  filter: { cameras: ['Canon R5'], ratings: [4], flags: [], tags: [], selection_id: null },
  plan: null,
};

const facets: Facets = {
  cameras: [{ value: 'Canon R5', count: 12 }],
  tags: [{ value: 'sunset', count: 3 }],
  ratings: [{ value: '4', count: 5 }],
};

type FakeClient = Pick<ApiClient, 'post'> & {
  post: Mock<(path: string, options?: unknown) => Promise<Facets>>;
};

function fakeClient(response: Facets = facets): FakeClient {
  return {
    post: vi.fn<(path: string, options?: unknown) => Promise<Facets>>(async () => response),
  } as unknown as FakeClient;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('fetchFacets', () => {
  it('posts the location and the filter to the facets endpoint', async () => {
    const client = fakeClient();

    const result = await fetchFacets(request, { client });

    expect(result).toBe(facets);
    expect(client.post).toHaveBeenCalledTimes(1);
    expect(client.post).toHaveBeenCalledWith('/api/facets', {
      body: { volume_id: 2, dir: 'trip/day 1', filter: request.filter, plan: null },
      signal: undefined,
    });
  });

  it('sends the named fields only', async () => {
    const client = fakeClient();

    await fetchFacets({ ...request, selected: [1], view: 'grid' } as BrowseRequest, { client });

    const [, options] = client.post.mock.calls[0] as [string, { body: Record<string, unknown> }];
    expect(Object.keys(options.body).sort()).toEqual(['dir', 'filter', 'plan', 'volume_id']);
  });

  it('keeps a null volume (the whole library) and the root dir', async () => {
    const client = fakeClient();

    await fetchFacets({ ...request, volume_id: null, dir: '' }, { client });

    const [, options] = client.post.mock.calls[0] as [string, { body: Record<string, unknown> }];
    expect(options.body.volume_id).toBeNull();
    expect(options.body.dir).toBe('');
  });

  it('does not rewrite the dir: it is the exact on-disk name', async () => {
    const client = fakeClient();

    await fetchFacets({ ...request, dir: 'Caf\u00e9/e\u0301t\u00e9 ' }, { client });

    const [, options] = client.post.mock.calls[0] as [string, { body: { dir: string } }];
    expect(options.body.dir).toBe('Caf\u00e9/e\u0301t\u00e9 ');
  });

  it('passes the abort signal through', async () => {
    const client = fakeClient();
    const controller = new AbortController();

    await fetchFacets(request, { client, signal: controller.signal });

    const [, options] = client.post.mock.calls[0] as [string, { signal: AbortSignal }];
    expect(options.signal).toBe(controller.signal);
  });

  it('rejects with an abort error when the signal fires', async () => {
    const controller = new AbortController();
    const client = {
      post: vi.fn(
        (_path: string, options?: { signal?: AbortSignal }) =>
          new Promise<Facets>((_resolve, reject) => {
            options?.signal?.addEventListener('abort', () =>
              reject(new DOMException('aborted', 'AbortError')),
            );
          }),
      ),
    } as unknown as FakeClient;

    const pending = fetchFacets(request, { client, signal: controller.signal });
    controller.abort();

    await expect(pending).rejects.toSatisfy(isAbortError);
  });

  it('uses the shared api client by default', async () => {
    const post = vi.spyOn(api, 'post').mockResolvedValue(facets);

    await expect(fetchFacets(request)).resolves.toBe(facets);

    expect(post).toHaveBeenCalledWith(
      '/api/facets',
      expect.objectContaining({ body: expect.any(Object) }),
    );
  });

  it('lets a failure through', async () => {
    const client = fakeClient();
    client.post.mockRejectedValue(new Error('boom'));

    await expect(fetchFacets(request, { client })).rejects.toThrow('boom');
  });
});

describe('mergeFacets', () => {
  const home: Facets = {
    cameras: [
      { value: 'Sony A7', count: 9 },
      { value: 'Canon R5', count: 4 },
    ],
    tags: [{ value: 'Sunset', count: 2 }],
    ratings: [
      { value: '5', count: 3 },
      { value: '1', count: 1 },
    ],
  };
  const away: Facets = {
    cameras: [
      { value: 'canon r5', count: 7 },
      { value: 'Nikon Z6', count: 1 },
    ],
    tags: [
      { value: 'sunset', count: 8 },
      { value: 'beach', count: 1 },
    ],
    ratings: [{ value: '0', count: 6 }],
  };

  it('is empty without an answer', () => {
    expect(mergeFacets([])).toEqual({ cameras: [], tags: [], ratings: [] });
  });

  it('keeps one answer as it is: the server order and the counts', () => {
    expect(mergeFacets([home])).toEqual({
      cameras: [
        { value: 'Sony A7', count: 9 },
        { value: 'Canon R5', count: 4 },
      ],
      tags: [{ value: 'Sunset', count: 2 }],
      ratings: [
        { value: '5', count: 3 },
        { value: '1', count: 1 },
      ],
    });
  });

  it('keeps a one-answer list that is empty', () => {
    expect(mergeFacets([{ cameras: [], tags: [], ratings: [] }])).toEqual({
      cameras: [],
      tags: [],
      ratings: [],
    });
  });

  it('takes the union of two answers and shows no counts', () => {
    const merged = mergeFacets([home, away]);

    expect(merged.cameras).toEqual([
      { value: 'Canon R5' },
      { value: 'Nikon Z6' },
      { value: 'Sony A7' },
    ]);
    expect(merged.tags).toEqual([{ value: 'beach' }, { value: 'Sunset' }]);
    for (const list of [merged.cameras, merged.tags, merged.ratings]) {
      for (const option of list) expect(option).not.toHaveProperty('count');
    }
  });

  it('compares names without case and keeps the first spelling', () => {
    const merged = mergeFacets([away, home]);

    expect(merged.cameras.map((option) => option.value)).toEqual([
      'canon r5',
      'Nikon Z6',
      'Sony A7',
    ]);
    expect(merged.tags.map((option) => option.value)).toEqual(['beach', 'sunset']);
  });

  it('sorts ratings numerically', () => {
    const merged = mergeFacets([home, away]);

    expect(merged.ratings.map((option) => option.value)).toEqual(['0', '1', '5']);
  });

  it('merges the same answer twice into one list', () => {
    const merged = mergeFacets([home, home]);

    expect(merged.cameras).toHaveLength(2);
    expect(merged.ratings.map((option) => option.value)).toEqual(['1', '5']);
  });

  it('works when one of two answers is empty', () => {
    const merged = mergeFacets([home, { cameras: [], tags: [], ratings: [] }]);

    expect(merged.cameras.map((option) => option.value)).toEqual(['Canon R5', 'Sony A7']);
  });

  it('does not change its input', () => {
    const snapshot = structuredClone([home, away]);

    mergeFacets([home, away]);

    expect([home, away]).toEqual(snapshot);
  });
});
