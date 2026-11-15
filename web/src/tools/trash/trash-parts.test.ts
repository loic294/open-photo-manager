import { render, type TemplateResult } from 'lit';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { makeTrashVolume } from '@/test/trash-fixtures';
import {
  renderEmpty,
  renderPager,
  renderReasonBadge,
  renderSkeleton,
  renderVolumeBadges,
  renderVolumeSelect,
  volumeOptions,
} from '@/tools/trash/trash-parts';

const containers: HTMLElement[] = [];

afterEach(() => {
  for (const container of containers.splice(0)) container.remove();
});

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

/** Draws a template into the page and waits for the elements inside it to render. */
async function show(template: TemplateResult): Promise<HTMLElement> {
  const container = document.createElement('div');
  document.body.append(container);
  containers.push(container);
  render(template, container);
  await Promise.all(
    [...container.querySelectorAll<HTMLElement & { updateComplete?: Promise<unknown> }>('*')].map(
      (element) => element.updateComplete,
    ),
  );
  return container;
}

const button = (container: ParentNode, label: string): HTMLButtonElement => {
  const found = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  if (!found) throw new Error(`No "${label}" button`);
  return found;
};

describe('renderReasonBadge', () => {
  it('says what the reason is and explains it in the tooltip', async () => {
    const badge = (await show(renderReasonBadge('user'))).querySelector('.badge');
    expect(text(badge)).toBe('Deleted');
    expect(badge?.getAttribute('data-tooltip')).toBe('Deleted by you');
  });

  it('calls an item that was found on disk by that name', async () => {
    const badge = (await show(renderReasonBadge(null))).querySelector('.badge');
    expect(text(badge)).toBe('Found on disk');
    expect(badge?.getAttribute('data-tooltip')).toContain('not trashed through this app');
  });

  it('shows a reason it does not know as it is', async () => {
    expect(text((await show(renderReasonBadge('archived'))).querySelector('.badge'))).toBe(
      'archived',
    );
  });
});

describe('volumeOptions', () => {
  it('lists all volumes first, then each volume that holds trash, marking the offline ones', () => {
    expect(
      volumeOptions([
        makeTrashVolume({ volume_id: 2, name: 'archive', online: false }),
        makeTrashVolume({ volume_id: 1, name: 'photos' }),
      ]),
    ).toEqual([
      { value: '', label: 'All volumes' },
      { value: '2', label: 'archive (offline)' },
      { value: '1', label: 'photos' },
    ]);
  });
});

describe('renderVolumeSelect', () => {
  const volumes = [
    makeTrashVolume({ volume_id: 1, name: 'photos' }),
    makeTrashVolume({ volume_id: 2, name: 'archive' }),
  ];

  const choose = (container: HTMLElement, value: string) => {
    const select = container.querySelector('select');
    if (!select) throw new Error('No select');
    select.value = value;
    select.dispatchEvent(new Event('change', { bubbles: true }));
  };

  it('is named "Volume" and shows the chosen volume', async () => {
    const container = await show(renderVolumeSelect(volumes, 2, () => undefined));
    const select = container.querySelector('select');
    expect(select?.getAttribute('aria-label')).toBe('Volume');
    expect(select?.value).toBe('2');
    expect([...(select?.options ?? [])].map((option) => option.text)).toEqual([
      'All volumes',
      'photos',
      'archive',
    ]);
  });

  it('shows all volumes when none is chosen', async () => {
    const container = await show(renderVolumeSelect(volumes, null, () => undefined));
    expect(container.querySelector('select')?.value).toBe('');
  });

  it('reports the volume id, and null for all volumes', async () => {
    const onChange = vi.fn();
    const container = await show(renderVolumeSelect(volumes, null, onChange));
    choose(container, '2');
    choose(container, '');
    expect(onChange.mock.calls).toEqual([[2], [null]]);
  });

  it('keeps its own select-change from bubbling to the page', async () => {
    const outer = vi.fn();
    const container = await show(renderVolumeSelect(volumes, null, () => undefined));
    container.addEventListener('select-change', outer);
    choose(container, '1');
    expect(outer).not.toHaveBeenCalled();
  });
});

describe('renderVolumeBadges', () => {
  it('shows one badge per volume with its size and the number of items in the tooltip', async () => {
    const container = await show(
      renderVolumeBadges([
        makeTrashVolume({ volume_id: 1, name: 'photos', items: 1, bytes: 2_048 }),
        makeTrashVolume({ volume_id: 2, name: 'archive', items: 1_250, bytes: 5_242_880 }),
      ]),
    );
    const badges = [...container.querySelectorAll('li')];
    expect(badges.map(text)).toEqual(['photos \u00b7 2 KB', 'archive \u00b7 5 MB']);
    expect(badges.map((badge) => badge.getAttribute('data-tooltip'))).toEqual([
      '1 item',
      '1,250 items',
    ]);
    expect(container.querySelector('ul')?.getAttribute('aria-label')).toBe('Volumes with trash');
  });

  it('warns about a volume that is offline or read-only', async () => {
    const container = await show(
      renderVolumeBadges([
        makeTrashVolume({ volume_id: 1, name: 'photos', online: false }),
        makeTrashVolume({ volume_id: 2, name: 'archive', writable: false }),
        makeTrashVolume({ volume_id: 3, name: 'scans' }),
      ]),
    );
    const badges = [...container.querySelectorAll('li')];
    expect(badges.map(text)).toEqual([
      'photos \u00b7 10 KB \u00b7 offline',
      'archive \u00b7 10 KB \u00b7 read-only',
      'scans \u00b7 10 KB',
    ]);
    expect(badges.map((badge) => badge.classList.contains('badge-warning'))).toEqual([
      true,
      true,
      false,
    ]);
    expect(badges.map((badge) => badge.querySelector('opm-icon')?.getAttribute('name'))).toEqual([
      'wifi-off',
      'lock',
      undefined,
    ]);
  });

  it('shows no badge when no volume holds trash', async () => {
    expect((await show(renderVolumeBadges([]))).querySelectorAll('li')).toHaveLength(0);
  });
});

describe('renderPager', () => {
  const state = { offset: 50, limit: 50, total: 120, count: 50 };

  it('says which items the page shows and how many there are', async () => {
    const container = await show(renderPager(state, () => undefined));
    expect(text(container.querySelector('[aria-live]'))).toBe('51\u2013100 of 120');
    expect(container.querySelector('nav')?.getAttribute('aria-label')).toBe('Pages');
  });

  it('goes to the previous and the next page', async () => {
    const onPage = vi.fn();
    const container = await show(renderPager(state, onPage));
    button(container, 'Previous page').click();
    button(container, 'Next page').click();
    expect(onPage.mock.calls).toEqual([[0], [100]]);
  });

  it('cannot go back from the first page', async () => {
    const container = await show(renderPager({ ...state, offset: 0 }, () => undefined));
    expect(button(container, 'Previous page').disabled).toBe(true);
    expect(button(container, 'Next page').disabled).toBe(false);
  });

  it('cannot go on from the last page', async () => {
    const container = await show(
      renderPager({ offset: 100, limit: 50, total: 120, count: 20 }, () => undefined),
    );
    expect(button(container, 'Previous page').disabled).toBe(false);
    expect(button(container, 'Next page').disabled).toBe(true);
  });

  it('does not go back past the first item when a page is partial', async () => {
    const onPage = vi.fn();
    const container = await show(
      renderPager({ offset: 20, limit: 50, total: 120, count: 50 }, onPage),
    );
    button(container, 'Previous page').click();
    expect(onPage).toHaveBeenCalledWith(0);
  });

  it('says there is nothing to page through when the list is empty', async () => {
    const container = await show(
      renderPager({ offset: 0, limit: 50, total: 0, count: 0 }, () => undefined),
    );
    expect(text(container.querySelector('[aria-live]'))).toBe('No items');
    expect(button(container, 'Previous page').disabled).toBe(true);
    expect(button(container, 'Next page').disabled).toBe(true);
  });
});

describe('renderSkeleton', () => {
  it('is a status named after what is loading', async () => {
    const status = (await show(renderSkeleton('Loading items'))).querySelector('[role="status"]');
    expect(status?.getAttribute('aria-label')).toBe('Loading items');
    expect(status?.querySelectorAll('.skeleton').length).toBeGreaterThan(0);
  });
});

describe('renderEmpty', () => {
  it('says the trash is empty, with the hint under it', async () => {
    const container = await show(
      renderEmpty(false, () => undefined, 'Items stay in the trash until you empty it.'),
    );
    expect(text(container.querySelector('p.font-medium'))).toBe('The trash is empty');
    expect(text(container)).toContain('Items stay in the trash until you empty it.');
    expect(container.querySelector('button')).toBeNull();
  });

  it('leaves out the hint line when there is none', async () => {
    const container = await show(renderEmpty(false, () => undefined));
    expect(container.querySelectorAll('p')).toHaveLength(1);
  });

  it('says nothing matches when filters hide every item, and clears them on request', async () => {
    const onClear = vi.fn();
    const container = await show(renderEmpty(true, onClear, 'Not shown'));
    expect(text(container.querySelector('p.font-medium'))).toBe('Nothing matches');
    expect(text(container)).toContain('No item in the trash matches these filters.');
    expect(text(container)).not.toContain('Not shown');
    const clear = container.querySelector('button');
    expect(text(clear)).toBe('Clear filters');
    clear?.click();
    expect(onClear).toHaveBeenCalledTimes(1);
  });
});
