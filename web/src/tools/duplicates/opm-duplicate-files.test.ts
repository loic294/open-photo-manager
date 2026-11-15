import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { events } from '@/api/sse';
import * as viewer from '@/components/opm-viewer';
import { setLocation } from '@/stores/nav';
import { saveSettings } from '@/stores/settings';
import { loadVolumes, resetVolumesForTests } from '@/stores/volumes';
import { fileItem } from '@/test/file-item';
import { mount } from '@/test/helpers';
import { sampleSettings } from '@/test/settings-fixtures';
import { matchMode, strictPrefix } from './duplicates-state';
import { defaultRules } from './file-selection';
import './opm-duplicate-files';

const groups = [
  {
    files: [1, 2, 3].map((id) => ({
      item: fileItem({
        id,
        volume_id: id === 1 ? 1 : 2,
        dir: id === 1 ? 'trip/day' : 'backup',
        size: id * 100,
      }),
      btime: null,
      has_description: id === 2,
      tags: id === 2 ? ['trip'] : [],
    })),
    pairs: [
      [1, 2],
      [1, 3],
      [2, 3],
    ],
  },
];

beforeEach(async () => {
  vi.spyOn(api, 'get').mockImplementation(async (path) => {
    if (path === '/api/volumes')
      return [
        { id: 1, name: 'photos' },
        { id: 2, name: 'archive' },
      ];
    if (path.endsWith('/duplicate-files')) return { groups };
    throw new Error(`Unexpected GET ${path}`);
  });
  vi.spyOn(api, 'post').mockImplementation(async (path) => {
    if (path === '/api/plans') return { id: 5, state: 'ready', stats: {}, warnings: [] };
    if (path === '/api/browse/folders')
      return { folders: [], counts: { in_folder: 0, in_subfolders: 0 } };
    throw new Error(`Unexpected POST ${path}`);
  });
  await loadVolumes();
  setLocation('source', { volume: 'photos', dir: 'trip' });
  matchMode.value = 'hash';
  strictPrefix.value = false;
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it('preserves image aspect ratios and explains selections on hover', async () => {
  vi.mocked(api.get).mockResolvedValueOnce({
    groups: groups.map((group) => ({
      ...group,
      files: group.files.map((file) => ({
        ...file,
        item: { ...file.item, width: 6000, height: 4000 },
      })),
    })),
  });
  const el = await searched();
  const card = el.querySelector('opm-file-card');
  expect(card?.parentElement?.style.aspectRatio).toBe('6000 / 4000');
  expect(card?.parentElement?.getAttribute('data-tooltip')).toContain('lowest file ID');
  card?.dispatchEvent(new CustomEvent('card-activate', { bubbles: true }));
  await settle(el);
  expect(card?.parentElement?.getAttribute('data-tooltip')).toBe('Keep: Manual selection');
  expect(el.textContent).toContain('0 files with duplicates in folder');
  expect(el.textContent).toContain('1 in subfolders');
});

it('shows readable folder paths and bolds only differing segments across copies', async () => {
  vi.mocked(api.get).mockResolvedValueOnce({
    groups: groups.map((group) => ({
      ...group,
      files: group.files.map((file, index) => ({
        ...file,
        item: { ...file.item, volume_id: 1, dir: index === 1 ? 'backup/day' : 'trip/day' },
      })),
    })),
  });
  const el = await searched();
  const paths = [...el.querySelectorAll('[data-copy-path]')];
  expect(paths.map((path) => path.textContent)).toEqual([
    'photos/trip/day',
    'photos/backup/day',
    'photos/trip/day',
  ]);
  expect(
    paths.map((path) => [...path.querySelectorAll('strong')].map((part) => part.textContent)),
  ).toEqual([['trip'], ['backup'], ['trip']]);
  expect(paths.every((path) => path.classList.contains('text-base'))).toBe(true);
  expect(paths[0]?.getAttribute('data-tooltip')).toContain('photos/trip/day/');
});

it('appends groups when the sentinel enters view and preserves earlier choices', async () => {
  let intersect = (_entries: { isIntersecting: boolean }[]) => {};
  const disconnect = vi.fn();
  vi.stubGlobal(
    'IntersectionObserver',
    class {
      constructor(callback: typeof intersect, options: IntersectionObserverInit) {
        intersect = callback;
        expect(options.root instanceof Element && options.root.hasAttribute('data-groups')).toBe(
          true,
        );
      }
      observe() {}
      disconnect = disconnect;
    },
  );
  vi.mocked(api.get).mockResolvedValueOnce({
    groups: Array.from({ length: 45 }, (_, index) => ({
      files: groups[0].files.map((file) => ({
        ...file,
        item: { ...file.item, id: file.item.id + index * 3 },
      })),
      pairs: groups[0].pairs.map((pair) => pair.map((id) => id + index * 3)),
    })),
  });
  const el = await searched();
  const first = el.querySelector('article');
  expect(el.querySelectorAll('article')).toHaveLength(20);
  el.querySelectorAll('opm-file-card')[1]?.dispatchEvent(
    new CustomEvent('card-activate', { bubbles: true }),
  );
  await settle(el);
  intersect([{ isIntersecting: true }]);
  await settle(el);
  expect(el.querySelectorAll('article')).toHaveLength(40);
  expect(el.querySelector('article')).toBe(first);
  expect(first?.querySelectorAll('opm-file-card')[1]?.selected).toBe(true);
  button(el, 'Load more groups').click();
  await settle(el);
  expect(el.querySelectorAll('article')).toHaveLength(45);
  expect(el.querySelector('[data-load-more]')).toBeNull();
  expect(disconnect).toHaveBeenCalled();
});

it('toggles additional keepers with Cmd, Ctrl and checkboxes, while a plain click replaces them', async () => {
  const el = await searched();
  const cards = [...el.querySelectorAll('opm-file-card')];
  const activate = async (index: number, options: MouseEventInit = {}, checkbox = false) => {
    cards[index]
      ?.querySelector(checkbox ? '[data-checkbox]' : '[role="button"]')
      ?.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, ...options }));
    await settle(el);
  };
  const selected = () => cards.map((card) => card.selected);
  await activate(1, { metaKey: true });
  expect(selected()).toEqual([true, true, false]);
  await activate(2, { ctrlKey: true });
  expect(selected()).toEqual([true, true, true]);
  await activate(1, {}, true);
  expect(selected()).toEqual([true, false, true]);
  await activate(2);
  expect(selected()).toEqual([false, false, true]);
  await activate(2, {}, true);
  expect(selected().some(Boolean)).toBe(true);
});

it('opens a copy in the shared viewer without changing keepers and rejects stale review changes', async () => {
  const open = vi.spyOn(viewer, 'openViewer').mockResolvedValue();
  const el = await searched();
  el.querySelector<HTMLButtonElement>('[aria-label^="View full size:"]')?.click();
  const source = open.mock.calls[0]?.[0];
  expect(source?.startId).toBe(1);
  expect(await source?.loadIds(new AbortController().signal)).toEqual([1, 2, 3]);
  expect(source?.review?.isKept(1)).toBe(true);
  expect(source?.review?.isKept(2)).toBe(false);
  source?.review?.choose(2, true);
  await settle(el);
  expect([...el.querySelectorAll('opm-file-card')].map((card) => card.selected)).toEqual([
    true,
    true,
    false,
  ]);
  events.emit({ type: 'fs_change', volume_id: 2, dirs: ['backup'] });
  await settle(el);
  expect(source?.review?.disabled()).toBe(true);
  source?.review?.choose(3, false);
  await settle(el);
  expect(source?.review?.isKept(3)).toBe(false);
});

it('overlays keep status and file size without nesting the fullscreen button in a card', async () => {
  const el = await searched();
  const card = el.querySelector('opm-file-card');
  const thumbnail = card?.parentElement;
  expect(card?.overlayControls).toBe(true);
  expect(thumbnail?.querySelector('[data-copy-status]')?.textContent).toBe('Keep');
  expect(thumbnail?.querySelector('[data-copy-size]')?.textContent).toBe('100 B');
  expect(thumbnail?.querySelector('[data-copy-size]')?.getAttribute('data-tooltip')).toBe(
    '100 bytes',
  );
  expect(card?.querySelector('[aria-label^="View full size:"]')).toBeNull();
  expect(thumbnail?.querySelector('button[aria-label^="View full size:"]')).not.toBeNull();
});

it('compares dates with the latest retained selection and falls back when it is unchecked', async () => {
  vi.mocked(api.get).mockResolvedValueOnce({
    groups: groups.map((group) => ({
      ...group,
      files: group.files.map((file, index) => ({
        ...file,
        item: { ...file.item, capture_ts: 1700000000 + index * 61 },
      })),
    })),
  });
  const el = await searched();
  const cards = [...el.querySelectorAll('opm-file-card')];
  const differences = () =>
    [...el.querySelectorAll('[data-copy-date]')].map(
      (date) => date.parentElement?.querySelector('[data-date-difference]')?.textContent ?? null,
    );
  const choose = async (index: number, toggle = false) => {
    cards[index]?.dispatchEvent(
      new CustomEvent('card-activate', {
        bubbles: true,
        detail: { toggle },
      }),
    );
    await settle(el);
  };
  expect(differences()).toEqual([null, '+1m 1s', '+2m 2s']);
  await choose(1, true);
  expect(differences()).toEqual(['-1m 1s', null, '+1m 1s']);
  await choose(2, true);
  expect(differences()).toEqual(['-2m 2s', '-1m 1s', null]);
  await choose(2, true);
  expect(differences()).toEqual(['-1m 1s', null, '+1m 1s']);
  await choose(2);
  expect(cards.map((card) => card.selected)).toEqual([false, false, true]);
  expect(differences()).toEqual(['-2m 2s', '-1m 1s', null]);
});

it('omits equal-date offsets and reports missing dates without inventing a difference', async () => {
  vi.mocked(api.get).mockResolvedValueOnce({
    groups: groups.map((group) => ({
      ...group,
      files: group.files.map((file, index) => ({
        ...file,
        item: { ...file.item, capture_ts: index === 2 ? null : 1700000000 },
      })),
    })),
  });
  const el = await searched();
  expect(
    [...el.querySelectorAll('[data-date-difference]')].map((badge) => badge.textContent),
  ).toEqual(['Unknown difference']);
  const cards = [...el.querySelectorAll('opm-file-card')];
  cards[2]?.dispatchEvent(new CustomEvent('card-activate', { bubbles: true }));
  await settle(el);
  expect(
    [...el.querySelectorAll('[data-date-difference]')].map((badge) => badge.textContent),
  ).toEqual(['No reference date', 'No reference date']);
});

it('skips groups locally and explicitly retains their files in execution options', async () => {
  const second = {
    files: groups[0].files.map((file) => ({
      ...file,
      item: { ...file.item, id: file.item.id + 3 },
    })),
    pairs: groups[0].pairs.map((pair) => pair.map((id) => id + 3)),
  };
  vi.mocked(api.get).mockResolvedValueOnce({ groups: [...groups, second] });
  const el = await searched();
  el.querySelector<HTMLButtonElement>('[aria-label="Skip duplicate match"]')?.click();
  await settle(el);
  expect(el.querySelectorAll('article')).toHaveLength(1);
  expect(el.textContent).toContain('4 to keep');
  expect(el.textContent).toContain('Skips reset on the next search');

  button(el, 'Automatic Selection').click();
  await settle(el);
  el.querySelector('opm-duplicate-selection')?.dispatchEvent(new Event('close'));
  button(el, 'Trash 2 duplicates').click();
  await settle(el);
  const confirm = [...document.querySelectorAll('opm-modal')].find(
    (modal) => modal.heading === 'Trash 2 duplicate files?',
  );
  expect(confirm).toBeDefined();
  vi.mocked(api.post).mockResolvedValueOnce({ id: 8, title: 'Trash duplicates' });
  button(confirm as Element, 'Trash 2 files').click();
  await settle(el);
  expect(api.post).toHaveBeenCalledWith(
    '/api/plans/5/execute',
    expect.objectContaining({
      body: { options: { keep: expect.arrayContaining([1, 2, 3, 4]) }, only: null },
    }),
  );
});

it('restores skipped groups with their selections and resets skips for a fresh search', async () => {
  const el = await searched();
  el.querySelector<HTMLButtonElement>('[aria-label="Skip duplicate match"]')?.click();
  await settle(el);
  expect(button(el, 'Trash 0 duplicates').disabled).toBe(true);
  button(el, 'Show skipped groups').click();
  await settle(el);
  expect(el.querySelectorAll('article')).toHaveLength(1);
  expect(el.querySelector('opm-file-card')?.selected).toBe(true);
  el.querySelector<HTMLButtonElement>('[aria-label="Skip duplicate match"]')?.click();
  await settle(el);
  button(el, 'Find duplicates').click();
  await settle(el);
  expect(el.querySelectorAll('article')).toHaveLength(1);
  expect(el.textContent).not.toContain('groups skipped');
});

async function settle(el: Element) {
  for (let i = 0; i < 10; i++) {
    await Promise.resolve();
    await Promise.all(
      [el, ...el.querySelectorAll('*')].map((child) =>
        'updateComplete' in child ? child.updateComplete : undefined,
      ),
    );
  }
}
function button(el: Element, text: string) {
  const target = [...el.querySelectorAll('button')].find(
    (entry) => entry.textContent?.trim() === text,
  );
  expect(target, text).toBeDefined();
  return target as HTMLButtonElement;
}
async function searched() {
  const el = await mount('opm-duplicate-files');
  await settle(el);
  button(el, 'Find duplicates').click();
  await settle(el);
  return el;
}

it('places all favorable rule badges beside keeper filenames and refreshes them after manual choices', async () => {
  const el = await searched();
  const rules = defaultRules().map((rule) => ({
    ...rule,
    enabled: rule.kind === 'description' || rule.kind === 'tags',
  }));
  el.querySelector('opm-duplicate-selection')?.dispatchEvent(
    new CustomEvent('selection-rules', { detail: rules }),
  );
  await settle(el);
  const names = el.querySelectorAll('[data-copy-name]');
  const badges = () => [...(names[1]?.parentElement?.querySelectorAll('[data-keeper-rule]') ?? [])];
  expect(badges().map((badge) => badge.getAttribute('data-keeper-rule'))).toEqual([
    'description',
    'tags',
  ]);
  expect(badges()[0]?.getAttribute('data-tooltip')).toContain('Decisive');
  expect(badges()[1]?.getAttribute('data-tooltip')).toContain('Supporting');
  expect(names[0]?.parentElement?.querySelector('[data-keeper-rule]')).toBeNull();
  el.querySelector('opm-file-card')?.dispatchEvent(
    new CustomEvent('card-activate', { bubbles: true }),
  );
  await settle(el);
  expect(names[0]?.parentElement?.querySelector('[data-keeper-rule="manual"]')?.textContent).toBe(
    'Manual selection',
  );
  expect(badges()).toEqual([]);
});

it('waits for the source volume instead of accidentally searching all volumes during startup', async () => {
  resetVolumesForTests();
  const el = await mount('opm-duplicate-files');
  await settle(el);
  expect(button(el, 'Find duplicates').disabled).toBe(true);
  await loadVolumes();
  await settle(el);
  expect(button(el, 'Find duplicates').disabled).toBe(false);
  button(el, 'Find duplicates').click();
  await settle(el);
  expect(api.post).toHaveBeenCalledWith(
    '/api/plans',
    expect.objectContaining({
      body: expect.objectContaining({
        params: expect.objectContaining({
          matching: expect.objectContaining({
            source: expect.objectContaining({
              scope: { volume_id: 1, dir: 'trip', recursive: true },
            }),
          }),
        }),
      }),
    }),
  );
});

it('searches globally from the source, renders outside copies and changes the keeper by clicking', async () => {
  const el = await searched();
  expect(api.post).toHaveBeenCalledWith(
    '/api/plans',
    expect.objectContaining({
      body: {
        kind: 'duplicates',
        replaces: null,
        params: {
          matching: {
            mode: 'hash',
            strict_prefix: false,
            source: {
              scope: { volume_id: 1, dir: 'trip', recursive: true },
              filter: expect.any(Object),
            },
            comparison: {
              scope: { volume_id: null, dir: '', recursive: true },
              filter: expect.any(Object),
            },
          },
          excluded: [],
        },
      },
    }),
  );
  const cards = [...el.querySelectorAll('opm-file-card')];
  expect(cards).toHaveLength(3);
  expect(cards[0]?.querySelector('.border-success')).not.toBeNull();
  expect(cards[1]?.querySelector('.border-error')).not.toBeNull();
  expect(el.textContent).toContain('archive/backup');
  expect(
    [...el.querySelectorAll('[data-copy-name]')].every(
      (field) => !field.classList.contains('font-bold'),
    ),
  ).toBe(true);
  expect(
    [...el.querySelectorAll('[data-copy-size]')].every((field) =>
      field.classList.contains('font-bold'),
    ),
  ).toBe(true);
  expect(
    [...el.querySelectorAll('[data-copy-date]')].every(
      (field) => !field.classList.contains('font-bold'),
    ),
  ).toBe(true);
  cards[1]?.querySelector<HTMLElement>('[role="button"]')?.click();
  await settle(el);
  expect(cards[1]?.querySelector('[aria-pressed="true"].border-success')).not.toBeNull();
  expect(cards[0]?.querySelector('.border-error')).not.toBeNull();
  const slider = el.querySelector<HTMLInputElement>('[aria-label="Duplicate thumbnail size"]');
  if (!slider) throw new Error('Missing size slider');
  slider.value = '400';
  slider.dispatchEvent(new Event('input'));
  await settle(el);
  expect(cards[1]?.size).toBe(400);
});

it('bolds differing filenames and capture dates, including missing dates, while equal sizes stay regular', async () => {
  vi.mocked(api.get).mockResolvedValueOnce({
    groups: [
      {
        files: groups[0]?.files.map((file, index) => ({
          ...file,
          item: {
            ...file.item,
            name: index === 1 ? 'different.jpg' : 'same.jpg',
            size: 102400,
            capture_ts: index === 0 ? null : 1700000000 + index,
          },
        })),
        pairs: groups[0]?.pairs,
      },
    ],
  });
  const el = await searched();
  expect(
    [...el.querySelectorAll('[data-copy-name]')].every((field) =>
      field.classList.contains('font-bold'),
    ),
  ).toBe(true);
  expect(
    [...el.querySelectorAll('[data-copy-date]')].every((field) =>
      field.classList.contains('font-bold'),
    ),
  ).toBe(true);
  expect(
    [...el.querySelectorAll('[data-copy-size]')].every(
      (field) => !field.classList.contains('font-bold'),
    ),
  ).toBe(true);
  expect(el.querySelector('[data-copy-date]')?.textContent).toBe('No capture date');
  const dates = [...el.querySelectorAll('[data-copy-date]')].map((field) => field.textContent);
  expect(dates[1]).not.toBe(dates[2]);
});

it('shows rating and flag icon pills on thumbnails without duplicate metadata rows or folder buttons', async () => {
  vi.mocked(api.get).mockResolvedValueOnce({
    groups: groups.map((group) => ({
      ...group,
      files: group.files.map((file, index) => ({
        ...file,
        item: {
          ...file.item,
          rating: [0, 1, 5][index],
          flag: [0, 1, -1][index],
        },
      })),
    })),
  });
  const el = await searched();
  const group = el.querySelector('article[aria-label="Duplicate group 1"]');
  expect(group?.querySelectorAll('opm-file-card')).toHaveLength(3);
  const cards = [...el.querySelectorAll('opm-file-card')];
  expect(cards[0]?.querySelector('[data-badge="rating"]')).toBeNull();
  expect(cards[1]?.querySelector('[data-badge="rating"]')?.getAttribute('aria-label')).toBe(
    '1 star',
  );
  expect(cards[2]?.querySelector('[data-badge="rating"]')?.getAttribute('aria-label')).toBe(
    '5 stars',
  );
  expect(cards[1]?.querySelector('[data-badge="flag"] opm-icon')?.getAttribute('name')).toBe(
    'flag',
  );
  expect(cards[2]?.querySelector('[data-badge="flag"]')?.getAttribute('aria-label')).toBe(
    'Flagged rejected',
  );
  expect(el.querySelector('[data-copy-rating], [data-copy-flag]')).toBeNull();
  expect(
    [...(group?.querySelectorAll('button') ?? [])].some((button) =>
      button.textContent?.includes('Keep files from this folder'),
    ),
  ).toBe(false);
});

it('opens the context action with ancestors and applies the selected folder to all groups', async () => {
  const el = await searched();
  const card = el.querySelector('opm-file-card');
  card?.dispatchEvent(
    new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }),
  );
  await settle(el);
  el.querySelector<HTMLButtonElement>('[data-duplicate-menu] button')?.click();
  await settle(el);
  const select = el.querySelector<HTMLSelectElement>('[aria-label="Preferred folder or parent"]');
  expect([...(select?.options ?? [])].map((option) => option.textContent)).toEqual([
    'photos/trip/day',
    'photos/trip',
    'photos/',
  ]);
  if (!select) throw new Error('Missing ancestor selector');
  select.value = '1';
  select.dispatchEvent(new Event('change'));
  const folderModal = [...el.querySelectorAll('opm-modal')].find(
    (modal) => modal.heading === 'Keep files from this folder',
  );
  if (!folderModal) throw new Error('Missing folder preference dialog');
  button(folderModal, 'Apply to all groups').click();
  await settle(el);
  expect(el.querySelector('opm-file-card')?.selected).toBe(true);
  button(el, 'Automatic Selection').click();
  await settle(el);
  const dialog = el.querySelector('opm-duplicate-selection');
  expect(dialog?.rules[0]).toMatchObject({
    kind: 'folder',
    enabled: true,
    folders: [{ volume_id: 1, dir: 'trip' }],
  });
});

it('shows search errors and disables destructive choices after an index change', async () => {
  const el = await searched();
  events.emit({ type: 'fs_change', volume_id: 2, dirs: ['backup'] });
  await settle(el);
  expect(button(el, 'Automatic Selection').disabled).toBe(true);
  expect(button(el, 'Trash 2 duplicates').disabled).toBe(true);
  vi.mocked(api.post).mockRejectedValueOnce(new Error('Search too large; narrow the source'));
  button(el, 'Find duplicates').click();
  await settle(el);
  expect(el.querySelector('[role="alert"]')?.textContent).toContain('Search too large');
});

it('invalidates results only when the shared soft-exclusion list changes', async () => {
  const el = await searched();
  const put = vi.spyOn(api, 'put').mockResolvedValue(sampleSettings({ name_max_len: 143 }));
  await saveSettings(sampleSettings());
  await settle(el);
  expect(button(el, 'Trash 2 duplicates').disabled).toBe(false);
  put.mockResolvedValue(
    sampleSettings({
      soft_excluded_folders: [{ volume_id: 2, path: 'backup' }],
    }),
  );
  await saveSettings(sampleSettings());
  await settle(el);
  expect(button(el, 'Automatic Selection').disabled).toBe(true);
  expect(button(el, 'Trash 2 duplicates').disabled).toBe(true);
  put.mockResolvedValue(sampleSettings());
  await saveSettings(sampleSettings());
});
