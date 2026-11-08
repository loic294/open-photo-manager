import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchFormatTokens } from '@/api/format';
import type { FormatTokensResponse } from '@/api/generated/FormatTokensResponse';
import type { RenamePreviewResponse } from '@/api/generated/RenamePreviewResponse';
import type { RenameRules } from '@/api/generated/RenameRules';
import { fetchRenameTokens, previewRename } from '@/api/rename';
import type { PatternChangeEvent } from '@/components/apm-pattern-editor';
import '@/components/apm-pattern-editor';
import { formatPreviewAdapter, renamePreviewAdapter } from '@/components/pattern-preview-adapter';
import { mount } from '@/test/helpers';
import { EMPTY_FILTER } from '@/util/filter';

vi.mock('@/api/format', () => ({ fetchFormatTokens: vi.fn(), previewFormat: vi.fn() }));
vi.mock('@/api/rename', () => ({ fetchRenameTokens: vi.fn(), previewRename: vi.fn() }));
const source = {
  scope: { volume_id: 1, dir: 'cafe\u0301', recursive: true },
  filter: EMPTY_FILTER,
};
const rules: RenameRules = {
  matcher: { mode: 'glob', pattern: '*.JPG', ignore_case: false },
  clean: true,
  rewrite: { find: '^IMG', replace: 'Photo', ignore_case: true },
  pattern: '{name}_{count}',
  count_digits: 3,
};
const catalogue: FormatTokensResponse = {
  tokens: [
    { name: 'count', aliases: [], group: 'file', description: 'Folder counter', example: '001' },
  ],
  examples: [],
  sample: { file_name: 'IMG.JPG', captured: 0, camera: null, folder: '' },
};
const response: RenamePreviewResponse = {
  issues: [],
  volume_id: 1,
  dir: 'first/folder',
  matched: 21000,
  folder_files: 22000,
  truncated: true,
  samples: [
    { file_id: 1, name: 'IMG (1).JPG', new_name: 'Photo_001.JPG', outcome: 'rename', error: null },
    {
      file_id: 2,
      name: 'unchanged.JPG',
      new_name: 'unchanged.JPG',
      outcome: 'unchanged',
      error: null,
    },
    { file_id: 3, name: 'pending.JPG', new_name: null, outcome: 'pending', error: null },
    {
      file_id: 4,
      name: 'failed.JPG',
      new_name: null,
      outcome: 'failed',
      error: 'extension_changed',
    },
  ],
};
beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(fetchRenameTokens).mockResolvedValue(catalogue);
  vi.mocked(previewRename).mockResolvedValue(response);
});
afterEach(() => {
  for (const el of document.querySelectorAll('apm-pattern-editor')) el.remove();
  vi.useRealTimers();
  vi.resetAllMocks();
});
async function setup() {
  const el = await mount('apm-pattern-editor', {
    value: rules.pattern,
    source,
    rules,
    requestKey: 'revision-1',
    adapter: renamePreviewAdapter,
    label: 'Name pattern',
    limit: 4,
  });
  const changes: PatternChangeEvent['detail'][] = [];
  el.addEventListener('pattern-change', (event) => {
    changes.push(event.detail);
    el.value = event.detail.value;
  });
  return { el, changes };
}

it('sends every rule and renders all server sample outcomes with first-folder and cap context', async () => {
  const { el, changes } = await setup();
  await vi.advanceTimersByTimeAsync(300);
  await el.updateComplete;
  expect(previewRename).toHaveBeenCalledWith({ source, rules, limit: 4 }, expect.any(AbortSignal));
  expect(el.textContent).toContain('Photo_001.JPG');
  expect(el.textContent).toContain('Unchanged');
  expect(el.textContent).toContain('Waiting for metadata');
  expect(el.textContent).toContain('original extension');
  expect(el.textContent).toContain('first/folder');
  expect(el.textContent).toContain('21000');
  expect(el.textContent).toContain('counters in this sample may be incomplete');
  expect(el.textContent).toContain('final conflict-resolved names');
  expect(changes.at(-1)).toMatchObject({ status: 'valid', requestKey: 'revision-1' });
  expect(fetchFormatTokens).not.toHaveBeenCalled();
});

it.each(['success', 'error'] as const)(
  'ignores late %s after an unrelated rule changes without editing the name pattern',
  async (outcome) => {
    let resolve: (value: RenamePreviewResponse) => void = () => {};
    let reject: (error: Error) => void = () => {};
    vi.mocked(previewRename).mockImplementationOnce(
      () =>
        new Promise((done, fail) => {
          resolve = done;
          reject = fail;
        }),
    );
    const { el, changes } = await setup();
    await vi.advanceTimersByTimeAsync(300);
    const signal = vi.mocked(previewRename).mock.calls[0]?.[1];
    el.rules = { ...rules, clean: false };
    el.requestKey = 'revision-2';
    await el.updateComplete;
    expect(signal?.aborted).toBe(true);
    if (outcome === 'success') resolve(response);
    else reject(new Error('late failure'));
    await Promise.resolve();
    await el.updateComplete;
    expect(changes.at(-1)).toMatchObject({ status: 'checking', requestKey: 'revision-2' });
    expect(el.textContent).not.toContain('late failure');
    await vi.advanceTimersByTimeAsync(300);
    await el.updateComplete;
    expect(previewRename).toHaveBeenLastCalledWith(
      {
        source,
        rules: { ...rules, clean: false },
        limit: 4,
      },
      expect.any(AbortSignal),
    );
    expect(changes.at(-1)).toMatchObject({ status: 'valid', requestKey: 'revision-2' });
  },
);

it('routes matcher/rewrite/count issues without highlighting or suggesting edits to the name pattern', async () => {
  const issues: RenamePreviewResponse['issues'] = [
    {
      field: 'matcher',
      code: 'invalid_regex',
      message: 'Bad matcher',
      start: 0,
      end: 8,
      suggestion: 'wrong',
    },
    {
      field: 'rewrite',
      code: 'invalid_regex',
      message: 'Bad rewrite',
      start: 0,
      end: 8,
      suggestion: null,
    },
    {
      field: 'count_digits',
      code: 'invalid_digits',
      message: 'Bad digits',
      start: 0,
      end: 1,
      suggestion: null,
    },
  ];
  vi.mocked(previewRename).mockResolvedValue({ ...response, samples: [], issues });
  const { el, changes } = await setup();
  await vi.advanceTimersByTimeAsync(300);
  await el.updateComplete;
  expect(changes.at(-1)).toMatchObject({ status: 'invalid', issues });
  expect(el.querySelector('input')?.getAttribute('aria-invalid')).toBe('false');
  expect(el.textContent).not.toContain('Use wrong');
  expect(el.querySelector('table')).toBeNull();
});

it('preserves UTF-8 name-pattern spans and suggestions through the adapter', async () => {
  vi.mocked(previewRename).mockResolvedValue({
    ...response,
    samples: [],
    issues: [
      {
        field: 'pattern',
        code: 'unknown_token',
        message: 'Unknown token',
        start: 8,
        end: 14,
        suggestion: '{name}',
      },
    ],
  });
  const { el } = await setup();
  el.value = 'é/😀/{neme}';
  await el.updateComplete;
  await vi.advanceTimersByTimeAsync(300);
  await el.updateComplete;
  [...el.querySelectorAll('button')].find((button) => button.textContent === 'Show')?.click();
  expect(el.querySelector('input')?.selectionStart).toBe(5);
  [...el.querySelectorAll('button')].find((button) => button.textContent === 'Use {name}')?.click();
  await el.updateComplete;
  expect(el.value).toBe('é/😀/{name}');
});

it('reports network failure as unvalidated and retries; empty samples remain explicit', async () => {
  vi.mocked(previewRename)
    .mockRejectedValueOnce(new Error('offline'))
    .mockResolvedValue({ ...response, matched: 0, truncated: false, samples: [] });
  const { el, changes } = await setup();
  await vi.advanceTimersByTimeAsync(300);
  await el.updateComplete;
  expect(changes.at(-1)?.status).toBe('idle');
  expect(el.textContent).toContain('offline');
  [...el.querySelectorAll('button')].find((button) => button.textContent === 'Retry')?.click();
  await vi.advanceTimersByTimeAsync(300);
  await el.updateComplete;
  expect(changes.at(-1)?.status).toBe('valid');
  expect(el.textContent).toContain('No files to preview');
  expect(el.textContent).toContain('Other folders are not sampled');
});

it('cancels on IME and disconnect, suppressing late completions', async () => {
  let resolve: (value: RenamePreviewResponse) => void = () => {};
  vi.mocked(previewRename).mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const { el, changes } = await setup();
  await vi.advanceTimersByTimeAsync(300);
  const signal = vi.mocked(previewRename).mock.calls[0]?.[1];
  const input = el.querySelector('input');
  input?.dispatchEvent(new CompositionEvent('compositionstart'));
  expect(changes.at(-1)?.status).toBe('checking');
  expect(signal?.aborted).toBe(true);
  input?.dispatchEvent(new CompositionEvent('compositionend'));
  await el.updateComplete;
  el.remove();
  const before = changes.length;
  resolve(response);
  await Promise.resolve();
  expect(changes).toHaveLength(before);
});

it('guards catalogue switches and retries failures without leaking count into format', async () => {
  let resolve: (value: FormatTokensResponse) => void = () => {};
  vi.mocked(fetchRenameTokens).mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  vi.mocked(fetchFormatTokens).mockRejectedValueOnce(new Error('catalogue offline'));
  const { el } = await setup();
  el.adapter = formatPreviewAdapter;
  el.source = null;
  await el.updateComplete;
  resolve(catalogue);
  await Promise.resolve();
  await el.updateComplete;
  expect(el.textContent).not.toContain('{count}');
  expect(el.textContent).toContain('catalogue offline');
  vi.mocked(fetchFormatTokens).mockResolvedValue({ ...catalogue, tokens: [] });
  [...el.querySelectorAll('button')].find((button) => button.textContent === 'Retry')?.click();
  await Promise.resolve();
  await el.updateComplete;
  expect(el.textContent).not.toContain('catalogue offline');
  expect(el.textContent).not.toContain('{count}');
});
