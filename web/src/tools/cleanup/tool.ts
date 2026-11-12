import { html, nothing } from 'lit';
import type { CleanupExecuteOptions } from '@/api/generated/CleanupExecuteOptions';
import type { CleanupParams } from '@/api/generated/CleanupParams';
import type { ConfirmOptions } from '@/components/confirm-dialog';
import { recursiveFor } from '@/stores/tool-state';
import { followSource } from '@/tools/framework/follow-source';
import { executePlanAction } from '@/tools/framework/plan-actions';
import { defineTool } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import { formatCount } from '@/util/format-count';

export function cleanupConfirmation(count: number, topLevel: number): ConfirmOptions {
  const nested = Math.max(0, count - topLevel);
  return {
    title: `Delete ${formatCount(count)} empty folders?`,
    message: `${formatCount(topLevel)} top-level empty folders and ${formatCount(nested)} nested folders will be removed. The base folder is kept. Empty folders are removed, not trashed. Leftover junk files (thumbnail caches, .DS_Store) go to the trash. Folders that contain anything else are left alone.`,
    confirmLabel: `Delete ${formatCount(count)} empty folders`,
    danger: true,
  };
}

const countOf = (ctx: ToolContext) => ctx.plan.stats.empty_folders ?? 0;

export const cleanupTool = defineTool({
  id: 'cleanup',
  title: 'Empty folder cleanup',
  summary: 'Use the source folder as the base. Only empty folders below it are removed.',
  help: [
    'Open a source folder. The comparison pane previews the empty folders below it.',
    'Turn off Include subfolders to limit cleanup to the current folder. Filters do not affect whether a folder is empty.',
    'Review the preview, choose Delete empty folders and confirm. The base folder is kept; leftover junk files go to the trash.',
  ],
  recursive: 'toggle',
  panes: {
    source: { heading: 'Source', baseMode: 'current', showMatches: false, planSide: null },
    comparison: {
      heading: 'Comparison',
      baseMode: 'none',
      showMatches: true,
      planSide: 'comparison',
    },
  },
  filter: {
    applies: 'none',
    reason: 'Empty means no indexed files of any kind, regardless of filters.',
  },
  controls: (ctx) => html`
    ${
      ctx.plan.status === 'ready' && countOf(ctx) === 0
        ? html`<span role="status" class="text-sm text-base-content/70">No empty folders</span>`
        : nothing
    }
    <span class="sr-only">For very large trees, pick a folder deeper in the tree.</span>
  `,
  setup: (ctx) => followSource(ctx),
  plan: {
    kind: 'cleanup',
    params: (ctx) => {
      const scope = ctx.scope('source');
      if (!scope) return { ready: false, reason: 'Open a source folder to find empty folders.' };
      const params: CleanupParams = {
        scope: { ...scope, recursive: recursiveFor(ctx.tool) },
      };
      return { ready: true, params: { ...params } };
    },
  },
  actions: (ctx) => ({
    defaultId: 'delete',
    options: [
      {
        id: 'delete',
        label: `Delete empty folders (${formatCount(countOf(ctx))})`,
        icon: 'folder-x',
        danger: true,
        disabled: (context) => (countOf(context) === 0 ? 'No empty folders' : null),
        run: async (context) => {
          const count = countOf(context);
          if (count === 0) return;
          const options: CleanupExecuteOptions = {};
          await executePlanAction(context, {
            side: null,
            options,
            confirm: () =>
              cleanupConfirmation(count, context.plan.stats.top_level_empty_folders ?? count),
            emptyMessage: 'No empty folders',
          });
        },
      },
    ],
  }),
});
