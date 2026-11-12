import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import type { PlanDto } from '@/api/generated/PlanDto';
import { events } from '@/api/sse';
import { routeAt } from '@/app/routes';
import { resetFilters, setFilter } from '@/stores/filters';
import { syncFromLocation } from '@/stores/nav';
import { resetPlanRunsForTests } from '@/stores/plan-runs';
import { resetSelectionForTests } from '@/stores/selection';
import { resetToolStateForTests, setPinnedBase } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { fileItem } from '@/test/file-item';
import { folderEntry } from '@/test/folder-entry';
import { mount } from '@/test/helpers';
import { sampleSettings } from '@/test/settings-fixtures';
import { moveRules, moveType } from './move-state';
import '@/tools/move/opm-move-page';
import '@/tools/format/opm-format-page';
import { formatRules } from '@/tools/format/format-state';
import { EMPTY_FILTER } from '@/util/filter';

let post: ReturnType<typeof spyPost>;
const spyPost = () => vi.spyOn(api, 'post');
beforeEach(async () => {
  window.history.replaceState(null, '', '/move?src=photos/trip&cmp=backup/old');
  syncFromLocation();
  stubBrowseServer({
    total: 2,
    item: (index) =>
      fileItem({ id: index + 1, overlay: index === 1 ? { planned: true } : { matched: true } }),
    folders: () => ({
      folders: [folderEntry({ name: 'new', path: 'trip/new', overlay: { planned: true } })],
      counts: { in_folder: 1, in_subfolders: 1 },
    }),
  });
  await loadVolumes();
  const originalPost = api.post.bind(api);
  post = vi.spyOn(api, 'post').mockImplementation(async (path, options) => {
    if (path === '/api/plans')
      return {
        id: 1,
        kind: 'move',
        state: 'ready',
        stats: { to_move: 2, bytes_to_move: 42, new_folders: 1 },
        warnings: [],
        result: null,
        task_id: null,
        created_at: 0,
        expires_at: 7200,
      } satisfies PlanDto;
    if (path === '/api/format/preview')
      return {
        error: null,
        appends_filename: true,
        has_filename_token: false,
        samples: [],
      };
    if (path === '/api/facets') return { cameras: [], tags: [], ratings: [] };
    return originalPost(path, options);
  });
  const originalGet = api.get.bind(api);
  vi.spyOn(api, 'get').mockImplementation(async (path, options) => {
    if (path === '/api/settings') return sampleSettings();
    if (path === '/api/format/tokens') return { tokens: [], examples: [], sample: {} };
    return originalGet(path, options);
  });
  vi.spyOn(api, 'delete').mockResolvedValue(undefined);
});
afterEach(() => {
  for (const el of document.querySelectorAll('opm-move-page, opm-format-page')) el.remove();
  events.stop();
  resetPlanRunsForTests();
  resetSelectionForTests();
  resetToolStateForTests();
  resetFilters();
  moveType.value = 'keep_source';
  moveRules.patternStatus.value = 'idle';
  formatRules.patternStatus.value = 'idle';
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function settle(root: Element, delay = 0) {
  if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
  for (let i = 0; i < 10; ++i) {
    await Promise.resolve();
    await Promise.all(
      [root, ...root.querySelectorAll('*')].map((element) =>
        'updateComplete' in element ? element.updateComplete : undefined,
      ),
    );
  }
}
const builds = () => post.mock.calls.filter(([path]) => path === '/api/plans');

it('pins both bases through real panes, builds, changes type and preserves the two-scroller contract', async () => {
  expect(routeAt('/move')?.tag).toBe('opm-move-page');
  const el = await mount('opm-move-page');
  await settle(el, 350);
  expect(builds()).toHaveLength(0);
  expect(el.textContent).toContain('Choose the source folder');
  for (const pane of el.querySelectorAll('opm-pane')) {
    const button = [...pane.querySelectorAll('button')].find(
      (candidate) => candidate.textContent?.trim() === 'Use this folder',
    );
    expect(button).toBeDefined();
    button?.click();
  }
  await settle(el, 350);
  expect(builds().at(-1)?.[1]?.body).toMatchObject({
    kind: 'move',
    params: {
      move_type: 'keep_source',
      source: { scope: { volume_id: 1, dir: 'trip' } },
      destination: { volume_id: 2, dir: 'old' },
    },
  });
  moveType.value = 'flat';
  await settle(el, 350);
  expect(builds().at(-1)?.[1]?.body).toMatchObject({ params: { move_type: 'flat' } });
  expect(el.querySelectorAll('[data-pane-body]')).toHaveLength(2);
  expect(el.querySelector('opm-tool-view')?.classList.contains('min-h-0')).toBe(true);
  const destination = [...el.querySelectorAll('opm-pane')].find(
    (pane) => pane.pane === 'comparison',
  );
  expect(destination?.querySelector('opm-file-card')?.textContent).toBeDefined();
});

it('passes handoff selection unchanged to source planning', async () => {
  setPinnedBase('move', 'source', { volume: 'photos', dir: 'trip' });
  setPinnedBase('move', 'comparison', { volume: 'backup', dir: 'old' });
  setFilter('move', { ...EMPTY_FILTER, selection_id: 99 });
  const el = await mount('opm-move-page');
  await settle(el, 350);
  expect(builds().at(-1)?.[1]?.body).toMatchObject({
    params: { source: { filter: { selection_id: 99 } } },
  });
});

it('opens formatting in place with the shared editor and rebuilds after a destination pin/reset', async () => {
  expect(routeAt('/format')?.tag).toBe('opm-format-page');
  const el = await mount('opm-format-page');
  await settle(el, 700);
  expect(builds().at(-1)?.[1]?.body).toMatchObject({
    kind: 'move',
    params: {
      move_type: 'custom',
      pattern: '{YYYY}/{mm}',
      destination: { volume_id: 1, dir: 'trip' },
      match_mode: null,
      strict_prefix: false,
    },
  });
  expect(el.querySelector('opm-pattern-editor')?.limit).toBe(5);
  setPinnedBase('format', 'comparison', { volume: 'backup', dir: 'other' });
  await settle(el, 350);
  expect(builds().at(-1)?.[1]?.body).toMatchObject({
    params: { destination: { volume_id: 2, dir: 'other' } },
  });
  setPinnedBase('format', 'comparison', null);
  await settle(el, 350);
  expect(builds().at(-1)?.[1]?.body).toMatchObject({
    params: { destination: { volume_id: 1, dir: 'trip' } },
  });
  expect(el.textContent).toContain('Move and format files');
});
