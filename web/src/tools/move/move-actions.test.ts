import { afterEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import { resetPlanRunsForTests } from '@/stores/plan-runs';
import { resetSelectionForTests } from '@/stores/selection';
import { resetToolStateForTests } from '@/stores/tool-state';
import { pressConfirm, shownConfirm } from '@/test/confirm';
import { makeTask } from '@/test/task-fixtures';
import { FakePlan, toolConfig } from '@/test/tool-config';
import { formatRules } from '@/tools/format/format-state';
import { FORMAT_WORDING } from '@/tools/format/tool';
import { createToolContext } from '@/tools/framework/tool-context';
import { confirmMove, MOVE_WORDING, moveActions } from './move-actions';

afterEach(() => {
  resetSelectionForTests();
  resetPlanRunsForTests();
  resetToolStateForTests();
  vi.restoreAllMocks();
});
function setup() {
  vi.spyOn(api, 'get').mockResolvedValue([]);
  const ctx = createToolContext(
    toolConfig({
      id: 'move',
      panes: {
        source: { heading: 'Source', baseMode: 'pinned', planSide: 'source', showMatches: false },
        comparison: {
          heading: 'Destination',
          baseMode: 'pinned',
          planSide: 'comparison',
          showMatches: false,
        },
      },
    }),
    document.createElement('div'),
  );
  const plan = new FakePlan();
  const executing = vi.fn();
  Object.defineProperty(plan, 'executing', { value: executing });
  Object.defineProperty(ctx, 'plan', { value: plan });
  plan.statusSignal.value = 'ready';
  plan.planIdSignal.value = 4;
  plan.statsSignal.value = { to_move: 12, replaced: 1 };
  plan.warningsSignal.value = [
    { code: 'cross_volume', message: 'Verified copy to another volume.', count: 12 },
  ];
  ctx.setBase('comparison', { volume: 'backup', dir: 'sorted' });
  return { ctx, plan, executing };
}
const options = {
  label: 'Move files',
  icon: 'folder-input' as const,
  emptyMessage: 'None move',
  wording: MOVE_WORDING,
};

it('uses exact selection counts, plan-wide warnings, danger for replacement, and reusable format wording', () => {
  const { ctx } = setup();
  const confirm = confirmMove(ctx, MOVE_WORDING);
  const info = confirm?.({ count: 3, bytes: 1024, selected: true });
  expect(info?.title).toBe('Move 3 files?');
  expect(info?.message).toContain('1 KB will be moved into backup/sorted.');
  expect(info?.message).toContain('Whole plan: Verified copy');
  expect(info?.danger).toBe(true);
  formatRules.pattern.value = '{camera}';
  const formatting = confirmMove(ctx, FORMAT_WORDING)?.({ count: 1, bytes: 1024, selected: false });
  expect(formatting?.title).toBe('Move and format 1 file?');
  expect(formatting?.message).toContain('pattern {camera} inside backup/sorted');
});

it('executes only the server restricted selection and clears selection after queueing', async () => {
  const { ctx, executing } = setup();
  ctx.selection('source').set([1, 2, 3]);
  const post = vi
    .spyOn(api, 'post')
    .mockResolvedValueOnce({ id: 8, item_count: 2, total_size: 1024, label: null })
    .mockResolvedValueOnce(makeTask({ id: 10, kind: 'plan_execute' }));
  const actions = moveActions(ctx, options);
  expect(actions.count?.(ctx)).toBe(3);
  const run = actions.options[0]?.run(ctx);
  await shownConfirm();
  await pressConfirm('Move 2 files');
  await run;
  expect(post).toHaveBeenNthCalledWith(
    1,
    '/api/plans/4/selection',
    expect.objectContaining({
      body: { side: 'source', restrict_ids: [1, 2, 3] },
    }),
  );
  expect(post).toHaveBeenNthCalledWith(
    2,
    '/api/plans/4/execute',
    expect.objectContaining({
      body: { options: {}, only: 8 },
    }),
  );
  expect(ctx.selection('source').count.value).toBe(0);
  expect(executing).toHaveBeenCalledWith(10);
});

it('does not execute after cancel and falls back to the plan count without selection', async () => {
  const { ctx } = setup();
  const post = vi
    .spyOn(api, 'post')
    .mockResolvedValue({ id: 8, item_count: 12, total_size: 1024, label: null });
  const actions = moveActions(ctx, options);
  expect(actions.count?.(ctx)).toBe(12);
  const run = actions.options[0]?.run(ctx);
  await pressConfirm('Cancel');
  await run;
  expect(post).toHaveBeenCalledOnce();
});

it.each([
  [404, 'plan_not_found'],
  [409, 'plan_not_ready'],
  [409, 'plan_executed'],
])('uses the shared expired-plan handling for %s %s', async (status, code) => {
  const { ctx, plan } = setup();
  ctx.selection('source').set([1]);
  vi.spyOn(api, 'post').mockRejectedValue(new ApiError({ status, code, message: 'Changed' }));
  await moveActions(ctx, options).options[0]?.run(ctx);
  expect(plan.retry).toHaveBeenCalledOnce();
  expect(ctx.selection('source').count.value).toBe(1);
});
