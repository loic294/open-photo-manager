import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { events } from '@/api/sse';
import { setLocation } from '@/stores/nav';
import { loadVolumes, resetVolumesForTests } from '@/stores/volumes';
import { fileItem } from '@/test/file-item';
import { mount } from '@/test/helpers';
import { matchMode, strictPrefix } from './duplicates-state';
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

it('groups copies in one card and displays each rating and flag beneath its thumbnail', async () => {
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
  const ratings = [...el.querySelectorAll('[data-copy-rating]')];
  expect(ratings[0]?.textContent?.trim()).toBe('Not rated');
  for (const [index, count] of [
    [1, 1],
    [2, 5],
  ] as const) {
    const stars = ratings[index]?.querySelector('[role="img"]');
    expect(stars?.getAttribute('aria-label')).toBe(`${count} out of 5 stars`);
    expect(stars?.children).toHaveLength(5);
    expect(stars?.children[count - 1]?.getAttribute('aria-current')).toBe('true');
  }
  expect(
    [...el.querySelectorAll('[data-copy-flag]')].map((flag) => flag.textContent?.trim()),
  ).toEqual(['None', 'Selected', 'Rejected']);
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
