import { duplicateActions } from '@/tools/duplicates/duplicates-actions';
import { duplicateControls } from '@/tools/duplicates/duplicates-controls';
import { duplicateParams } from '@/tools/duplicates/duplicates-params';
import { defineTool } from '@/tools/framework/tool-config';

export const duplicatesTool = defineTool({
  id: 'duplicates',
  title: 'Duplicate finder',
  summary: 'Find files in the comparison pane that also exist in the source pane.',
  recursive: 'toggle',
  filter: { applies: 'choice' },
  panes: {
    source: { heading: 'Source', baseMode: 'current', showMatches: false, planSide: 'source' },
    comparison: {
      heading: 'Comparison',
      baseMode: 'current',
      showMatches: true,
      planSide: 'comparison',
    },
  },
  plan: { kind: 'duplicates', params: duplicateParams },
  controls: duplicateControls,
  actions: duplicateActions,
});
