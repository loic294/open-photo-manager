import { followSource } from '@/tools/framework/follow-source';
import { defineTool } from '@/tools/framework/tool-config';
import { renameActions } from '@/tools/rename/rename-actions';
import { renameControls } from '@/tools/rename/rename-controls';
import { renameParams, validationReason } from '@/tools/rename/rename-state';

export const renameTool = defineTool({
  id: 'rename',
  title: 'Batch rename',
  summary:
    'Rename files in their current folders; extensions stay unchanged. The comparison shows current files plus non-selectable new-name ghosts; folder counts remain real.',
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
