import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { EMPTY_FILTER } from '@/util/filter';

afterEach(() => vi.restoreAllMocks());

it('sends the complete generated request and cancellation signal', async () => {
  const { previewRename } = await import('./rename');
  const post = vi.spyOn(api, 'post').mockResolvedValue({ samples: [] });
  const signal = new AbortController().signal;
  const body = {
    source: { scope: { volume_id: 1, dir: 'cafe\u0301', recursive: true }, filter: EMPTY_FILTER },
    rules: {
      matcher: { mode: 'glob' as const, pattern: '*.JPG', ignore_case: true },
      clean: true,
      rewrite: { find: '^IMG', replace: '$1', ignore_case: false },
      pattern: '{name}_{count}',
      count_digits: 3,
    },
    limit: 5,
  };
  await previewRename(body, signal);
  expect(post).toHaveBeenCalledWith('/api/rename/preview', { body, signal });
});

it('retries catalogue failures and keeps format and rename caches independent', async () => {
  vi.resetModules();
  const { api: freshApi } = await import('@/api/client');
  const { fetchRenameTokens } = await import('./rename');
  const { fetchFormatTokens } = await import('./format');
  const get = vi
    .spyOn(freshApi, 'get')
    .mockRejectedValueOnce(new Error('offline'))
    .mockResolvedValue({ tokens: [], examples: [], sample: {} });
  await expect(fetchRenameTokens()).rejects.toThrow('offline');
  await Promise.all([fetchRenameTokens(), fetchRenameTokens()]);
  await fetchRenameTokens();
  await fetchFormatTokens();
  expect(get.mock.calls.map(([path]) => path)).toEqual([
    '/api/rename/tokens',
    '/api/rename/tokens',
    '/api/format/tokens',
  ]);
});
