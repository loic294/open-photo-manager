import { followSource } from '@/tools/framework/follow-source';
import { defineTool } from '@/tools/framework/tool-config';
import { renameActions } from '@/tools/rename/rename-actions';
import { renameControls } from '@/tools/rename/rename-controls';
import { renameParams, validationReason } from '@/tools/rename/rename-state';

export const renameTool = defineTool({
  id: 'rename',
  title: 'Batch rename',
  summary:
    'Rename files in their current folders; extensions stay unchanged. The comparison previews only proposed names as non-selectable cards; folder counts remain real.',
  help: [
    'Open the source folder and choose which filenames to match.',
    'Set the cleaning, rewrite and name-pattern rules. Extensions are preserved and counters restart in each folder.',
    'Review the proposed names in the comparison pane. It shows only files that would be renamed. Filters and Include subfolders control the scope.',
    'Select files to limit the operation, then use the rename action and confirm. Files stay in their current folders.',
  ],
  recursive: 'toggle',
  filter: { applies: 'source' },
  panes: {
    source: { heading: 'Source', baseMode: 'current', showMatches: true, planSide: 'source' },
    comparison: {
      heading: 'Comparison',
      baseMode: 'none',
      showMatches: false,
      planSide: 'comparison',
    },
  },
  plan: {
    kind: 'rename',
    params: (ctx) => {
      const reason = validationReason(ctx);
      const params = renameParams(ctx);
      return reason || !params ? { ready: false, reason } : { ready: true, params };
    },
  },
  controls: renameControls,
  actions: renameActions,
  setup: (ctx) => followSource(ctx),
});
