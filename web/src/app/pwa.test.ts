import { runInNewContext } from 'node:vm';
import { afterEach, expect, it, vi } from 'vitest';
import { toast } from '@/components/toast';
import serviceWorkerSource from '../../public/sw.js?raw';
import { registerPwa } from './pwa';

vi.mock('@/components/toast', () => ({ toast: { error: vi.fn() } }));

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
  vi.clearAllMocks();
});

it('registers only in production on secure origins', async () => {
  const register = vi.fn().mockResolvedValue({});
  vi.stubGlobal('window', { isSecureContext: true });
  vi.stubGlobal('navigator', { serviceWorker: { register } });
  vi.stubEnv('PROD', false);
  await registerPwa();
  expect(register).not.toHaveBeenCalled();
  vi.stubEnv('PROD', true);
  vi.stubGlobal('window', { isSecureContext: false });
  await registerPwa();
  expect(register).not.toHaveBeenCalled();
  vi.stubGlobal('window', { isSecureContext: true });
  await registerPwa();
  expect(register).toHaveBeenCalledWith('/sw.js', { scope: '/', updateViaCache: 'none' });
});

it('reports service worker installation failures', async () => {
  vi.stubEnv('PROD', true);
  vi.stubGlobal('window', { isSecureContext: true });
  vi.stubGlobal('navigator', {
    serviceWorker: { register: vi.fn().mockRejectedValue(new Error('Unavailable')) },
  });
  await registerPwa();
  expect(toast.error).toHaveBeenCalledWith(expect.stringContaining('could not be installed'));
});

it('caches only the offline notice and leaves API responses and media untouched', async () => {
  type WorkerEvent = {
    request: { url: string; mode: string };
    waitUntil: (promise: Promise<unknown>) => void;
    respondWith: (promise: Promise<unknown>) => void;
  };
  const listeners = new Map<string, (event: WorkerEvent) => void>();
  const offline = { status: 200 };
  const add = vi.fn().mockResolvedValue(undefined);
  const match = vi.fn().mockResolvedValue(offline);
  const fetch = vi.fn().mockRejectedValue(new Error('Offline'));
  runInNewContext(serviceWorkerSource, {
    URL,
    Response,
    fetch,
    caches: { open: vi.fn().mockResolvedValue({ add }), match },
    self: {
      location: { origin: 'https://photos.test' },
      addEventListener: (name: string, handler: (event: WorkerEvent) => void) =>
        listeners.set(name, handler),
    },
  });
  const waitUntil = vi.fn();
  const respondWith = vi.fn();
  const event = {
    request: { url: 'https://photos.test/duplicates', mode: 'navigate' },
    waitUntil,
    respondWith,
  };
  listeners.get('install')?.(event);
  await waitUntil.mock.calls[0][0];
  expect(add).toHaveBeenCalledExactlyOnceWith('/offline.html');
  for (const [path, mode] of [
    ['/api/files/1/content', 'navigate'],
    ['/api/browse/items', 'cors'],
    ['/assets/app.js', 'no-cors'],
  ]) {
    listeners.get('fetch')?.({
      ...event,
      request: { url: `https://photos.test${path}`, mode },
    });
  }
  expect(respondWith).not.toHaveBeenCalled();
  expect(fetch).not.toHaveBeenCalled();
  expect(match).not.toHaveBeenCalled();
  listeners.get('fetch')?.(event);
  expect(await respondWith.mock.calls[0][0]).toBe(offline);
  expect(match).toHaveBeenCalledExactlyOnceWith('/offline.html');
});
