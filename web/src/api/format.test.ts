import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { EMPTY_FILTER } from '@/util/filter';

afterEach(() => vi.restoreAllMocks());

it('passes samples and abort signals through the API client', async () => {
  const { previewFormat } = await import('./format');
  const post = vi.spyOn(api, 'post').mockResolvedValue({ samples: [] });
  const signal = new AbortController().signal;
  const body = {
    pattern: '{YYYY}',
    source: {
      scope: { volume_id: 1, dir: '', recursive: true },
      filter: EMPTY_FILTER,
    },
    limit: 5,
  };
  await previewFormat(body, signal);
  expect(post).toHaveBeenCalledWith('/api/format/preview', { body, signal });
});

it('shares the catalogue after success but retries failures', async () => {
  vi.resetModules();
  const { api: freshApi } = await import('@/api/client');
  const { fetchFormatTokens } = await import('./format');
  const get = vi
    .spyOn(freshApi, 'get')
    .mockRejectedValueOnce(new Error('offline'))
    .mockResolvedValue({ tokens: [], examples: [], sample: {} });
  await expect(fetchFormatTokens()).rejects.toThrow('offline');
  await Promise.all([fetchFormatTokens(), fetchFormatTokens()]);
  await fetchFormatTokens();
  expect(get).toHaveBeenCalledTimes(2);
});
