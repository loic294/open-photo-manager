import { signal } from '@preact/signals-core';
import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import type { PlanDto } from '@/api/generated/PlanDto';
import { events } from '@/api/sse';
import '@/components/opm-task-indicator';
import { resetFilters } from '@/stores/filters';
import { setLocation, syncFromLocation } from '@/stores/nav';
import { resetPlanRunsForTests } from '@/stores/plan-runs';
import { resetSelectionForTests, selection } from '@/stores/selection';
import { resetTasksForTests, startTasks } from '@/stores/tasks';
import { clearToasts, toasts } from '@/stores/toasts';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { mount } from '@/test/helpers';
import { makeTask, taskEvent } from '@/test/task-fixtures';
import { plainPane, toolConfig } from '@/test/tool-config';
import '@/tools/framework/opm-tool-view';
import { executePlanAction } from '@/tools/framework/plan-actions';
import type { PlanParamsResult } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';

const stops: (() => void)[] = [];
afterEach(() => {
  for (const view of document.querySelectorAll('opm-tool-view')) view.remove();
  for (const stop of stops.splice(0)) stop();
  events.stop();
  resetPlanRunsForTests();
  resetTasksForTests();
  resetSelectionForTests();
  resetFilters();
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

it('wires the real view from parameters through panes, queued execution, nonblocking progress, result and rebuild', async () => {
  stubBrowseServer();
  await loadVolumes();
  setLocation('source', { volume: 'photos', dir: 'trip' });
  setLocation('comparison', { volume: 'backup', dir: 'trip' });
  const input = signal<PlanParamsResult>({ ready: false, reason: 'Choose both bases' });
  let context: ToolContext | undefined;
  let nextId = 0;
  const plans = new Map<number, PlanDto>();
  const browsePost = api.post.bind(api);
  const browseGet = api.get.bind(api);
  const post = vi.spyOn(api, 'post').mockImplementation(async (path, options) => {
    if (path === '/api/plans') {
      const plan: PlanDto = {
        id: ++nextId,
        kind: 'duplicates',
        state: 'ready',
        stats: { matches: 2 },
        warnings: [],
        result: null,
        task_id: null,
        created_at: 0,
        expires_at: 7200,
      };
      plans.set(plan.id, plan);
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
      const task = makeTask({ id: 8, kind: 'plan_execute', state: 'queued', items_total: 2 });
      events.emit({ type: 'task', task });
      return task;
    }
    if (path === '/api/facets') return { cameras: [], tags: [], ratings: [] };
    return browsePost(path, options);
  });
  vi.spyOn(api, 'get').mockImplementation(async (path, options) => {
    if (path === '/api/tasks') return [];
    if (path.startsWith('/api/plans/')) return plans.get(Number(path.split('/').at(-1)));
    return browseGet(path, options);
  });
  vi.spyOn(api, 'delete').mockResolvedValue(undefined);
  stops.push(startTasks());
  const config = toolConfig({
    id: 'duplicates',
    plan: { kind: 'duplicates', params: () => input.value },
    panes: {
      source: { ...plainPane('Source'), planSide: 'source' },
      comparison: { ...plainPane('Comparison'), planSide: 'comparison' },
    },
    setup: (ctx) => {
      context = ctx;
      return undefined;
    },
    actions: () => ({
      defaultId: 'delete',
      options: [
        {
          id: 'delete',
          label: 'Delete comparison items',
          disabled: () => null,
          run: (ctx) =>
            executePlanAction(ctx, {
              side: 'comparison',
              options: { action: 'delete_comparison' },
              confirm: null,
              emptyMessage: 'No matches',
            }),
        },
      ],
    }),
  });
  const view = await mount('opm-tool-view', { config });
  const indicator = await mount('opm-task-indicator');
  await settle(view);
  vi.useFakeTimers();
  const action = () => view.querySelector<HTMLButtonElement>('opm-bottom-bar .btn-primary');
  expect(action()?.disabled).toBe(true);
  expect(action()?.closest('[data-tip]')?.getAttribute('data-tip')).toBe('Choose both bases');
  input.value = { ready: true, params: { source: { volume_id: 1, dir: 'trip', recursive: true } } };
  await settle(view);
  expect(action()?.disabled).toBe(true);
  expect(view.textContent).toContain('Matching...');
  await vi.advanceTimersByTimeAsync(300);
  await settle(view);
  expect(action()?.disabled).toBe(false);
  const refs = [...view.querySelectorAll('opm-pane')].map((pane) => pane.plan);
  expect(refs).toEqual([
    { plan_id: 1, side: 'source' },
    { plan_id: 1, side: 'comparison' },
  ]);
  expect(context?.plan.status).toBe('ready');
  selection.comparison.set([10, 11]);
  action()?.click();
  await settle(view);
  expect(context?.plan.status).toBe('executing');
  expect(action()?.disabled).toBe(true);
  expect(view.textContent).toContain('Working...');
  expect(selection.comparison.toArray()).toEqual([]);
  expect([...view.querySelectorAll('opm-pane')].map((pane) => pane.plan)).toEqual([null, null]);
  events.emit(taskEvent({ id: 8, kind: 'plan_execute', items_total: 2, items_done: 1 }));
  await settle(indicator);
  expect(indicator.querySelector('progress')).not.toBeNull();
  // Browsing continues while the filesystem task is working.
  setLocation('source', { volume: 'photos', dir: 'another' });
  await settle(view);
  expect(view.querySelector('opm-pane')?.browseRequest?.dir).toBe('another');
  const old = plans.get(1);
  if (!old) throw new Error('Missing built plan');
  old.result = {
    done: 1,
    skipped: 1,
    failed: 0,
    bytes: 21,
    cancelled: false,
    note: null,
    failures: [
      { file_id: 10, path: 'photos/trip/a.jpg', code: 'no_survivor', message: 'No safe copy' },
    ],
  };
  old.state = 'executed';
  events.emit(
    taskEvent({ id: 8, kind: 'plan_execute', state: 'done', title: 'Deleted 1 file, skipped 1' }),
  );
  await settle(view);
  expect(toasts.value.find((toast) => toast.kind === 'warning')?.action?.label).toBe('Details');
  const last = [...view.querySelectorAll('button')].find((button) =>
    button.textContent?.includes('Last run:'),
  );
  expect(last?.textContent).toContain('0 failed, 1 skipped');
  await vi.advanceTimersByTimeAsync(1000);
  await settle(view);
  expect(context?.plan.status).toBe('ready');
  expect(context?.plan.planId).toBe(2);
  expect(post.mock.calls.filter(([path]) => path === '/api/plans')).toHaveLength(2);
  view.querySelector<HTMLButtonElement>('[aria-label="Dismiss last run"]')?.click();
  await settle(view);
  expect(view.textContent).not.toContain('Last run:');
});
