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
  help: [
    'Open a volume and navigate folders independently in each pane. Use the path breadcrumbs or Up to return to a parent.',
    'Open Filters beside the page title to narrow the listings. Each pane has its own sort; either thumbnail-size slider scales both panes.',
    'Turn on Sync navigation to show the same folder and link scrolling in both panes.',
    'Double-click a file to open the viewer. Select files to hand them off to another tool.',
  ],
  panes: {
    source: { heading: 'Source', baseMode: 'none', showMatches: false, planSide: null },
    comparison: { heading: 'Comparison', baseMode: 'none', showMatches: false, planSide: null },
  },
  filter: { applies: 'both' },
});
