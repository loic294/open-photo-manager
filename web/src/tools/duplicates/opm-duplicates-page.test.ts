import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import type { PlanDto } from '@/api/generated/PlanDto';
import { events } from '@/api/sse';
import { routeAt } from '@/app/routes';
import { filterScopes, resetFilters, setFilter, setScope } from '@/stores/filters';
import { setLocation, syncFromLocation } from '@/stores/nav';
import { resetPlanRunsForTests } from '@/stores/plan-runs';
import { lastAction, setLastAction } from '@/stores/prefs';
import { resetSelectionForTests } from '@/stores/selection';
import { resetTasksForTests, startTasks } from '@/stores/tasks';
import { clearToasts, toasts } from '@/stores/toasts';
import { resetToolStateForTests, setRecursive } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { fileItem } from '@/test/file-item';
import { folderEntry } from '@/test/folder-entry';
import { mount } from '@/test/helpers';
import { makeTask } from '@/test/task-fixtures';
import '@/tools/duplicates/opm-duplicates-page';
import { matchMode, strictPrefix } from '@/tools/duplicates/duplicates-state';
import { duplicatesTool } from '@/tools/duplicates/tool';
import { EMPTY_FILTER } from '@/util/filter';

let plans: PlanDto[];
let post: ReturnType<typeof spyPost>;
const spyPost = () => vi.spyOn(api, 'post');
const stops: (() => void)[] = [];

beforeEach(async () => {
  window.history.replaceState(null, '', '/duplicates?src=photos/trip&cmp=backup/old');
  syncFromLocation();
  stubBrowseServer({
    total: 2,
    item: (index) => fileItem({ id: index + 1, overlay: { matched: true } }),
    folders: () => ({
      folders: [folderEntry({ name: 'child', path: 'trip/child', matches: 2 })],
      counts: { in_folder: 2, in_subfolders: 2 },
    }),
  });
  await loadVolumes();
  plans = [];
  const originalPost = api.post.bind(api);
  post = vi.spyOn(api, 'post').mockImplementation(async (path, options) => {
    if (path === '/api/plans') {
      const plan: PlanDto = {
        id: plans.length + 1,
        kind: 'duplicates',
        state: 'ready',
        stats: {
          source_matched: 2,
          comparison_matched: 2,
          source_matched_bytes: 42,
          comparison_matched_bytes: 42,
        },
        warnings: [],
        result: null,
        task_id: null,
        created_at: 0,
        expires_at: 7200,
      };
      plans.push(plan);
      return plan;
    }
    if (path.endsWith('/selection'))
      return {
        id: 99,
        item_count: 2,
        total_size: 42,
        label: null,
        created_at: 0,
        expires_at: 86400,
      };
    if (path.endsWith('/execute')) {
      const task = makeTask({
        id: 8,
        kind: 'plan_execute',
        state: 'queued',
        title: 'Delete 2 files',
      });
      events.emit({ type: 'task', task });
      return task;
    }
    if (path === '/api/facets') return { cameras: [], tags: [], ratings: [] };
    return originalPost(path, options);
  });
  const originalGet = api.get.bind(api);
  vi.spyOn(api, 'get').mockImplementation(async (path, options) => {
    if (path === '/api/tasks') return [];
    if (path.startsWith('/api/plans/'))
      return plans.find((plan) => plan.id === Number(path.split('/').at(-1)));
    return originalGet(path, options);
  });
  vi.spyOn(api, 'delete').mockResolvedValue(undefined);
});

afterEach(() => {
  for (const el of document.querySelectorAll('opm-duplicates-page, opm-confirm-dialog'))
    el.remove();
  for (const stop of stops.splice(0)) stop();
  events.stop();
  resetPlanRunsForTests();
  resetTasksForTests();
  resetSelectionForTests();
  resetToolStateForTests();
  resetFilters();
  clearToasts();
  filterScopes.value = {};
  lastAction.value = {};
  matchMode.value = 'hash';
  strictPrefix.value = false;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function settle(root: Element, delay = 0) {
  if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
  for (let i = 0; i < 10; i++) {
    await Promise.resolve();
    await Promise.all(
      [root, ...root.querySelectorAll('*')].map((element) =>
        'updateComplete' in element ? element.updateComplete : undefined,
      ),
    );
  }
}
async function page() {
  const el = await mount('opm-duplicates-page');
  await settle(el, 350);
  return el;
}
const builds = () => post.mock.calls.filter(([path]) => path === '/api/plans');

it('loads the real page via its route, config, panes, match borders and comparison-only badges', async () => {
  const route = routeAt('/duplicates');
  expect(route).toMatchObject({ tag: 'opm-duplicates-page', icon: 'copy', menu: 'tools' });
  const module = (await route?.load()) as typeof import('@/tools/duplicates/opm-duplicates-page');
  expect(customElements.get('opm-duplicates-page')).toBe(module.OpmDuplicatesPage);
  const el = await page();
  expect(el.querySelector('opm-tool-view')?.config).toBe(duplicatesTool);
  expect(el.querySelector('h2')).toBeNull();
  expect(el.querySelector('opm-modal')?.heading).toBe('Duplicate finder');
  expect(el.querySelector('[data-floating-actions] opm-split-button')).toBeNull();
  expect(el.querySelector('opm-bottom-bar opm-split-button')).not.toBeNull();
  const panes = [...el.querySelectorAll('opm-pane')];
  expect(panes.map((pane) => pane.plan)).toEqual([
    { plan_id: 1, side: 'source' },
    { plan_id: 1, side: 'comparison' },
  ]);
  expect(panes.map((pane) => pane.showMatches)).toEqual([false, true]);
  for (const pane of panes) expect(pane.querySelector('.border-success')).not.toBeNull();
  expect(panes[0]?.querySelector('opm-folder-list .badge-success')).toBeNull();
  expect(panes[1]?.querySelector('opm-folder-list .badge-success')).not.toBeNull();
  expect(el.querySelector('opm-split-button')?.textContent).toContain('Delete comparison items');
});

it('mode, folder, recursive and filter scope edits rebuild with the actual generated shape', async () => {
  const el = await page();
  expect(builds()[0]?.[1]?.body).toMatchObject({
    kind: 'duplicates',
    params: {
      mode: 'hash',
      strict_prefix: false,
      source: { scope: { volume_id: 1, dir: 'trip', recursive: true }, filter: EMPTY_FILTER },
      comparison: { scope: { volume_id: 2, dir: 'old', recursive: true }, filter: EMPTY_FILTER },
    },
  });
  el.querySelector('opm-match-mode')?.dispatchEvent(
    new CustomEvent('match-change', {
      detail: { mode: 'name_prefix', strict: true },
      bubbles: true,
    }),
  );
  await settle(el, 350);
  expect(builds().at(-1)?.[1]?.body).toMatchObject({
    params: { mode: 'name_prefix', strict_prefix: true },
  });
  setLocation('comparison', { volume: 'backup', dir: 'another' });
  setRecursive('duplicates', false);
  setFilter('duplicates', { ...EMPTY_FILTER, ratings: [5] });
  setScope('duplicates', 'source');
  await settle(el, 350);
  expect(builds().at(-1)?.[1]?.body).toMatchObject({
    params: {
      source: { scope: { recursive: false }, filter: { ...EMPTY_FILTER, ratings: [5] } },
      comparison: { scope: { dir: 'another', recursive: false }, filter: EMPTY_FILTER },
    },
  });
});

it('remembers Move; the real comparison delete confirmation queues then shows the shared result toast', async () => {
  setLastAction('duplicates', 'move');
  stops.push(startTasks());
  const el = await page();
  expect(el.querySelector('opm-split-button')?.textContent).toContain('Move files');
  setLastAction('duplicates', 'delete-comparison');
  await settle(el);
  const split = el.querySelector('opm-split-button');
  split?.dispatchEvent(
    new CustomEvent('split-action', {
      detail: { optionId: 'delete-comparison' },
      bubbles: true,
    }),
  );
  await settle(el);
  const dialog = document.querySelector('opm-confirm-dialog');
  if (!dialog) throw new Error('Missing confirmation');
  await settle(dialog);
  expect(dialog.textContent).toContain('Delete 2 files?');
  expect(dialog.textContent).toContain('42 B will be moved to the trash');
  dialog.querySelector<HTMLButtonElement>('.btn-error')?.click();
  await settle(el);
  expect(post.mock.calls.find(([path]) => path.endsWith('/execute'))?.[1]?.body).toEqual({
    options: { action: 'delete_comparison' },
    only: 99,
  });
  const plan = plans[0];
  if (!plan) throw new Error('Missing plan');
  plan.result = {
    done: 2,
    skipped: 0,
    failed: 0,
    bytes: 42,
    failures: [],
    cancelled: false,
    note: null,
  };
  events.emit({
    type: 'task',
    task: makeTask({ id: 8, kind: 'plan_execute', state: 'done', title: 'Delete 2 files' }),
  });
  await settle(el);
  expect(toasts.value.some((toast) => toast.kind === 'success')).toBe(true);
});
