import { afterEach, expect, it, vi } from 'vitest';
import { fetchFormatTokens, previewFormat } from '@/api/format';
import type { FormatPreviewResponse } from '@/api/generated/FormatPreviewResponse';
import type { FormatTokensResponse } from '@/api/generated/FormatTokensResponse';
import { mount } from '@/test/helpers';
import { EMPTY_FILTER } from '@/util/filter';
import { type ApmPatternEditor, type PatternChangeEvent, patternSpan } from './apm-pattern-editor';

vi.mock('@/api/format', () => ({ fetchFormatTokens: vi.fn(), previewFormat: vi.fn() }));
const catalogue: FormatTokensResponse = {
  tokens: [
    { name: 'YYYY', aliases: ['year'], group: 'date', description: 'Year', example: '2026' },
  ],
  examples: [{ pattern: '{YYYY}', description: 'By year', result: '2026/a.jpg' }],
  sample: { file_name: 'a.jpg', captured: 0, camera: null, folder: '' },
};
const source = {
  scope: { volume_id: 1, dir: 'café', recursive: true },
  filter: EMPTY_FILTER,
};
const valid: FormatPreviewResponse = {
  error: null,
  appends_filename: true,
  has_filename_token: false,
  samples: [
    {
      file_id: 1,
      source: 'photos/a.jpg',
      destination: '2026/a.jpg',
      skip: null,
      capture_src: 'xmp',
    },
  ],
};

afterEach(() => {
  for (const el of document.querySelectorAll('apm-pattern-editor')) el.remove();
  vi.useRealTimers();
  vi.resetAllMocks();
});

async function setup(props: Partial<ApmPatternEditor> = {}) {
  vi.mocked(fetchFormatTokens).mockResolvedValue(catalogue);
  vi.mocked(previewFormat).mockResolvedValue(valid);
  const el = await mount('apm-pattern-editor', { value: '{YYYY}', source, ...props });
  const changes: PatternChangeEvent['detail'][] = [];
  el.addEventListener('pattern-change', (event) => {
    changes.push(event.detail);
    el.value = event.detail.value;
  });
  vi.useFakeTimers();
  // Replace the timer scheduled before fake timers were installed.
  el.value += '/';
  await el.updateComplete;
  return { el, changes, input: el.querySelector('input') as HTMLInputElement };
}

async function settle() {
  await vi.advanceTimersByTimeAsync(300);
}

it('converts UTF-8 spans without splitting surrogate pairs', () => {
  expect(patternSpan('é/😀/{yyyy}', 8, 14)).toEqual([5, 11]);
  expect(patternSpan('abc', 0, 0)).toEqual([0, 0]);
});

it('loads only server chips and inserts at a selected range, preserving focus', async () => {
  const { el, input } = await setup();
  input.focus();
  input.setSelectionRange(0, 6);
  const chip = [...el.querySelectorAll('button')].find((button) => button.textContent === '{YYYY}');
  expect(chip).toBeDefined();
  expect(el.textContent).not.toContain('{year}');
  chip?.click();
  await el.updateComplete;
  expect(input.value).toBe('{YYYY}/');
  expect(input.selectionStart).toBe(6);
  expect(document.activeElement).toBe(input);
});

it.each([
  ['', 0, '{YYYY}'],
  ['abc', 1, 'a{YYYY}bc'],
  ['abc', 3, 'abc{YYYY}'],
])('inserts at the caret in %s', async (value, caret, expected) => {
  const { el, input } = await setup();
  el.value = value;
  await el.updateComplete;
  input.setSelectionRange(caret, caret);
  [...el.querySelectorAll('button')].find((button) => button.textContent === '{YYYY}')?.click();
  await el.updateComplete;
  expect(input.value).toBe(expected);
  expect(input.selectionStart).toBe(caret + 6);
});

it('retries a failed catalogue while keeping the input usable, then applies server examples', async () => {
  const { el, input } = await setup();
  vi.mocked(fetchFormatTokens).mockRejectedValueOnce(new Error('catalogue offline'));
  // Reconnect tests the component's retry path without introducing a second token catalogue.
  el.remove();
  document.body.append(el);
  await Promise.resolve();
  await el.updateComplete;
  expect(el.textContent).toContain('catalogue offline');
  expect(input.disabled).toBe(false);
  vi.mocked(fetchFormatTokens).mockResolvedValue(catalogue);
  [...el.querySelectorAll('button')].find((button) => button.textContent === 'Retry')?.click();
  await Promise.resolve();
  await el.updateComplete;
  [...el.querySelectorAll<HTMLButtonElement>('li button')]
    .find((button) => button.textContent?.includes('By year'))
    ?.click();
  await el.updateComplete;
  expect(el.value).toBe('{YYYY}');
});

it('debounces a burst, renders samples with prefix and date source, and announces validity', async () => {
  const { el, input, changes } = await setup({ basePrefix: 'photos/Sorted/' });
  for (const value of ['a', 'ab', 'abc']) {
    input.value = value;
    input.dispatchEvent(new Event('input'));
    await el.updateComplete;
  }
  await settle();
  expect(previewFormat).toHaveBeenCalledTimes(1);
  expect(previewFormat).toHaveBeenCalledWith(
    { pattern: 'abc', source, limit: 5 },
    expect.any(AbortSignal),
  );
  await el.updateComplete;
  expect(el.textContent).toContain('photos/Sorted/2026/a.jpg');
  expect(el.textContent).toContain('xmp');
  expect(el.textContent).toContain('the original name is added at the end');
  expect(changes.at(-1)?.status).toBe('valid');
  expect(changes.some((change) => change.status === 'checking')).toBe(true);
});

it('selects and replaces the exact server error span in non-ASCII input', async () => {
  const { el, input } = await setup({ value: 'é/😀/{yyyy}' });
  vi.mocked(previewFormat).mockResolvedValue({
    ...valid,
    error: {
      code: 'unknown_token',
      message: 'Unknown token',
      suggestion: '{YYYY}',
      start: 8,
      end: 14,
    },
  });
  await settle();
  await el.updateComplete;
  expect(input.getAttribute('aria-invalid')).toBe('true');
  const buttons = [...el.querySelectorAll('button')];
  buttons.find((button) => button.textContent === 'Show')?.click();
  expect([input.selectionStart, input.selectionEnd]).toEqual([5, 11]);
  buttons.find((button) => button.textContent === 'Use {YYYY}')?.click();
  await el.updateComplete;
  expect(input.value).toBe('é/😀/{YYYY}/');
});

it('aborts stale checks and ignores their late answers', async () => {
  const { el, input } = await setup();
  let resolve!: (value: FormatPreviewResponse) => void;
  vi.mocked(previewFormat).mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  await settle();
  const signal = vi.mocked(previewFormat).mock.calls[0]?.[1];
  input.value = 'new';
  input.dispatchEvent(new Event('input'));
  await el.updateComplete;
  expect(signal?.aborted).toBe(true);
  await settle();
  resolve({
    ...valid,
    error: { code: 'old', message: 'Obsolete', suggestion: null, start: 0, end: 1 },
  });
  await Promise.resolve();
  await el.updateComplete;
  expect(el.textContent).not.toContain('Obsolete');
});

it('does not preview or insert during composition, without a source, or while disabled', async () => {
  const { el, input } = await setup({ source: null });
  await settle();
  expect(previewFormat).not.toHaveBeenCalled();
  expect(el.textContent).toContain('Pick a source folder');
  el.source = source;
  await el.updateComplete;
  input.dispatchEvent(new CompositionEvent('compositionstart'));
  input.value = '日本';
  input.dispatchEvent(new Event('input'));
  await settle();
  expect(previewFormat).not.toHaveBeenCalled();
  input.dispatchEvent(new CompositionEvent('compositionend'));
  await el.updateComplete;
  await settle();
  expect(previewFormat).toHaveBeenCalledOnce();
  el.disabled = true;
  await el.updateComplete;
  expect(input.disabled).toBe(true);
  expect(
    [...el.querySelectorAll('button[title]')].every(
      (button) => (button as HTMLButtonElement).disabled,
    ),
  ).toBe(true);
});

it('network failures remain idle rather than invalid, and empty/skip samples are explicit', async () => {
  const { el, changes } = await setup();
  vi.mocked(previewFormat).mockRejectedValueOnce(new Error('offline'));
  await settle();
  await el.updateComplete;
  expect(changes.at(-1)?.status).toBe('idle');
  expect(el.textContent).toContain('Could not check the pattern');
  vi.mocked(previewFormat).mockResolvedValue({ ...valid, samples: [] });
  [...el.querySelectorAll('button')].find((button) => button.textContent === 'Retry')?.click();
  await settle();
  await el.updateComplete;
  expect(el.textContent).toContain('No files to preview');
  vi.mocked(previewFormat).mockResolvedValue({
    ...valid,
    samples: [
      {
        file_id: 1,
        source: 'photos/a.jpg',
        destination: null,
        capture_src: null,
        skip: 'metadata_pending',
      },
    ],
  });
  el.value = 'other';
  await el.updateComplete;
  await settle();
  await el.updateComplete;
  expect(el.textContent).toContain('Metadata is still being indexed');
});
