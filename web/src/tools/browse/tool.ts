import { defineTool } from '@/tools/framework/tool-config';

/**
 * Browse, the landing page: two panes to look around the library side by side, narrowed down by
 * the filter bar (it applies to both panes). No plan, no actions: it is the framework with
 * nothing added, which also puts the shared components together in one place.
 */
export const browseTool = defineTool({
  id: 'browse',
  title: 'Browse',
  summary: 'Look around the library in two panes and narrow them down with the filters.',
  panes: {
    source: { heading: 'Source', baseMode: 'none', showMatches: false, planSide: null },
    comparison: { heading: 'Comparison', baseMode: 'none', showMatches: false, planSide: null },
  },
  filter: { applies: 'both' },
});
