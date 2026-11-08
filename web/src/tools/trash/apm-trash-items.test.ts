import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@/test/helpers';
import { makeTrashItem, makeTrashVolume, TRASHED_AT } from '@/test/trash-fixtures';
import type { ApmTrashItems } from '@/tools/trash/apm-trash-items';
import { formatFileTime } from '@/util/format';
import '@/tools/trash/apm-trash-items';

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

const rows = (host: HTMLElement) => [...host.querySelectorAll('tbody tr')];

function button(scope: ParentNode, label: string): HTMLButtonElement {
  const found = [...scope.querySelectorAll<HTMLButtonElement>('button')].find(
    (candidate) => text(candidate) === label,
  );
  if (!found) throw new Error(`No "${label}" button`);
  return found;
}

function labelled<T extends HTMLElement>(scope: ParentNode, label: string): T {
  const found = scope.querySelector<T>(`[aria-label="${label}"]`);
  if (!found) throw new Error(`Nothing is labelled "${label}"`);
  return found;
}

/** The details of every `name` event the host sends, in order. */
function record(host: HTMLElement, name: keyof GlobalEventHandlersEventMap): unknown[] {
  const heard: unknown[] = [];
  host.addEventListener(name, (event) => heard.push((event as CustomEvent).detail));
  return heard;
}

const photos = makeTrashVolume({ volume_id: 1, name: 'photos' });
const archive = makeTrashVolume({ volume_id: 2, name: 'archive' });

const page = () => [
  makeTrashItem({ id: 1, name: 'IMG_0001.jpg', size: 2_048 }),
  makeTrashItem({ id: 2, name: 'IMG_0002.jpg', size: 4_096 }),
  makeTrashItem({
    id: 3,
    name: 'Trip',
    dir: '2024',
    is_dir: true,
    size: 1_048_576,
    reason: 'replaced',
  }),
];

async function mountItems(props: Partial<ApmTrashItems> = {}): Promise<ApmTrashItems> {
  return mount('apm-trash-items', {
    items: page(),
    volumes: [photos],
    total: 3,
    ...props,
  });
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime((TRASHED_AT + 3 * 86_400) * 1000);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('apm-trash-items rows', () => {
  it('shows what each item is, where it came from, how big it is and when it was deleted', async () => {
    const host = await mountItems();
    const [first, , folder] = rows(host);
    const cells = (row: Element | undefined) =>
      [...(row?.querySelectorAll('td') ?? [])].slice(1, 7).map(text);
    expect(cells(first)).toEqual([
      'IMG_0001.jpg',
      '2024/Trip',
      'photos',
      '2 KB',
      '3 days ago',
      'Deleted',
    ]);
    expect(cells(folder)).toEqual(['Trip', '2024', 'photos', '1 MB', '3 days ago', 'Replaced']);
  });

  it('puts the full name and the exact time in tooltips', async () => {
    const host = await mountItems();
    const [first] = rows(host);
    expect(first?.querySelector('td:nth-child(2) .truncate')?.getAttribute('title')).toBe(
      'IMG_0001.jpg',
    );
    expect(first?.querySelector('td:nth-child(6) span')?.getAttribute('title')).toBe(
      formatFileTime(TRASHED_AT),
    );
  });

  it('draws a folder differently from a file', async () => {
    const host = await mountItems();
    const icon = (row: Element | undefined) =>
      row?.querySelector('td:nth-child(2) apm-icon')?.getAttribute('name');
    expect(rows(host).map(icon)).toEqual(['file', 'file', 'folder']);
  });

  it('calls the root of a volume "Volume root"', async () => {
    const host = await mountItems({ items: [makeTrashItem({ dir: '' })] });
    expect(text(rows(host)[0]?.querySelector('td:nth-child(3)'))).toBe('Volume root');
  });

  it('says a reason it knows in words, and an item found on disk as such', async () => {
    const host = await mountItems({
      items: [makeTrashItem({ id: 1, reason: 'moved' }), makeTrashItem({ id: 2, reason: null })],
    });
    expect(rows(host).map((row) => text(row.querySelector('.badge')))).toEqual([
      'Moved',
      'Found on disk',
    ]);
  });

  it('keeps the rows in the order it was given', async () => {
    const host = await mountItems({ items: [...page()].reverse() });
    expect(rows(host).map((row) => text(row.querySelector('td:nth-child(2)')))).toEqual([
      'Trip',
      'IMG_0002.jpg',
      'IMG_0001.jpg',
    ]);
  });
});

describe('apm-trash-items sorting', () => {
  const ariaSorts = (host: HTMLElement) =>
    Object.fromEntries(
      [...host.querySelectorAll('th[aria-sort]')].map((th) => [
        text(th),
        th.getAttribute('aria-sort'),
      ]),
    );

  it('says which column the list is sorted by and in which direction', async () => {
    const descending = await mountItems({ sort: 'trashed_at', order: 'desc' });
    expect(ariaSorts(descending)).toEqual({
      Name: 'none',
      'Original folder': 'none',
      Size: 'none',
      Deleted: 'descending',
    });
    const ascending = await mountItems({ sort: 'name', order: 'asc' });
    expect(ariaSorts(ascending)).toMatchObject({ Name: 'ascending', Deleted: 'none' });
  });

  it('shows the direction on the sorted column and a neutral arrow on the others', async () => {
    const host = await mountItems({ sort: 'size', order: 'asc' });
    const arrow = (label: string) =>
      [...host.querySelectorAll('th[aria-sort]')]
        .find((th) => text(th) === label)
        ?.querySelector('apm-icon')
        ?.getAttribute('name');
    expect(arrow('Size')).toBe('arrow-up');
    expect(arrow('Name')).toBe('arrow-up-down');
  });

  it('asks to sort by the column that is clicked', async () => {
    const host = await mountItems();
    const heard = record(host, 'trash-sort');
    for (const label of ['Name', 'Original folder', 'Size', 'Deleted']) button(host, label).click();
    expect(heard).toEqual([
      { sort: 'name' },
      { sort: 'path' },
      { sort: 'size' },
      { sort: 'trashed_at' },
    ]);
  });
});

describe('apm-trash-items selection', () => {
  const rowBox = (host: HTMLElement, name: string) =>
    labelled<HTMLInputElement>(host, `Select ${name}`);
  const headerBox = (host: HTMLElement) =>
    labelled<HTMLInputElement>(host, 'Select all items on this page');

  it('checks the rows that are selected', async () => {
    const host = await mountItems({ selected: new Set([2]) });
    expect(
      ['IMG_0001.jpg', 'IMG_0002.jpg', 'Trip'].map((name) => rowBox(host, name).checked),
    ).toEqual([false, true, false]);
  });

  it('reports a row that is ticked and unticked', async () => {
    const host = await mountItems();
    const heard = record(host, 'trash-select');
    rowBox(host, 'IMG_0002.jpg').click();
    rowBox(host, 'IMG_0002.jpg').click();
    expect(heard).toEqual([
      { id: 2, selected: true },
      { id: 2, selected: false },
    ]);
  });

  it('shows the header checkbox as partial when only some of the page is selected', async () => {
    const host = await mountItems({ selected: new Set([1]) });
    expect(headerBox(host).checked).toBe(false);
    expect(headerBox(host).indeterminate).toBe(true);
  });

  it('shows the header checkbox as checked when the whole page is selected', async () => {
    const host = await mountItems({ selected: new Set([1, 2, 3]) });
    expect(headerBox(host).checked).toBe(true);
    expect(headerBox(host).indeterminate).toBe(false);
  });

  it('shows the header checkbox as empty when nothing is selected', async () => {
    const host = await mountItems();
    expect(headerBox(host).checked).toBe(false);
    expect(headerBox(host).indeterminate).toBe(false);
  });

  it('asks to select the page, and to unselect it', async () => {
    const host = await mountItems();
    const heard = record(host, 'trash-select-page');
    headerBox(host).click();
    headerBox(host).click();
    expect(heard).toEqual([{ selected: true }, { selected: false }]);
  });

  it('counts the selection on "Restore selected" and sends it in page order', async () => {
    const host = await mountItems({ selected: new Set([3, 1]) });
    const heard = record(host, 'trash-restore-items');
    const restore = button(host, 'Restore selected (2)');
    expect(restore.disabled).toBe(false);
    restore.click();
    expect(heard).toEqual([{ ids: [1, 3] }]);
  });

  it('counts only the selected items that are on the page', async () => {
    const host = await mountItems({ selected: new Set([2, 40]) });
    expect(button(host, 'Restore selected (1)')).toBeDefined();
  });

  it('cannot restore a selection of none', async () => {
    const host = await mountItems();
    expect(button(host, 'Restore selected (0)').disabled).toBe(true);
  });

  it('waits for the request that is on its way', async () => {
    const host = await mountItems({ selected: new Set([1]), busy: true });
    expect(button(host, 'Restore selected (1)').disabled).toBe(true);
    expect(button(rows(host)[0] as Element, 'Restore').disabled).toBe(true);
  });
});

describe('apm-trash-items restoring one item', () => {
  it('asks to restore the item of the row', async () => {
    const host = await mountItems();
    const heard = record(host, 'trash-restore-items');
    button(rows(host)[1] as Element, 'Restore').click();
    expect(heard).toEqual([{ ids: [2] }]);
  });
});

describe('apm-trash-items on a volume that cannot be changed', () => {
  const offlineItem = makeTrashItem({ id: 5, volume_id: 2, volume: 'archive', name: 'Old.jpg' });

  it('does not let an item of an offline volume be selected or restored, and says why', async () => {
    const host = await mountItems({
      items: [makeTrashItem({ id: 1 }), offlineItem],
      volumes: [photos, { ...archive, online: false }],
    });
    const why = 'Volume archive is offline: its items cannot be restored until it is back.';
    const box = labelled<HTMLInputElement>(host, 'Select Old.jpg');
    const restore = button(rows(host)[1] as Element, 'Restore');
    expect(box.disabled).toBe(true);
    expect(box.getAttribute('title')).toBe(why);
    expect(restore.disabled).toBe(true);
    expect(restore.getAttribute('title')).toBe(why);
    expect(restore.parentElement?.getAttribute('title')).toBe(why);
    expect(labelled<HTMLInputElement>(host, 'Select IMG_0001.jpg').disabled).toBe(false);
  });

  it('does not let an item of a read-only volume be restored, and says why', async () => {
    const host = await mountItems({
      items: [offlineItem],
      volumes: [{ ...archive, writable: false }],
    });
    const restore = button(rows(host)[0] as Element, 'Restore');
    expect(restore.disabled).toBe(true);
    expect(restore.getAttribute('title')).toBe(
      'Volume archive is read-only: its items cannot be restored.',
    );
  });

  it('leaves the items it cannot restore out of the header checkbox', async () => {
    const host = await mountItems({
      items: [makeTrashItem({ id: 1 }), offlineItem],
      volumes: [photos, { ...archive, online: false }],
      selected: new Set([1]),
    });
    const header = labelled<HTMLInputElement>(host, 'Select all items on this page');
    expect(header.checked).toBe(true);
    expect(header.indeterminate).toBe(false);
  });

  it('disables the header checkbox when nothing on the page can be selected', async () => {
    const host = await mountItems({
      items: [offlineItem],
      volumes: [{ ...archive, online: false }],
    });
    expect(labelled<HTMLInputElement>(host, 'Select all items on this page').disabled).toBe(true);
  });

  it('treats an item of a volume the list does not know as restorable', async () => {
    const host = await mountItems({ volumes: [] });
    expect(button(rows(host)[0] as Element, 'Restore').disabled).toBe(false);
  });
});

describe('apm-trash-items toolbar', () => {
  const searchBox = (host: HTMLElement) => labelled<HTMLInputElement>(host, 'Search the trash');

  it('shows the text of the search and sends every change of it', async () => {
    const host = await mountItems({ search: 'holiday' });
    const box = searchBox(host);
    expect(box.value).toBe('holiday');
    expect(box.placeholder).toBe('Search by path');
    const heard = record(host, 'trash-search');
    box.value = 'holiday 2';
    box.dispatchEvent(new Event('input', { bubbles: true }));
    box.value = '';
    box.dispatchEvent(new Event('input', { bubbles: true }));
    expect(heard).toEqual([{ value: 'holiday 2' }, { value: '' }]);
  });

  it('filters by volume, with null for all volumes', async () => {
    const host = await mountItems({ volumes: [photos, archive], volumeId: 1 });
    const select = labelled<HTMLSelectElement>(host, 'Volume');
    expect(select.value).toBe('1');
    const heard = record(host, 'trash-volume');
    select.value = '2';
    select.dispatchEvent(new Event('change', { bubbles: true }));
    select.value = '';
    select.dispatchEvent(new Event('change', { bubbles: true }));
    expect(heard).toEqual([{ volumeId: 2 }, { volumeId: null }]);
  });

  it('shows the deletion the list is narrowed to, and lets the user show every deletion', async () => {
    const host = await mountItems({ batchLabel: 'Deletion of 1 Jan 2025 on photos' });
    expect(text(host.querySelector('.badge'))).toBe('Deletion of 1 Jan 2025 on photos');
    const heard = record(host, 'trash-clear-batch');
    const close = labelled<HTMLButtonElement>(host, 'Show every deletion');
    expect(close.getAttribute('title')).toBe('Show every deletion');
    close.click();
    expect(heard).toHaveLength(1);
  });

  it('shows no deletion chip when the list is not narrowed to a deletion', async () => {
    const host = await mountItems({ batchLabel: null });
    expect(host.querySelector('[aria-label="Show every deletion"]')).toBeNull();
  });
});

describe('apm-trash-items states', () => {
  it('shows placeholders instead of rows during the first load', async () => {
    const host = await mountItems({ loading: true });
    expect(host.querySelector('[role="status"]')?.getAttribute('aria-label')).toBe('Loading items');
    expect(host.querySelector('table')).toBeNull();
  });

  it('says the trash is empty, with the retention rule under it', async () => {
    const host = await mountItems({
      items: [],
      total: 0,
      emptyHint: 'Items stay in the trash until you empty it.',
    });
    expect(text(host)).toContain('The trash is empty');
    expect(text(host)).toContain('Items stay in the trash until you empty it.');
    expect(host.querySelector('table')).toBeNull();
  });

  it('says nothing matches when filters hide every item, and clears them on request', async () => {
    const host = await mountItems({ items: [], total: 0, filtered: true });
    expect(text(host)).toContain('Nothing matches');
    expect(text(host)).not.toContain('The trash is empty');
    const heard = record(host, 'trash-clear-filters');
    button(host, 'Clear filters').click();
    expect(heard).toHaveLength(1);
  });

  it('keeps the search box and the volume filter when nothing matches', async () => {
    const host = await mountItems({ items: [], total: 0, filtered: true, search: 'zzz' });
    expect(labelled<HTMLInputElement>(host, 'Search the trash').value).toBe('zzz');
    expect(host.querySelector('[aria-label="Volume"]')).not.toBeNull();
  });
});

describe('apm-trash-items pager', () => {
  it('says which items the page shows', async () => {
    const host = await mountItems({ offset: 50, total: 120 });
    expect(text(host.querySelector('[aria-live]'))).toBe('51\u201353 of 120');
  });

  it('asks for the page before and the page after', async () => {
    const host = await mountItems({ offset: 50, total: 120 });
    const heard = record(host, 'trash-page');
    labelled<HTMLButtonElement>(host, 'Previous page').click();
    labelled<HTMLButtonElement>(host, 'Next page').click();
    expect(heard).toEqual([{ offset: 0 }, { offset: 100 }]);
  });

  it('pages by its limit', async () => {
    const host = await mountItems({ offset: 0, limit: 2, total: 3 });
    const heard = record(host, 'trash-page');
    labelled<HTMLButtonElement>(host, 'Next page').click();
    expect(heard).toEqual([{ offset: 2 }]);
  });

  it('cannot go before the first page or after the last one', async () => {
    const only = await mountItems({ offset: 0, total: 3 });
    expect(labelled<HTMLButtonElement>(only, 'Previous page').disabled).toBe(true);
    expect(labelled<HTMLButtonElement>(only, 'Next page').disabled).toBe(true);
    const last = await mountItems({ offset: 50, total: 53 });
    expect(labelled<HTMLButtonElement>(last, 'Previous page').disabled).toBe(false);
    expect(labelled<HTMLButtonElement>(last, 'Next page').disabled).toBe(true);
  });
});
