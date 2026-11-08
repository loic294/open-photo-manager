import { isApiError } from '@/api/errors';
import type { PlanSide } from '@/api/generated/PlanSide';
import type { SelectionDto } from '@/api/generated/SelectionDto';
import { createPlanSelection, executePlan } from '@/api/plans';
import { type ConfirmOptions, confirmDialog } from '@/components/confirm-dialog';
import { toast } from '@/components/toast';
import { PANES } from '@/stores/nav';
import { trackPlanRun } from '@/stores/plan-runs';
import type { ToolContext } from '@/tools/framework/tool-context';

export interface PlanActionSpec {
  side: PlanSide | null;
  options: Record<string, unknown>;
  confirm:
    | ((summary: {
        count: number | null;
        bytes: number | null;
        selected: boolean;
      }) => ConfirmOptions)
    | null;
  emptyMessage: string;
}

/** Queues work, never waits for it. Every plan tool uses this same effective-selection flow. */
export async function executePlanAction(ctx: ToolContext, spec: PlanActionSpec): Promise<void> {
  const binding = ctx.plan;
  const planId = binding.planId;
  if (binding.status !== 'ready' || planId === null) return;
  if (!binding.executing) throw new Error('The plan binding cannot track execution.');
  try {
    let selection: SelectionDto | null = null;
    let selected = false;
    if (spec.side !== null) {
      const pane = PANES.find((id) => ctx.config.panes[id].planSide === spec.side);
      if (!pane) throw new Error(`The tool has no pane for plan side "${spec.side}".`);
      const ids = ctx.selection(pane).toArray();
      selected = ids.length > 0;
      selection = await createPlanSelection(planId, spec.side, {
        restrictIds: selected ? ids : null,
      });
      if (selection.item_count === 0) {
        toast.info(spec.emptyMessage);
        return;
      }
    }
    if (
      spec.confirm &&
      !(await confirmDialog(
        spec.confirm({
          count: selection?.item_count ?? null,
          bytes: selection?.total_size ?? null,
          selected,
        }),
      ))
    )
      return;
    // Navigation/filters may have changed while numbers or confirmation were on screen.
    if (binding.status !== 'ready' || binding.planId !== planId) {
      toast.warning('The matches changed. Check the updated plan and try again.');
      binding.retry();
      return;
    }
    const task = await executePlan(planId, { options: spec.options, only: selection?.id ?? null });
    binding.executing(task.id);
    trackPlanRun({ tool: ctx.tool, planId, taskId: task.id, title: task.title });
    ctx.clearSelections();
  } catch (error) {
    if (
      isApiError(error) &&
      ((error.status === 409 && ['plan_not_ready', 'plan_executed'].includes(error.code)) ||
        (error.status === 404 && error.code === 'plan_not_found'))
    ) {
      toast.warning('The matches changed. Check the updated plan and try again.');
      binding.retry();
      return;
    }
    throw error;
  }
}
