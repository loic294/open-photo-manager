import { afterEach, describe, expect, it, vi } from 'vitest';
import { CSRF_HEADER, createApiClient } from '@/api/client';
import { createPlan, createPlanSelection, deletePlan, executePlan, getPlan } from '@/api/plans';

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('typed plan API', () => {
  it('sends complete DTO bodies, propagates signals and uses the shared CSRF client', async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response('{}'));
    const client = createApiClient({ fetch });
    const signal = new AbortController().signal;
    const options = { client, signal };
    await createPlan('duplicates', { nested: { dir: 'E\u0301te\u0301' } }, options);
    await createPlan('move', {}, { ...options, replaces: 7 });
    await getPlan(7, options);
    await deletePlan(7, options);
    await executePlan(7, { options: { action: 'delete_source' }, only: null }, options);
    await createPlanSelection(7, 'source', options);
    await createPlanSelection(7, 'comparison', { ...options, restrictIds: [1, 2] });
    const requests = fetch.mock.calls.map(([path, init]) => ({
      path,
      method: init?.method,
      body: init?.body ? JSON.parse(String(init.body)) : null,
    }));
    expect(requests).toEqual([
      {
        path: '/api/plans',
        method: 'POST',
        body: {
          kind: 'duplicates',
          params: { nested: { dir: 'E\u0301te\u0301' } },
          replaces: null,
        },
      },
      { path: '/api/plans', method: 'POST', body: { kind: 'move', params: {}, replaces: 7 } },
      { path: '/api/plans/7', method: 'GET', body: null },
      { path: '/api/plans/7', method: 'DELETE', body: null },
      {
        path: '/api/plans/7/execute',
        method: 'POST',
        body: { options: { action: 'delete_source' }, only: null },
      },
      {
        path: '/api/plans/7/selection',
        method: 'POST',
        body: { side: 'source', restrict_ids: null },
      },
      {
        path: '/api/plans/7/selection',
        method: 'POST',
        body: { side: 'comparison', restrict_ids: [1, 2] },
      },
    ]);
    for (const [, init] of fetch.mock.calls) {
      expect(init?.signal).toBe(signal);
      expect(new Headers(init?.headers).get(CSRF_HEADER)).toBe(init?.method === 'GET' ? null : '1');
    }
  });

  it.each([404, 409])('does not swallow a %s delete error', async (status) => {
    const client = createApiClient({
      fetch: async () =>
        new Response(JSON.stringify({ error: { code: 'plan_not_found', message: 'Expired' } }), {
          status,
        }),
    });
    await expect(deletePlan(7, { client })).rejects.toMatchObject({ status, message: 'Expired' });
  });

  it('propagates aborts untouched', async () => {
    const client = createApiClient({
      fetch: async () => {
        throw new DOMException('Aborted', 'AbortError');
      },
    });
    await expect(createPlan('cleanup', {}, { client })).rejects.toMatchObject({
      name: 'AbortError',
    });
  });
});
