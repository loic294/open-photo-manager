import { afterEach, expect, it, vi } from 'vitest';
import { createApiClient } from '@/api/client';
import { createSelection, getSelection } from '@/api/selections';

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it('creates and reads selections using complete DTOs and the injected client/signal', async () => {
  const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response('{"id":5}'));
  const client = createApiClient({ fetch });
  const signal = new AbortController().signal;
  await expect(createSelection([1, 2], undefined, { client, signal })).resolves.toEqual({ id: 5 });
  await createSelection([3], 'From Duplicate finder', { client, signal });
  await getSelection(5, { client, signal });
  expect(fetch.mock.calls.map(([path, init]) => [path, init?.method, init?.body])).toEqual([
    ['/api/selections', 'POST', '{"ids":[1,2],"label":null}'],
    ['/api/selections', 'POST', '{"ids":[3],"label":"From Duplicate finder"}'],
    ['/api/selections/5', 'GET', null],
  ]);
  expect(fetch.mock.calls.every(([, init]) => init?.signal === signal)).toBe(true);
});

it('propagates server errors', async () => {
  const client = createApiClient({
    fetch: async () =>
      new Response('{"error":{"code":"selection_not_found","message":"Selection expired"}}', {
        status: 404,
      }),
  });
  await expect(getSelection(5, { client })).rejects.toMatchObject({ code: 'selection_not_found' });
});
