import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Filter } from '@/api/generated/Filter';
import type { FoldersResponse } from '@/api/generated/FoldersResponse';
import type { PlanRef } from '@/api/generated/PlanRef';
import type { VolumeDto } from '@/api/generated/VolumeDto';

const photos: VolumeDto = {
  id: 4,
  name: 'photos',
  writable: true,
  online: true,
  fs_type: null,
  case_insensitive: null,
  watch_mode: 'watching',
  watch_reason: null,
};
const backup: VolumeDto = {
  id: 9,
  name: 'backup',
  writable: false,
  online: false,
  fs_type: null,
  case_insensitive: null,
  watch_mode: 'watching',
  watch_reason: null,
};

const filter: Filter = {
  cameras: ['Canon EOS R5'],
  ratings: [],
  flags: [],
  tags: [],
  selection_id: null,
};
const plan: PlanRef = { plan_id: 12, side: 'comparison' };

// The volumes store is a module singleton: every test starts from an unloaded one.
async function setup(loaded?: readonly VolumeDto[]) {
  vi.resetModules();
  const [browse, volumes, client] = await Promise.all([
    import('@/api/browse'),
    import('@/stores/volumes'),
    import('@/api/client'),
  ]);
  if (loaded) {
    vi.spyOn(client.api, 'get').mockResolvedValue(loaded);
    await volumes.loadVolumes();
  }
  return { ...browse, volumes, client };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('requestFor', () => {
  it('translates the volume name to its id', async () => {
    const { requestFor } = await setup([photos, backup]);

    expect(requestFor({ volume: 'photos', dir: '2024/trip' }, filter, plan)).toEqual({
      volume_id: 4,
      dir: '2024/trip',
      filter,
      plan,
    });
    expect(requestFor({ volume: 'backup', dir: '' }, filter, null)).toEqual({
      volume_id: 9,
      dir: '',
      filter,
      plan: null,
    });
  });

  it('is the volumes root without a volume, and needs no volume list for it', async () => {
    const { requestFor } = await setup();

    expect(requestFor({ volume: null, dir: '' }, filter, plan)).toEqual({
      volume_id: null,
      dir: '',
      filter,
      plan,
    });
  });

  it('never lets a directory ride along without a volume', async () => {
    const { requestFor } = await setup();

    expect(requestFor({ volume: null, dir: 'stray' }, filter, null)?.dir).toBe('');
  });

  it('is null while the volumes are not loaded', async () => {
    const { requestFor } = await setup();

    expect(requestFor({ volume: 'photos', dir: '' }, filter, null)).toBeNull();
  });

  it('is null for a volume name the store does not know', async () => {
    const { requestFor } = await setup([photos]);

    expect(requestFor({ volume: 'cards', dir: '' }, filter, null)).toBeNull();
  });

  it('matches names exactly, as they are on disk', async () => {
    const { requestFor } = await setup([{ ...photos, name: 'Été' }]);

    expect(requestFor({ volume: 'Été', dir: '' }, filter, null)?.volume_id).toBe(4);
    expect(requestFor({ volume: 'Ete\u0301', dir: '' }, filter, null)).toBeNull();
    expect(requestFor({ volume: 'été', dir: '' }, filter, null)).toBeNull();
  });

  it('keeps the filter and the plan it is given', async () => {
    const { requestFor } = await setup([photos]);

    const request = requestFor({ volume: 'photos', dir: '' }, filter, plan);

    expect(request?.filter).toBe(filter);
    expect(request?.plan).toBe(plan);
  });

  it('follows the store when the volumes arrive', async () => {
    const { requestFor, volumes, client } = await setup();
    expect(requestFor({ volume: 'photos', dir: '' }, filter, null)).toBeNull();

    vi.spyOn(client.api, 'get').mockResolvedValue([photos]);
    await volumes.loadVolumes();

    expect(requestFor({ volume: 'photos', dir: '' }, filter, null)?.volume_id).toBe(4);
  });
});

describe('fetchFolders', () => {
  const response: FoldersResponse = { folders: [], counts: { in_folder: 2, in_subfolders: 5 } };

  it('posts the request to the folders endpoint and returns the answer', async () => {
    const { fetchFolders } = await setup();
    const post = vi.fn().mockResolvedValue(response);
    const signal = new AbortController().signal;

    const result = await fetchFolders(
      { volume_id: 4, dir: 'a/b', filter, plan },
      { client: { post }, signal },
    );

    expect(result).toBe(response);
    expect(post).toHaveBeenCalledWith('/api/browse/folders', {
      body: { volume_id: 4, dir: 'a/b', filter, plan },
      signal,
    });
  });

  it('sends only the fields of a browse request', async () => {
    const { fetchFolders } = await setup();
    const post = vi.fn().mockResolvedValue(response);
    const request = { volume_id: null, dir: '', filter, plan: null, extra: 'x' };

    await fetchFolders(request, { client: { post } });

    expect(post.mock.calls[0]?.[1]?.body).toEqual({
      volume_id: null,
      dir: '',
      filter,
      plan: null,
    });
  });

  it('goes through the shared client by default, with the csrf header', async () => {
    const { fetchFolders } = await setup();
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(response), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );
    vi.stubGlobal('fetch', fetchSpy);

    await expect(fetchFolders({ volume_id: 1, dir: '', filter, plan: null })).resolves.toEqual(
      response,
    );

    const [url, init] = fetchSpy.mock.calls[0] ?? [];
    expect(url).toBe('/api/browse/folders');
    expect(init.method).toBe('POST');
    expect(init.headers.get('x-apm-request')).toBe('1');
    vi.unstubAllGlobals();
  });

  it('lets the error of the server through with its code', async () => {
    const { fetchFolders } = await setup();
    const { ApiError } = await import('@/api/errors');
    const failure = new ApiError({ status: 404, code: 'folder_not_found', message: 'gone' });
    const post = vi.fn().mockRejectedValue(failure);

    await expect(
      fetchFolders({ volume_id: 1, dir: 'x', filter, plan: null }, { client: { post } }),
    ).rejects.toBe(failure);
  });
});
