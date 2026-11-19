import { createSelection } from '@/api/selections';
import { trashFiles } from '@/api/trash';
import { confirmDialog } from '@/components/confirm-dialog';
import { toast } from '@/components/toast';
import { PANES } from '@/stores/nav';
import type { ToolActions } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import { formatFileSize } from '@/util/format';
import { formatCount } from '@/util/format-count';

function selectedIds(ctx: ToolContext): number[] {
  return [...new Set(PANES.flatMap((pane) => ctx.selection(pane).toArray()))];
}

async function deleteSelected(ctx: ToolContext): Promise<void> {
  const ids = selectedIds(ctx);
  if (ids.length === 0) return;
  const stored = await createSelection(ids, 'Browse');
  if (stored.item_count === 0) {
    toast.info('The selected files are no longer available.');
    ctx.clearSelections();
    return;
  }
  const files = `${formatCount(stored.item_count)} ${stored.item_count === 1 ? 'file' : 'files'}`;
  const confirmed = await confirmDialog({
    title: `Delete ${files}?`,
    message: `${formatFileSize(stored.total_size)} will move to the trash and can be restored from Trash. Sidecars that are not used by another file move with the selected files.`,
    confirmLabel: `Delete ${files}`,
    danger: true,
  });
  if (!confirmed) return;
  await trashFiles({ selection_id: stored.id });
  ctx.clearSelections();
}

export function browseActions(_ctx: ToolContext): ToolActions {
  return {
    defaultId: 'delete-selected',
    count: (context) => selectedIds(context).length,
    options: [
      {
        id: 'delete-selected',
        label: 'Delete selected',
        icon: 'trash-2',
        danger: true,
        hint: 'Move the selected files to Trash',
        disabled: (context) =>
          selectedIds(context).length === 0 ? 'Select files to delete' : null,
        run: deleteSelected,
      },
    ],
  };
}
