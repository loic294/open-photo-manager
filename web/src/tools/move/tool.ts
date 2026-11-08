import { defineTool } from '@/tools/framework/tool-config';
import { MOVE_WORDING, moveActions } from '@/tools/move/move-actions';
import { ensureSettings, moveControls } from '@/tools/move/move-controls';
import { buildMoveParams } from '@/tools/move/move-params';
import { matchMode, moveRules, moveType, strictPrefix } from '@/tools/move/move-state';

export const moveTool = defineTool({
  id: 'move',
  title: 'Asset move',
  summary: 'Move files from the source into the destination and preview where they will land.',
  recursive: 'toggle',
  filter: { applies: 'source' },
  panes: {
    source: { heading: 'Source', baseMode: 'pinned', showMatches: false, planSide: 'source' },
    comparison: {
      heading: 'Destination',
      baseMode: 'pinned',
      showMatches: false,
      planSide: 'comparison',
    },
  },
  plan: {
    kind: 'move',
    params: (ctx) =>
      buildMoveParams(ctx, {
        moveType: moveType.value,
        pattern: moveRules.pattern.value,
        patternStatus: moveRules.patternStatus.value,
        policy: moveRules.policy.value,
        matchMode: matchMode.value,
        strictPrefix: strictPrefix.value,
      }),
  },
  controls: moveControls,
  actions: (ctx) =>
    moveActions(ctx, {
      label: 'Move files',
      icon: 'folder-input',
      emptyMessage: 'None of the selected files will move.',
      wording: MOVE_WORDING,
    }),
  setup: ensureSettings,
});
