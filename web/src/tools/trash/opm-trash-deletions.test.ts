import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@/test/helpers';
import { makeTrashBatch, makeTrashVolume, TRASHED_AT } from '@/test/trash-fixtures';
import type { TrashBatchDetail, TrashBatchEvent } from '@/tools/trash/opm-trash-deletions';
import { formatFileTime } from '@/util/format';
import '@/tools/trash/opm-trash-deletions';

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

const rows = (host: HTMLElement) => [...host.querySelectorAll('tbody tr')];
const cells = (row: Element) => [...row.querySelectorAll('td')];

function button(scope: ParentNode, label: string): HTMLButtonElement {
  const found = [...scope.querySelectorAll<HTMLButtonElement>('button')].find(
    (candidate) => text(candidate) === label,
  );
  if (!found) throw new Error(`No "${label}" button`);
  return found;
}

/** Three days after the fixtures were trashed. */
const NOW_MS = (TRASHED_AT + 3 * 86_400) * 1000;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime(NOW_MS);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('opm-trash-deletions', () => {
  it('shows a row per deletion with when, where, how much and why', async () => {
    const host = await mount('opm-trash-deletions', {
      batches: [
        makeTrashBatch({ items: 1_250, bytes: 5_242_880, reasons: ['user', 'replaced'] }),
        makeTrashBatch({
          volume_id: 2,
          volume: 'archive',
          batch: '20250102-090000-t9',
          trashed_at: TRASHED_AT + 86_400,
          items: 1,
          bytes: 2_048,
          reasons: ['junk'],
        }),
      ],
      volumes: [makeTrashVolume(), makeTrashVolume({ volume_id: 2, name: 'archive' })],
    });

    const reasons = (row: Element | undefined) =>
      [...(row?.querySelectorAll('.badge') ?? [])].map(text);
    const [firstRow, secondRow] = rows(host);
    const [first, second] = [firstRow, secondRow].map((row) => cells(row as Element).map(text));
    expect(first?.slice(0, 4)).toEqual(['3 days ago', 'photos', '1,250', '5 MB']);
    expect(reasons(firstRow)).toEqual(['Deleted', 'Replaced']);
    expect(second?.slice(0, 4)).toEqual(['2 days ago', 'archive', '1', '2 KB']);
    expect(reasons(secondRow)).toEqual(['Junk']);
  });

  it('puts the exact time of a deletion in a tooltip', async () => {
    const host = await mount('opm-trash-deletions', { batches: [makeTrashBatch()] });
    expect(host.querySelector('tbody td span')?.getAttribute('data-tooltip')).toBe(
      formatFileTime(TRASHED_AT),
    );
  });

  it('calls a deletion without a recorded reason "Found on disk"', async () => {
    const host = await mount('opm-trash-deletions', {
      batches: [makeTrashBatch({ reasons: [] })],
    });
    expect(text(cells(rows(host)[0] as Element)[4])).toBe('Found on disk');
  });

  it('is a table with a caption and named columns for assistive technology', async () => {
    const host = await mount('opm-trash-deletions', { batches: [makeTrashBatch()] });
    expect(text(host.querySelector('caption'))).toBe('Deletions, newest first');
    expect([...host.querySelectorAll('th')].map(text)).toEqual([
      'Deleted',
      'Volume',
      'Items',
      'Size',
      'Reason',
      'Actions',
    ]);
  });

  it('keeps a deletion of each volume that has the same batch name', async () => {
    const host = await mount('opm-trash-deletions', {
      batches: [makeTrashBatch(), makeTrashBatch({ volume_id: 2, volume: 'archive' })],
    });
    expect(rows(host)).toHaveLength(2);
  });

  it('asks to restore a whole deletion with the deletion as the detail, and the event bubbles', async () => {
    const batch = makeTrashBatch();
    const host = await mount('opm-trash-deletions', {
      batches: [makeTrashBatch({ batch: 'other' }), batch],
      volumes: [makeTrashVolume()],
    });
    const heard: TrashBatchDetail[] = [];
    const listen = (event: TrashBatchEvent) => heard.push(event.detail);
    document.body.addEventListener('trash-restore-batch', listen);
    try {
      button(rows(host)[1] as Element, 'Restore all').click();
    } finally {
      document.body.removeEventListener('trash-restore-batch', listen);
    }
    expect(heard).toEqual([{ batch }]);
  });

  it('asks to view the items of a deletion', async () => {
    const batch = makeTrashBatch();
    const host = await mount('opm-trash-deletions', { batches: [batch] });
    const heard: TrashBatchDetail[] = [];
    host.addEventListener('trash-view-batch', (event) => heard.push(event.detail));
    button(host, 'View items').click();
    expect(heard).toEqual([{ batch }]);
  });

  it('does not offer to restore a deletion on an offline volume, and says why', async () => {
    const host = await mount('opm-trash-deletions', {
      batches: [makeTrashBatch()],
      volumes: [makeTrashVolume({ online: false })],
    });
    const restore = button(host, 'Restore all');
    const why = 'Volume photos is offline: its items cannot be restored until it is back.';
    expect(restore.disabled).toBe(true);
    expect(restore.getAttribute('data-tooltip')).toBe(why);
    expect(restore.parentElement?.getAttribute('data-tooltip')).toBe(why);
    const heard = vi.fn();
    host.addEventListener('trash-restore-batch', heard);
    restore.click();
    expect(heard).not.toHaveBeenCalled();
    expect(button(host, 'View items').disabled).toBe(false);
  });

  it('does not offer to restore a deletion on a read-only volume, and says why', async () => {
    const host = await mount('opm-trash-deletions', {
      batches: [makeTrashBatch()],
      volumes: [makeTrashVolume({ writable: false })],
    });
    const restore = button(host, 'Restore all');
    expect(restore.disabled).toBe(true);
    expect(restore.getAttribute('data-tooltip')).toBe(
      'Volume photos is read-only: its items cannot be restored.',
    );
  });

  it('waits while a request is on its way, without blaming the volume', async () => {
    const host = await mount('opm-trash-deletions', {
      batches: [makeTrashBatch()],
      volumes: [makeTrashVolume()],
      busy: true,
    });
    const restore = button(host, 'Restore all');
    expect(restore.disabled).toBe(true);
    expect(restore.hasAttribute('title')).toBe(false);
    expect(button(host, 'View items').disabled).toBe(false);
  });

  it('restores a deletion on a volume the list does not know', async () => {
    const host = await mount('opm-trash-deletions', {
      batches: [makeTrashBatch({ volume_id: 9, volume: 'scans' })],
      volumes: [makeTrashVolume()],
    });
    expect(button(host, 'Restore all').disabled).toBe(false);
  });

  it('renders no row for no deletion', async () => {
    const host = await mount('opm-trash-deletions');
    expect(rows(host)).toHaveLength(0);
  });
});
