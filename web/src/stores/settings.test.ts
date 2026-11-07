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
