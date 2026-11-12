import { afterEach, describe, expect, it } from 'vitest';
import { openPlanResult } from '@/components/opm-plan-result';
import type { PlanRun } from '@/stores/plan-runs';
import { mount } from '@/test/helpers';

const run: PlanRun = {
  tool: 'duplicates',
  planId: 1,
  taskId: 2,
  title: 'Deleted 12 files',
  result: {
    done: 12,
    skipped: 3,
    failed: 10,
    bytes: 2048,
    cancelled: false,
    note: null,
    failures: [
      { file_id: 1, path: 'Photos/a.jpg', code: 'source_missing', message: 'Technical detail A' },
      { file_id: 2, path: 'Photos/b.jpg', code: 'source_missing', message: 'Technical detail B' },
      { file_id: null, path: 'Archive/c', code: 'not_empty', message: 'New files arrived' },
    ],
  },
};
afterEach(() => {
  document.querySelectorAll('opm-plan-result').forEach((el) => {
    el.remove();
  });
});

async function show(value = run) {
  const el = await mount('opm-plan-result', { run: value, open: true });
  await el.querySelector('opm-modal')?.updateComplete;
  return el;
}

describe('plan result', () => {
  it('shows coded safety skips as outcome details without calling them failures', async () => {
    const el = await show({
      ...run,
      result: {
        done: 0,
        failed: 0,
        skipped: 2,
        bytes: 0,
        cancelled: false,
        note: null,
        failures: [
          {
            file_id: 1,
            path: 'Photos/a.jpg',
            code: 'no_survivor',
            message: 'No copy would remain',
          },
          { file_id: 2, path: 'Photos/b.jpg', code: 'stale_match', message: 'Counterpart changed' },
        ],
      },
    });
    expect(el.querySelectorAll('tbody tr')).toHaveLength(2);
    expect(el.textContent).toContain('No surviving matched copy');
    expect(el.textContent).toContain('Matched counterpart changed');
    expect(el.textContent).not.toContain('Showing the first');
    expect(
      [...el.querySelectorAll('.stat')].map((node) =>
        node.textContent?.replace(/\s+/g, ' ').trim(),
      ),
    ).toContain('Failed 0');
  });

  it('reuses a large modal, DaisyUI stats and the bounded zebra table', async () => {
    const el = await show();
    expect(el.querySelector('opm-modal')?.size).toBe('lg');
    expect(
      [...el.querySelectorAll('.stat')].map((node) =>
        node.textContent?.replace(/\s+/g, ' ').trim(),
      ),
    ).toEqual(['Done 12', 'Skipped 3', 'Failed 10', 'Bytes 2 KB']);
    const table = el.querySelector('table');
    expect(table?.classList.contains('table-sm')).toBe(true);
    expect(table?.classList.contains('table-zebra')).toBe(true);
    expect(table?.parentElement?.classList.contains('max-h-96')).toBe(true);
    expect(table?.parentElement?.classList.contains('overflow-y-auto')).toBe(true);
    expect(el.querySelectorAll('tbody tr')).toHaveLength(3);
    expect(el.textContent).toContain('Photos/a.jpg');
    expect(el.querySelector('td[title]')?.getAttribute('title')).toBe('Technical detail A');
    expect(el.textContent).toContain('Showing the first 3 of 13');
    expect(
      [...el.querySelectorAll('li')].map((node) =>
        [...node.querySelectorAll('span')].map((span) => span.textContent?.trim()),
      ),
    ).toEqual([
      ['Source no longer exists', '2'],
      ['Folder is no longer empty', '1'],
    ]);
  });

  it('handles clean results, empty failures and cancellation notes', async () => {
    const el = await show({
      ...run,
      result: {
        done: 12,
        failed: 0,
        skipped: 0,
        bytes: 0,
        failures: [],
        cancelled: true,
        note: 'Stopped by the user',
      },
    });
    expect(el.querySelector('table')).toBeNull();
    expect(el.textContent).toContain('No item failures were reported');
    expect(el.textContent).toContain('Stopped by the user');
    expect(el.textContent).not.toContain('Showing the first');
  });

  it('escapes locations and messages and humanizes future codes', async () => {
    const el = await show({
      ...run,
      result: {
        done: 0,
        failed: 1,
        skipped: 0,
        bytes: 0,
        failures: [
          {
            file_id: 1,
            path: '<img src=x>',
            code: 'new_error',
            message: '<script>danger</script>',
          },
        ],
        cancelled: false,
        note: null,
      },
    });
    expect(el.querySelector('img')).toBeNull();
    expect(el.querySelector('script')).toBeNull();
    expect(el.textContent).toContain('New error');
    expect(el.querySelector('td[title]')?.getAttribute('title')).toBe('<script>danger</script>');
  });

  it('openPlanResult reuses one host, updates content and closes normally', async () => {
    openPlanResult(run);
    openPlanResult({ ...run, title: 'Another result' });
    expect(document.querySelectorAll('opm-plan-result')).toHaveLength(1);
    const el = document.querySelector('opm-plan-result');
    await el?.updateComplete;
    await el?.querySelector('opm-modal')?.updateComplete;
    expect(el?.querySelector('opm-modal')?.heading).toBe('Another result');
    el?.querySelector<HTMLButtonElement>('.modal-action button')?.click();
    await el?.updateComplete;
    expect(el?.open).toBe(false);
  });
});
