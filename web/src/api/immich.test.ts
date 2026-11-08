import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import * as immich from '@/api/immich';

afterEach(() => vi.restoreAllMocks());

it('uses typed wrappers for every route, with exact query/body and cancellation', async () => {
  const get = vi.spyOn(api, 'get').mockResolvedValue({});
  const post = vi.spyOn(api, 'post').mockResolvedValue({});
  const put = vi.spyOn(api, 'put').mockResolvedValue({});
  const remove = vi.spyOn(api, 'delete').mockResolvedValue({});
  const signal = new AbortController().signal;
  await immich.fetchImmichStatus({ check: true, signal });
  expect(get).toHaveBeenLastCalledWith('/api/immich/status', { query: { check: true }, signal });
  const request = {
    server_url: 'http://localhost',
    email: 'admin@example.test',
    password: 'test',
    allow_insecure_tls: true,
  };
  await immich.connectImmich(request);
  expect(post).toHaveBeenLastCalledWith('/api/immich/connect', { body: request });
  await immich.disconnectImmich();
  expect(remove).toHaveBeenCalledWith('/api/immich/connection');
  await immich.fetchImmichLibraries();
  expect(get).toHaveBeenLastCalledWith('/api/immich/libraries');
  await immich.saveImmichMappings([]);
  expect(put).toHaveBeenLastCalledWith('/api/immich/mappings', { body: { mappings: [] } });
  await immich.detectImmichPrefix({ volume_id: 1, library_id: null }, signal);
  expect(post).toHaveBeenLastCalledWith('/api/immich/detect-prefix', {
    body: { volume_id: 1, library_id: null },
    signal,
  });
  await immich.excludeImmichTrash();
  expect(post).toHaveBeenLastCalledWith('/api/immich/exclude-trash');
  await immich.requestImmichSync({ full: true });
  expect(post).toHaveBeenLastCalledWith('/api/immich/sync', { body: { full: true } });
  await immich.fetchImmichOutbox({ state: 'attention', offset: 50, signal });
  expect(get).toHaveBeenLastCalledWith('/api/immich/outbox', {
    query: { state: 'attention', offset: 50, limit: 50 },
    signal,
  });
  await immich.retryImmichOutbox([1]);
  expect(post).toHaveBeenLastCalledWith('/api/immich/outbox/retry', { body: { ids: [1] } });
  await immich.retryImmichOutbox();
  expect(post).toHaveBeenLastCalledWith('/api/immich/outbox/retry', { body: { ids: null } });
  await immich.dismissImmichOutbox([1]);
  expect(post).toHaveBeenLastCalledWith('/api/immich/outbox/dismiss', { body: { ids: [1] } });
});
