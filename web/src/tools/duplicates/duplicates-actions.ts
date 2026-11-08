import { isApiError } from '@/api/errors';
import type { DuplicateExecuteOptions } from '@/api/generated/DuplicateExecuteOptions';
import { createPlanSelection } from '@/api/plans';
import { createSelection } from '@/api/selections';
import { toast } from '@/components/toast';
import { PANES, type PaneId } from '@/stores/nav';
import { lastActionOf } from '@/stores/prefs';
import { activePane } from '@/stores/selection';
import { handOffSelection } from '@/tools/framework/hand-off';
import { executePlanAction } from '@/tools/framework/plan-actions';
import type { ToolActions } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import { formatFileSize } from '@/util/format';
import { formatCount } from '@/util/format-count';

export function explicitPane(ctx: ToolContext): PaneId | null {
  const panes = PANES.filter((pane) => ctx.selection(pane).count.value > 0);
  return panes.length === 2 ? activePane.value : (panes[0] ?? null);
}

async function moveFiles(ctx: ToolContext): Promise<void> {
  if (ctx.plan.status !== 'ready' || ctx.plan.planId === null) return;
  const pane = explicitPane(ctx);
  try {
    const selection = pane
      ? await createSelection(ctx.selection(pane).toArray(), 'Duplicate finder')
      : await createPlanSelection(ctx.plan.planId, 'comparison');
    if (selection.item_count === 0) {
      toast.info('There are no files to move.');
      return;
    }
    handOffSelection({
      target: 'move',
      origin: pane ?? 'comparison',
      selection,
      label: `${formatCount(selection.item_count)} files from Duplicate finder`,
    });
  } catch (error) {
    if (
      isApiError(error) &&
      (error.status === 409 || (error.status === 404 && error.code === 'plan_not_found'))
    ) {
      toast.warning('The matches changed, check again');
      ctx.plan.retry();
      return;
    }
    throw error;
  }
}

function deleteFiles(ctx: ToolContext, side: PaneId): Promise<void> {
  return executePlanAction(ctx, {
    side,
    options: {
      action: side === 'source' ? 'delete_source' : 'delete_comparison',
    } satisfies DuplicateExecuteOptions,
    confirm: ({ count, bytes, selected }) => {
      if (count === null || bytes === null) throw new Error('Missing matching file counts.');
      const files = `${formatCount(count)} ${count === 1 ? 'file' : 'files'}`;
      const other = side === 'source' ? 'comparison' : 'source';
      return {
        title: `Delete ${files}?`,
        message:
          `${formatFileSize(bytes)} will be moved to the trash and can be restored from Trash. ` +
          `A file is only deleted while a matching copy stays in the ${other} pane.` +
          (selected ? ' Only your selection is used, limited to files that have a match.' : ''),
        confirmLabel: `Delete ${files}`,
        danger: true,
      };
    },
    emptyMessage: 'There are no matching files to delete.',
  });
}

export function duplicateActions(_ctx: ToolContext): ToolActions {
  return {
    defaultId: 'delete-comparison',
    count: (ctx) => {
      const option = lastActionOf(ctx.tool);
      const pane =
        option === 'move'
          ? (explicitPane(ctx) ?? 'comparison')
          : option === 'delete-source'
            ? 'source'
            : 'comparison';
      return ctx.selection(pane).count.value || (ctx.plan.stats[`${pane}_matched`] ?? 0);
    },
    options: [
      {
        id: 'delete-source',
        label: 'Delete source items',
        icon: 'trash-2',
        danger: true,
        disabled: (ctx) =>
          (ctx.plan.stats.source_matched ?? 0) === 0 ? 'No source files have a match' : null,
        run: (ctx) => deleteFiles(ctx, 'source'),
      },
      {
        id: 'delete-comparison',
        label: 'Delete comparison items',
        icon: 'trash-2',
        danger: true,
        disabled: (ctx) =>
          (ctx.plan.stats.comparison_matched ?? 0) === 0
            ? 'No comparison files have a match'
            : null,
        run: (ctx) => deleteFiles(ctx, 'comparison'),
      },
      {
        id: 'move',
        label: 'Move files',
        icon: 'folder-input',
        disabled: (ctx) =>
          explicitPane(ctx) === null && (ctx.plan.stats.comparison_matched ?? 0) === 0
            ? 'No matches to move'
            : null,
        run: moveFiles,
      },
    ],
  };
}
