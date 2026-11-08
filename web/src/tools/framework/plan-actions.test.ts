import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { SelectionDto } from '@/api/generated/SelectionDto';
import { confirmDialog } from '@/components/confirm-dialog';
import { trackPlanRun } from '@/stores/plan-runs';
import { resetSelectionForTests, selection } from '@/stores/selection';
import { clearToasts, toasts } from '@/stores/toasts';
import { makeTask } from '@/test/task-fixtures';
import { FakePlan, plainPane, toolConfig } from '@/test/tool-config';
import { executePlanAction, type PlanActionSpec } from '@/tools/framework/plan-actions';
import { createToolContext } from '@/tools/framework/tool-context';

vi.mock('@/components/confirm-dialog', () => ({ confirmDialog: vi.fn(async () => true) }));
vi.mock('@/stores/plan-runs', () => ({ trackPlanRun: vi.fn() }));
vi.mock('@/tools/framework/plan-binding', async (original) => {
  const actual = await original<typeof import('@/tools/framework/plan-binding')>();
  return { ...actual, createPlanBinding: () => binding };
});

let binding: FakePlan & { executing: ReturnType<typeof vi.fn<(taskId: number) => void>> };
const selected: SelectionDto = {
  id: 8,
  item_count: 2,
  total_size: 42,
  label: null,
  created_at: 0,
  expires_at: 86400,
};
const spec = (patch: Partial<PlanActionSpec> = {}): PlanActionSpec => ({
  side: 'comparison',
  options: { action: 'delete_comparison' },
  confirm: () => ({ title: 'Delete files?', message: 'Move them to trash', danger: true }),
  emptyMessage: 'No matched files',
  ...patch,
});

function setup() {
  const ctx = createToolContext(
    toolConfig({
      id: 'duplicates',
      plan: { kind: 'duplicates', params: () => ({ ready: true, params: {} }) },
      panes: {
        source: { ...plainPane('Source'), planSide: 'source' },
        comparison: { ...plainPane('Comparison'), planSide: 'comparison' },
      },
    }),
    document.createElement('div'),
  );
  const post = vi
    .spyOn(api, 'post')
    .mockImplementation(async (path) =>
      path.endsWith('/selection')
        ? selected
        : makeTask({ id: 9, title: 'Delete 2 files', state: 'queued' }),
    );
  const clear = vi.spyOn(ctx, 'clearSelections');
  return { ctx, post, clear };
}

beforeEach(() => {
  binding = Object.assign(new FakePlan(), { executing: vi.fn<(taskId: number) => void>() });
  binding.statusSignal.value = 'ready';
  binding.planIdSignal.value = 3;
  vi.mocked(confirmDialog).mockResolvedValue(true);
});
afterEach(() => {
  resetSelectionForTests();
  clearToasts();
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

describe('effective selection and execution', () => {
  it.each(['idle', 'building', 'executing', 'error'] as const)(
    'refuses a %s plan before any work',
    async (state) => {
      const { ctx, post, clear } = setup();
      binding.statusSignal.value = state;
      await executePlanAction(ctx, spec());
      expect(post).not.toHaveBeenCalled();
      expect(clear).not.toHaveBeenCalled();
      expect(confirmDialog).not.toHaveBeenCalled();
    },
  );

  it('restricts an explicit selection to server matches and confirms exact numbers before queueing', async () => {
    const { ctx, post, clear } = setup();
    selection.comparison.set([10, 11, 12]);
    const confirm = vi.fn(() => ({ title: 'Delete?', message: 'Trash' }));
    await executePlanAction(ctx, spec({ confirm }));
    expect(post.mock.calls.map(([path, options]) => [path, options?.body])).toEqual([
      ['/api/plans/3/selection', { side: 'comparison', restrict_ids: [10, 11, 12] }],
      ['/api/plans/3/execute', { options: { action: 'delete_comparison' }, only: 8 }],
    ]);
    expect(confirm).toHaveBeenCalledWith({ count: 2, bytes: 42, selected: true });
    expect(binding.executing).toHaveBeenCalledWith(9);
    expect(trackPlanRun).toHaveBeenCalledWith({
      tool: 'duplicates',
      planId: 3,
      taskId: 9,
      title: 'Delete 2 files',
    });
    expect(clear).toHaveBeenCalledOnce();
    expect(selection.comparison.toArray()).toEqual([]);
  });

  it('selects all matches when none are explicitly selected', async () => {
    const { ctx, post } = setup();
    selection.source.set([10]);
    const confirm = vi.fn(() => ({ title: 'Delete?', message: 'Trash' }));
    await executePlanAction(ctx, spec({ confirm }));
    expect(post.mock.calls[0]?.[1]?.body).toEqual({ side: 'comparison', restrict_ids: null });
    expect(confirm).toHaveBeenCalledWith({ count: 2, bytes: 42, selected: false });
  });

  it('uses the pane mapping, not the name of the side', async () => {
    const { ctx, post } = setup();
    ctx.config.panes.source.planSide = 'comparison';
    ctx.config.panes.comparison.planSide = 'source';
    selection.source.set([99]);
    await executePlanAction(ctx, spec({ confirm: null }));
    expect(post.mock.calls[0]?.[1]?.body).toEqual({ side: 'comparison', restrict_ids: [99] });
  });

  it('an empty effective selection informs without confirming or executing', async () => {
    const { ctx, post, clear } = setup();
    post.mockResolvedValue({ ...selected, item_count: 0 });
    await executePlanAction(ctx, spec());
    expect(post).toHaveBeenCalledOnce();
    expect(confirmDialog).not.toHaveBeenCalled();
    expect(toasts.value[0]).toMatchObject({ kind: 'info', message: 'No matched files' });
    expect(clear).not.toHaveBeenCalled();
  });

  it('waits for counts before asking, and cancelled confirmation never executes or clears', async () => {
    const { ctx, post, clear } = setup();
    let resolve: (value: SelectionDto) => void = () => {};
    post.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    vi.mocked(confirmDialog).mockResolvedValue(false);
    const pending = executePlanAction(ctx, spec());
    expect(confirmDialog).not.toHaveBeenCalled();
    resolve(selected);
    await pending;
    expect(confirmDialog).toHaveBeenCalledOnce();
    expect(post).toHaveBeenCalledOnce();
    expect(clear).not.toHaveBeenCalled();
    expect(binding.executing).not.toHaveBeenCalled();
  });

  it('cleanup passes only:null with null counts and no selection request', async () => {
    const { ctx, post } = setup();
    const confirm = vi.fn(() => ({ title: 'Delete folders?', message: 'Trash junk' }));
    await executePlanAction(ctx, spec({ side: null, confirm }));
    expect(post).toHaveBeenCalledOnce();
    expect(post.mock.calls[0]?.[0]).toBe('/api/plans/3/execute');
    expect(post.mock.calls[0]?.[1]?.body).toMatchObject({ only: null });
    expect(confirm).toHaveBeenCalledWith({ count: null, bytes: null, selected: false });
  });

  it('resolves when queued, not when the background task finishes', async () => {
    const { ctx } = setup();
    await expect(executePlanAction(ctx, spec({ confirm: null }))).resolves.toBeUndefined();
    expect(binding.executing).toHaveBeenCalledWith(9);
  });

  it('does not execute a changed plan after the confirmation', async () => {
    const { ctx, post, clear } = setup();
    vi.mocked(confirmDialog).mockImplementationOnce(async () => {
      binding.statusSignal.value = 'building';
      return true;
    });
    await executePlanAction(ctx, spec());
    expect(post).toHaveBeenCalledOnce();
    expect(binding.retry).toHaveBeenCalledOnce();
    expect(clear).not.toHaveBeenCalled();
  });
});

describe('errors', () => {
  it.each([
    [409, 'plan_not_ready'],
    [409, 'plan_executed'],
    [404, 'plan_not_found'],
  ])('recovers %s %s with a warning and rebuild', async (status, code) => {
    const { ctx, post, clear } = setup();
    post.mockRejectedValueOnce(
      new ApiError({ status: Number(status), code: String(code), message: 'Changed' }),
    );
    await expect(executePlanAction(ctx, spec())).resolves.toBeUndefined();
    expect(toasts.value[0]?.kind).toBe('warning');
    expect(binding.retry).toHaveBeenCalledOnce();
    expect(clear).not.toHaveBeenCalled();
  });

  it('also recovers errors from execution, after the selection/confirmation', async () => {
    const { ctx, post } = setup();
    post.mockResolvedValueOnce(selected).mockRejectedValueOnce(
      new ApiError({
        status: 409,
        code: 'plan_executed',
        message: 'Already executed',
      }),
    );
    await expect(executePlanAction(ctx, spec())).resolves.toBeUndefined();
    expect(binding.retry).toHaveBeenCalledOnce();
  });

  it('other errors reject for the framework toast and leave selections alone', async () => {
    const { ctx, post, clear } = setup();
    const error = new ApiError({ status: 500, code: 'internal', message: 'Try later' });
    post.mockRejectedValueOnce(error);
    selection.comparison.set([10]);
    await expect(executePlanAction(ctx, spec())).rejects.toBe(error);
    expect(toasts.value).toEqual([]);
    expect(clear).not.toHaveBeenCalled();
    expect(selection.comparison.toArray()).toEqual([10]);
  });
});
