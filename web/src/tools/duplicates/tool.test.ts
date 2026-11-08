import { expect, it } from 'vitest';
import { duplicateActions } from '@/tools/duplicates/duplicates-actions';
import { duplicateControls } from '@/tools/duplicates/duplicates-controls';
import { duplicateParams } from '@/tools/duplicates/duplicates-params';
import { duplicatesTool } from '@/tools/duplicates/tool';
import { checkToolConfig } from '@/tools/framework/tool-config';

it('declares the duplicate planner, shared actions, filters and directional match badges', () => {
  expect(checkToolConfig(duplicatesTool)).toBeNull();
  expect(duplicatesTool).toMatchObject({
    id: 'duplicates',
    title: 'Duplicate finder',
    recursive: 'toggle',
    filter: { applies: 'choice' },
    plan: { kind: 'duplicates', params: duplicateParams },
    controls: duplicateControls,
    actions: duplicateActions,
    panes: {
      source: { heading: 'Source', baseMode: 'current', showMatches: false, planSide: 'source' },
      comparison: {
        heading: 'Comparison',
        baseMode: 'current',
        showMatches: true,
        planSide: 'comparison',
      },
    },
  });
});
