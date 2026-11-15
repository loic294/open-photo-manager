import { afterEach, describe, expect, it, vi } from 'vitest';
import { CSRF_HEADER } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { SettingsDto } from '@/api/generated/SettingsDto';
import { sampleSettings } from '@/test/settings-fixtures';

async function setup() {
  vi.resetModules();
  const [store, client] = await Promise.all([import('@/stores/settings'), import('@/api/client')]);
  return {
    store,
    get: vi.spyOn(client.api, 'get'),
    put: vi.spyOn(client.api, 'put'),
  };
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

const failure = (status = 503, code = 'db_busy') =>
  new ApiError({ status, code, message: 'The database is busy' });

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('settings store', () => {
  it('surfaces a startup load failure instead of silently hiding exclusion tags', async () => {
    const { store, get } = await setup();
    const { toasts } = await import('@/stores/toasts');
    get.mockRejectedValue(new Error('Cannot load exclusions'));
    const stop = store.startSettings();
    await vi.waitFor(() =>
      expect(toasts.value.some((toast) => toast.message.includes('Cannot load exclusions'))).toBe(
        true,
      ),
    );
    stop();
    expect(store.settings.value).toBeNull();
  });

  it('loads persisted folder exclusions at startup, refreshes on reconnect and stops listening', async () => {
    const { store, get } = await setup();
    const { events } = await import('@/api/sse');
    let reconnect = () => {};
    const unsubscribe = vi.fn();
    vi.spyOn(events, 'onResync').mockImplementation((callback) => {
      reconnect = callback;
      return unsubscribe;
    });
    const doc = sampleSettings({ soft_excluded_folders: [{ volume_id: 1, path: 'Protected' }] });
    get.mockResolvedValue(doc);
    const stop = store.startSettings();
    await vi.waitFor(() => expect(store.settings.value).toEqual(doc));
    get.mockResolvedValue(sampleSettings());
    reconnect();
    await vi.waitFor(() => expect(store.settings.value?.soft_excluded_folders).toEqual([]));
    expect(get).toHaveBeenCalledTimes(2);
    stop();
    expect(unsubscribe).toHaveBeenCalledOnce();
  });

  it('is empty until something has been loaded', async () => {
    const { store } = await setup();

    expect(store.settings.value).toBeNull();
  });

  it('loads GET /api/settings into the signal and resolves with it', async () => {
    const { store, get } = await setup();
    const doc = sampleSettings({ name_max_len: 143 });
    get.mockResolvedValue(doc);

    await expect(store.loadSettings()).resolves.toEqual(doc);

    expect(get).toHaveBeenCalledExactlyOnceWith('/api/settings');
    expect(store.settings.value).toEqual(doc);
  });

  it('shares one request between loads made while it runs, then fetches again', async () => {
    const { store, get } = await setup();
    const first = deferred<SettingsDto>();
    get.mockReturnValueOnce(first.promise);

    const a = store.loadSettings();
    const b = store.loadSettings();
    expect(b).toBe(a);
    first.resolve(sampleSettings());
    await a;
    expect(get).toHaveBeenCalledTimes(1);

    const fresh = sampleSettings({ name_max_len: 200 });
    get.mockResolvedValueOnce(fresh);
    await expect(store.loadSettings()).resolves.toEqual(fresh);
    expect(get).toHaveBeenCalledTimes(2);
    expect(store.settings.value).toEqual(fresh);
  });

  it('rejects with the ApiError, keeps what it had and loads again on the next call', async () => {
    const { store, get } = await setup();
    const doc = sampleSettings();
    get.mockResolvedValueOnce(doc);
    await store.loadSettings();

    const error = failure();
    get.mockRejectedValueOnce(error);
    await expect(store.loadSettings()).rejects.toBe(error);
    expect(store.settings.value).toEqual(doc);

    const newer = sampleSettings({ trash_retention_days: 7 });
    get.mockResolvedValueOnce(newer);
    await expect(store.loadSettings()).resolves.toEqual(newer);
  });

  it('saves the whole document with PUT, CSRF header included, and keeps the normalised answer', async () => {
    const { store } = await setup();
    const sent = { ...sampleSettings(), sidecar_extensions: ['.XMP', 'xmp', ' json '] };
    const normalised = { ...sent, sidecar_extensions: ['xmp', 'json'] };
    const fetchMock = vi.fn<typeof fetch>(
      async () =>
        new Response(JSON.stringify(normalised), {
          headers: { 'content-type': 'application/json' },
        }),
    );
    vi.stubGlobal('fetch', fetchMock);

    await expect(store.saveSettings(sent)).resolves.toEqual(normalised);

    const [url, init] = fetchMock.mock.calls[0] ?? [];
    expect(String(url)).toBe('/api/settings');
    expect(init?.method).toBe('PUT');
    expect(new Headers(init?.headers).get(CSRF_HEADER)).toBe('1');
    expect(JSON.parse(String(init?.body))).toEqual(sent);
    expect(store.settings.value).toEqual(normalised);
  });

  it('rejects a failed save with the ApiError and keeps the stored settings', async () => {
    const { store, get, put } = await setup();
    const doc = sampleSettings();
    get.mockResolvedValueOnce(doc);
    await store.loadSettings();
    const error = new ApiError({
      status: 422,
      code: 'validation_failed',
      message: 'name_max_len: must be between 16 and 255',
      fields: [{ field: 'name_max_len', message: 'must be between 16 and 255' }],
    });
    put.mockRejectedValueOnce(error);

    await expect(store.saveSettings({ ...doc, name_max_len: 5 })).rejects.toBe(error);

    expect(store.settings.value).toEqual(doc);
  });

  it('does not let a load that began before a save overwrite the saved settings', async () => {
    const { store, get, put } = await setup();
    const stale = deferred<SettingsDto>();
    get.mockReturnValueOnce(stale.promise);
    const saved = sampleSettings({ name_max_len: 143 });
    put.mockResolvedValueOnce(saved);

    const loading = store.loadSettings();
    await store.saveSettings(saved);
    stale.resolve(sampleSettings({ name_max_len: 255 }));

    await expect(loading).resolves.toEqual(saved);
    expect(store.settings.value).toEqual(saved);
  });
});

describe('excluded folders', () => {
  const folder = { volume_id: 1, path: '2024' };

  it('excludes and includes a folder and keeps the answered document', async () => {
    const { store } = await setup();
    const client = await import('@/api/client');
    const excluded = sampleSettings({ excluded_folders: [folder] });
    const post = vi
      .spyOn(client.api, 'post')
      .mockResolvedValueOnce(excluded)
      .mockResolvedValueOnce(sampleSettings());

    await store.excludeFolder(folder);
    expect(store.settings.value).toEqual(excluded);
    await store.includeFolder(folder);
    expect(store.settings.value?.excluded_folders).toEqual([]);

    expect(post).toHaveBeenNthCalledWith(1, '/api/settings/excluded-folders', { body: folder });
    expect(post).toHaveBeenNthCalledWith(2, '/api/settings/excluded-folders/remove', {
      body: folder,
    });
  });

  it('matches a folder and what is below it, never a sibling that shares its prefix', async () => {
    const { store } = await setup();
    const list = [folder];

    expect(store.isExcluded(list, 1, '2024')).toBe(true);
    expect(store.isExcluded(list, 1, '2024/trip')).toBe(true);
    expect(store.isExcluded(list, 1, '2024.old')).toBe(false);
    expect(store.isExcluded(list, 2, '2024')).toBe(false);
    expect(store.isExcluded(list, 1, '')).toBe(false);
  });
});
