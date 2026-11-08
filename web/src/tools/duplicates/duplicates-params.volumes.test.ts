import { expect, it } from 'vitest';
import { syncFromLocation } from '@/stores/nav';
import { duplicateParams } from '@/tools/duplicates/duplicates-params';
import { duplicatesTool } from '@/tools/duplicates/tool';
import { createToolContext } from '@/tools/framework/tool-context';

it('is not ready before volumes have loaded even with named panes', () => {
  window.history.replaceState(null, '', '/duplicates?src=photos/trip&cmp=archive/trip');
  syncFromLocation();
  const ctx = createToolContext(duplicatesTool, document.createElement('div'));
  expect(duplicateParams(ctx)).toMatchObject({ ready: false });
  ctx.dispose();
});
