import type { MoveExecuteOptions } from '@/api/generated/MoveExecuteOptions';
import type { ConfirmOptions } from '@/components/confirm-dialog';
import type { IconName } from '@/components/opm-icon';
import type { PaneLocation } from '@/stores/nav';
import { executePlanAction, type PlanActionSpec } from '@/tools/framework/plan-actions';
import type { ToolActions } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import { formatFileSize } from '@/util/format';
import { formatCount } from '@/util/format-count';

export function formatLocation(location: PaneLocation | null): string {
  return location?.volume ? `${location.volume}${location.dir ? `/${location.dir}` : ''}` : '';
}

export function fileCount(count: number): string {
  return `${formatCount(count)} ${count === 1 ? 'file' : 'files'}`;
}

export interface MoveWording {
  title(count: number): string;
  confirmLabel(count: number): string;
  intro(info: { size: string; destination: string }): string;
}

export const MOVE_WORDING: MoveWording = {
  title: (count) => `Move ${fileCount(count)}?`,
  confirmLabel: (count) => `Move ${fileCount(count)}`,
  intro: ({ size, destination }) => `${size} will be moved into ${destination}.`,
};

export function confirmMove(ctx: ToolContext, wording: MoveWording): PlanActionSpec['confirm'] {
  return ({ count, bytes, selected }): ConfirmOptions => {
    if (count === null || bytes === null) throw new Error('Missing moving file counts.');
    const lines = [
      wording.intro({
        size: formatFileSize(bytes),
        destination: formatLocation(ctx.base('comparison')),
      }),
    ];
    for (const warning of ctx.plan.warnings) {
      if (warning.code === 'cross_volume' || warning.code === 'replaces_existing') {
        lines.push(`${selected ? 'Whole plan: ' : ''}${warning.message}`);
      }
    }
    lines.push('Folders that become empty are not removed; use Empty folder cleanup afterwards.');
    return {
      title: wording.title(count),
      confirmLabel: wording.confirmLabel(count),
      message: lines.join('\n\n'),
      danger: (ctx.plan.stats.replaced ?? 0) > 0,
    };
  };
}

export function moveActions(
  _ctx: ToolContext,
  options: { label: string; icon: IconName; emptyMessage: string; wording: MoveWording },
): ToolActions {
  return {
    defaultId: 'move',
    count: (ctx) => ctx.selection('source').count.value || (ctx.plan.stats.to_move ?? 0),
    options: [
      {
        id: 'move',
        label: options.label,
        icon: options.icon,
        disabled: (ctx) => ((ctx.plan.stats.to_move ?? 0) === 0 ? 'Nothing to move' : null),
        run: (ctx) =>
          executePlanAction(ctx, {
            side: 'source',
            options: {} satisfies MoveExecuteOptions,
            confirm: confirmMove(ctx, options.wording),
            emptyMessage: options.emptyMessage,
          }),
      },
    ],
  };
}
