import { duplicateActions } from '@/tools/duplicates/duplicates-actions';
import { duplicateControls } from '@/tools/duplicates/duplicates-controls';
import { duplicateParams } from '@/tools/duplicates/duplicates-params';
import { defineTool } from '@/tools/framework/tool-config';

export const duplicatesTool = defineTool({
  id: 'duplicates',
  title: 'Duplicate finder',
  summary: 'Find files in the comparison pane that also exist in the source pane.',
  help: [
    'Open a source folder on the left and a comparison folder on the right.',
    'Choose a matching mode. Hash finds identical contents; the other modes compare names and capture times.',
    'Use filters and Include subfolders to narrow the files being compared. Review the matching files before acting.',
    'Select matches to limit the action, or leave the selection empty for all matches. Choose which side to delete, then confirm. Deleted files go to the trash.',
  ],
  floatingActions: true,
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
