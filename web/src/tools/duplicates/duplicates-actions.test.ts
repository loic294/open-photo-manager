import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { SelectionDto } from '@/api/generated/SelectionDto';
import { confirmDialog } from '@/components/confirm-dialog';
import { filterFor, resetFilters } from '@/stores/filters';
import { locations, setLocation, syncFromLocation } from '@/stores/nav';
import { resetPlanRunsForTests } from '@/stores/plan-runs';
import { lastAction, setLastAction } from '@/stores/prefs';
import { activePane, resetSelectionForTests, selection } from '@/stores/selection';
import { clearToasts, toasts } from '@/stores/toasts';
import { handoffFor, pinnedBase, resetToolStateForTests } from '@/stores/tool-state';
import { makeTask } from '@/test/task-fixtures';
import { FakePlan } from '@/test/tool-config';
import { duplicateActions, explicitPane } from '@/tools/duplicates/duplicates-actions';
import { duplicatesTool } from '@/tools/duplicates/tool';
import { createToolContext } from '@/tools/framework/tool-context';
import { EMPTY_FILTER } from '@/util/filter';
import { formatFileSize } from '@/util/format';
import { formatCount } from '@/util/format-count';

vi.mock('@/components/confirm-dialog', () => ({ confirmDialog: vi.fn(async () => true) }));
vi.mock('@/tools/framework/plan-binding', async (original) => ({
  ...(await original<typeof import('@/tools/framework/plan-binding')>()),
  createPlanBinding: () => binding,
}));

let binding: FakePlan & { executing: ReturnType<typeof vi.fn> };
const result: SelectionDto = {
  id: 99,
  item_count: 2988,
  total_size: 12_400_000_000,
  label: null,
  created_at: 0,
  expires_at: 86400,
};

beforeEach(() => {
  window.history.replaceState(null, '', '/duplicates?src=photos/trip&cmp=backup/old');
  syncFromLocation();
  binding = Object.assign(new FakePlan(), { executing: vi.fn() });
  binding.statusSignal.value = 'ready';
  binding.planIdSignal.value = 3;
  binding.statsSignal.value = { source_matched: 10, comparison_matched: 20 };
  vi.mocked(confirmDialog).mockResolvedValue(true);
  vi.spyOn(api, 'get').mockResolvedValue([]);
});
afterEach(() => {
  resetSelectionForTests();
  resetPlanRunsForTests();
  resetToolStateForTests();
  resetFilters();
  clearToasts();
  lastAction.value = {};
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

function setup() {
  const ctx = createToolContext(duplicatesTool, document.createElement('div'));
  const post = vi
    .spyOn(api, 'post')
    .mockImplementation(async (path) =>
      path.endsWith('/execute') ? makeTask({ id: 8, state: 'queued' }) : result,
    );
  const actions = duplicateActions(ctx);
  const run = (id: string) => {
    const action = actions.options.find((option) => option.id === id);
    if (!action) throw new Error(`Missing action ${id}`);
    return action.run(ctx);
  };
  return { ctx, actions, post, run };
}

it('offers actions in user order with the comparison default and exact disabling reasons', () => {
  const { ctx, actions } = setup();
  expect(actions.defaultId).toBe('delete-comparison');
  expect(
    actions.options.map(({ id, label, icon, danger }) => ({ id, label, icon, danger })),
  ).toEqual([
    { id: 'delete-source', label: 'Delete source items', icon: 'trash-2', danger: true },
    { id: 'delete-comparison', label: 'Delete comparison items', icon: 'trash-2', danger: true },
    { id: 'move', label: 'Move files', icon: 'folder-input', danger: undefined },
  ]);
  expect(actions.options.map((option) => option.disabled?.(ctx))).toEqual([null, null, null]);
  binding.statsSignal.value = { source_matched: 0, comparison_matched: 0 };
  expect(actions.options.map((option) => option.disabled?.(ctx))).toEqual([
    'No source files have a match',
    'No comparison files have a match',
    'No matches to move',
  ]);
  selection.source.set([7]);
  expect(actions.options[2]?.disabled?.(ctx)).toBeNull();
});

it.each([
  ['delete-source', 'source', 10],
  ['delete-comparison', 'comparison', 20],
  ['unknown', 'comparison', 20],
  ['move', 'comparison', 20],
] as const)('counts remembered %s on %s, using selection before matches', (id, pane, count) => {
  const { ctx, actions } = setup();
  setLastAction('duplicates', id);
  expect(actions.count?.(ctx)).toBe(count);
  selection[pane].set([7, 8]);
  expect(actions.count?.(ctx)).toBe(2);
});

it('move count follows the explicit pane, including the active pane when both are selected', () => {
  const { ctx, actions } = setup();
  setLastAction('duplicates', 'move');
  expect(explicitPane(ctx)).toBeNull();
  selection.source.set([1, 2, 3]);
  expect(explicitPane(ctx)).toBe('source');
  expect(actions.count?.(ctx)).toBe(3);
  selection.comparison.set([9]);
  activePane.value = 'comparison';
  expect(explicitPane(ctx)).toBe('comparison');
  expect(actions.count?.(ctx)).toBe(1);
});

it.each(['source', 'comparison'] as const)(
  'deletes %s through the real shared action client',
  async (side) => {
    const { post, run } = setup();
    selection[side].set([1, 2, 3]);
    selection[side === 'source' ? 'comparison' : 'source'].set([4]);
    await run(`delete-${side}`);
    expect(post.mock.calls.map(([path, options]) => [path, options?.body])).toEqual([
      ['/api/plans/3/selection', { side, restrict_ids: [1, 2, 3] }],
      ['/api/plans/3/execute', { options: { action: `delete_${side}` }, only: 99 }],
    ]);
    expect(confirmDialog).toHaveBeenCalledWith({
      title: `Delete ${formatCount(2988)} files?`,
      confirmLabel: `Delete ${formatCount(2988)} files`,
      danger: true,
      message:
        `${formatFileSize(result.total_size)} will be moved to the trash and can be restored from Trash. ` +
        `A file is only deleted while a matching copy stays in the ${side === 'source' ? 'comparison' : 'source'} pane. ` +
        'Only your selection is used, limited to files that have a match.',
    });
    expect(binding.executing).toHaveBeenCalledWith(8);
    expect(selection.source.count.value).toBe(0);
    expect(selection.comparison.count.value).toBe(0);
  },
);

it.each(['source', 'comparison'] as const)(
  'without explicit %s ids selects all matches; singular is grammatical',
  async (side) => {
    const { post, run } = setup();
    post.mockResolvedValueOnce({ ...result, item_count: 1 });
    await run(`delete-${side}`);
    expect(post.mock.calls[0]?.[1]?.body).toEqual({ side, restrict_ids: null });
    expect(confirmDialog).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Delete 1 file?',
        confirmLabel: 'Delete 1 file',
        message: expect.not.stringContaining('Only your selection'),
      }),
    );
  },
);

it('cancellation and an empty server intersection never execute or clear', async () => {
  const { post, run } = setup();
  selection.comparison.set([7]);
  vi.mocked(confirmDialog).mockResolvedValueOnce(false);
  await run('delete-comparison');
  expect(post).toHaveBeenCalledOnce();
  expect(selection.comparison.toArray()).toEqual([7]);
  post.mockResolvedValueOnce({ ...result, item_count: 0 });
  await run('delete-comparison');
  expect(post).toHaveBeenCalledTimes(2);
  expect(confirmDialog).toHaveBeenCalledOnce();
  expect(toasts.value[0]?.message).toBe('There are no matching files to delete.');
});

it.each([
  [409, 'plan_not_ready'],
  [404, 'plan_not_found'],
] as const)('delete recovers %s %s without executing', async (status, code) => {
  const { post, run } = setup();
  post.mockRejectedValueOnce(new ApiError({ status, code, message: 'Stale' }));
  await run('delete-comparison');
  expect(post).toHaveBeenCalledOnce();
  expect(binding.retry).toHaveBeenCalledOnce();
  expect(toasts.value[0]?.kind).toBe('warning');
});

it.each(['source', 'comparison'] as const)(
  'move hands off explicit %s files, including unmatched ids',
  async (pane) => {
    const { post, run } = setup();
    const origin = locations[pane].value;
    selection[pane].set([7, 8, 9]);
    await run('move');
    expect(post.mock.calls[0]?.[0]).toBe('/api/selections');
    expect(post.mock.calls[0]?.[1]?.body).toEqual({ ids: [7, 8, 9], label: 'Duplicate finder' });
    expect(filterFor('move')).toEqual({ ...EMPTY_FILTER, selection_id: 99 });
    expect(handoffFor('move')).toEqual({
      label: `${formatCount(2988)} files from Duplicate finder`,
    });
    expect(locations.source.value).toEqual(origin);
    expect(pinnedBase('move', 'source')).toEqual(origin);
    expect(window.location.pathname).toBe('/move');
    expect(window.location.search).toContain(`src=${origin.volume}/${origin.dir}`);
    expect(confirmDialog).not.toHaveBeenCalled();
  },
);

it.each(['source', 'comparison'] as const)('both panes selected: active %s wins', async (pane) => {
  const { post, run } = setup();
  selection.source.set([1, 2]);
  selection.comparison.set([3]);
  activePane.value = pane;
  await run('move');
  expect(post.mock.calls[0]?.[1]?.body).toEqual({
    ids: pane === 'source' ? [1, 2] : [3],
    label: 'Duplicate finder',
  });
});

it('move without a selection requests comparison matches and uses its location', async () => {
  const { post, run } = setup();
  const origin = locations.comparison.value;
  await run('move');
  expect(post.mock.calls[0]?.[0]).toBe('/api/plans/3/selection');
  expect(post.mock.calls[0]?.[1]?.body).toEqual({ side: 'comparison', restrict_ids: null });
  expect(locations.source.value).toEqual(origin);
  expect(pinnedBase('move', 'source')).toEqual(origin);
});

it('empty move stays on duplicates and reports it', async () => {
  const { post, run } = setup();
  post.mockResolvedValueOnce({ ...result, item_count: 0 });
  await run('move');
  expect(window.location.pathname).toBe('/duplicates');
  expect(handoffFor('move')).toBeNull();
  expect(toasts.value[0]?.message).toBe('There are no files to move.');
});

it.each([
  [409, 'conflict'],
  [404, 'plan_not_found'],
] as const)('move recovers %s %s, preserving selection and navigation', async (status, code) => {
  const { post, run } = setup();
  selection.source.set([7]);
  post.mockRejectedValueOnce(new ApiError({ status, code, message: 'Stale' }));
  await run('move');
  expect(binding.retry).toHaveBeenCalledOnce();
  expect(toasts.value[0]).toMatchObject({
    kind: 'warning',
    message: 'The matches changed, check again',
  });
  expect(window.location.pathname).toBe('/duplicates');
  expect(selection.source.toArray()).toEqual([7]);
});

it.each(['move', 'delete-comparison'])('other errors from %s reject to framework', async (id) => {
  const { post, run } = setup();
  const error = new ApiError({ status: 500, code: 'internal', message: 'Try later' });
  post.mockRejectedValueOnce(error);
  await expect(run(id)).rejects.toBe(error);
  expect(toasts.value).toEqual([]);
});
