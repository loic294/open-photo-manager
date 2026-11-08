import { executePlanAction } from '@/tools/framework/plan-actions';
import type { ToolActions } from '@/tools/framework/tool-config';
import { fileCount } from '@/tools/move/move-actions';
import { validationReason } from '@/tools/rename/rename-state';
import { formatFileSize } from '@/util/format';

export function renameActions(): ToolActions {
  return {
    defaultId: 'rename',
    count: (ctx) => ctx.selection('source').count.value || (ctx.plan.stats.to_rename ?? 0),
    options: [
      {
        id: 'rename',
        label: 'Rename files',
        icon: 'pencil',
        disabled: (ctx) =>
          validationReason(ctx) ??
          ((ctx.plan.stats.to_rename ?? 0) === 0 ? 'Nothing to rename' : null),
        run: async (ctx) => {
          if (validationReason(ctx)) return;
          await executePlanAction(ctx, {
            side: 'source',
            options: {},
            emptyMessage: 'None of the selected files need renaming.',
            confirm: ({ count, bytes }) => {
              if (count === null || bytes === null) throw new Error('Missing rename file counts.');
              return {
                title: `Rename ${fileCount(count)}?`,
                confirmLabel: `Rename ${fileCount(count)}`,
                message: `${formatFileSize(bytes)} of files will be renamed. Files remain in their current folders, extensions stay the same, and required sidecars follow their files.`,
              };
            },
          });
        },
      },
    ],
  };
}
