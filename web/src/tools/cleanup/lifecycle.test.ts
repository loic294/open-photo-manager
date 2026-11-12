import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import type { PlanDto } from '@/api/generated/PlanDto';
import { events } from '@/api/sse';
import { setLocation, syncFromLocation } from '@/stores/nav';
import { resetPlanRunsForTests } from '@/stores/plan-runs';
import { resetSelectionForTests, selection } from '@/stores/selection';
import { resetTasksForTests, startTasks } from '@/stores/tasks';
import { clearToasts } from '@/stores/toasts';
import { resetToolStateForTests } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { mount } from '@/test/helpers';
import { makeTask, taskEvent } from '@/test/task-fixtures';
import '@/tools/cleanup/opm-cleanup-page';

vi.mock('@/components/confirm-dialog', () => ({ confirmDialog: vi.fn(async () => true) }));
vi.mock('@/components/opm-viewer', () => ({ openViewer: vi.fn() }));

const stops: (() => void)[] = [];
afterEach(() => {
  for (const page of document.querySelectorAll('opm-cleanup-page')) page.remove();
  for (const stop of stops.splice(0)) stop();
  events.stop();
  resetPlanRunsForTests();
  resetTasksForTests();
  resetSelectionForTests();
  resetToolStateForTests();
  clearToasts();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.history.replaceState(null, '', '/');
  syncFromLocation();
});

async function settle(root: Element) {
  for (let i = 0; i < 12; i++) {
    await Promise.resolve();
    await Promise.all(
      [root, ...root.querySelectorAll('*')].map((element) =>
        'updateComplete' in element ? element.updateComplete : undefined,
      ),
    );
  }
}

it('builds the real binding, executes without selected IDs, refreshes both panes and rebuilds at zero', async () => {
  const browse = stubBrowseServer();
  await loadVolumes();
  setLocation('source', { volume: 'photos', dir: 'base' });
  let nextId = 0;
  let completed = false;
  const plans = new Map<number, PlanDto>();
  const browsePost = api.post.bind(api);
  const post = vi.spyOn(api, 'post').mockImplementation(async (path, options) => {
    if (path === '/api/plans') {
      const plan: PlanDto = {
        id: ++nextId,
        kind: 'cleanup',
        state: 'ready',
        stats: { empty_folders: completed ? 0 : 3, top_level_empty_folders: 2 },
        warnings: [],
        result: null,
        task_id: null,
        created_at: 0,
        expires_at: 7200,
      };
      plans.set(plan.id, plan);
      return plan;
    }
    if (path.endsWith('/execute'))
      return makeTask({ id: 8, kind: 'plan_execute', state: 'queued' });
    return browsePost(path, options);
  });
  const browseGet = api.get.bind(api);
  vi.spyOn(api, 'get').mockImplementation(async (path, options) => {
    if (path === '/api/tasks') return [];
    if (path.startsWith('/api/plans/')) return plans.get(Number(path.split('/').at(-1)));
    return browseGet(path, options);
  });
  vi.spyOn(api, 'delete').mockResolvedValue(undefined);
  stops.push(startTasks());
  vi.useFakeTimers();
  const page = await mount('opm-cleanup-page');
  await settle(page);
  const action = () => page.querySelector<HTMLButtonElement>('opm-bottom-bar .btn-error');
  expect(action()?.disabled).toBe(true);
  await vi.advanceTimersByTimeAsync(300);
  await settle(page);
  expect(action()?.disabled).toBe(false);
  expect([...page.querySelectorAll('opm-pane')].map((pane) => pane.plan)).toEqual([
    null,
    { plan_id: 1, side: 'comparison' },
  ]);
  selection.source.set([11]);
  action()?.click();
  await settle(page);
  expect(post.mock.calls.filter(([path]) => path.endsWith('/execute'))[0]?.[1]?.body).toEqual({
    options: {},
    only: null,
  });
  expect(action()?.disabled).toBe(true);
  const old = plans.get(1);
  if (!old) throw new Error('No cleanup plan');
  old.result = {
    done: 2,
    skipped: 1,
    failed: 0,
    bytes: 0,
    cancelled: false,
    note: null,
    failures: [],
  };
  old.state = 'executed';
  completed = true;
  const before = browse.calls.folders.length;
  events.emit(
    taskEvent({
      id: 8,
      kind: 'plan_execute',
      state: 'done',
      title: 'Deleted 2 empty folders, 1 skipped',
    }),
  );
  await settle(page);
  await vi.advanceTimersByTimeAsync(1000);
  await settle(page);
  expect(nextId).toBe(2);
  expect(browse.calls.folders.length).toBeGreaterThan(before);
  expect(action()?.disabled).toBe(true);
  expect(page.textContent).toContain('No empty folders');
  expect(page.textContent).toContain('0 failed, 1 skipped');
});
