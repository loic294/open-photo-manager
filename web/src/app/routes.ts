import type { IconName } from '@/components/apm-icon';

/** Where a route appears in the main menu. */
export type MenuGroup = 'tools' | 'footer';

export interface AppRoute {
  /** Absolute path. The address bar may add a trailing slash. */
  path: string;
  /** The custom element that renders the page. The module `load` imports must define it. */
  tag: string;
  /** Imports the module that defines `tag`. Called on the first visit only, never at startup. */
  load: () => Promise<unknown>;
  /** Menu text, page heading and document title. */
  label: string;
  icon: IconName;
  /** Menu group. Omit for a route without a menu entry. */
  menu?: MenuGroup;
  /** Assigned to the element as properties before it connects. */
  props?: Readonly<Record<string, unknown>>;
}

const loadPlaceholder = () => import('@/components/apm-route-placeholder');

/**
 * A route whose feature is not built yet. When it is, replace its line below with a full entry:
 *   { path: '/duplicates', tag: 'apm-duplicates-page', label: 'Duplicate finder', icon: 'copy',
 *     load: () => import('@/tools/duplicates/apm-duplicates-page'), menu: 'tools' },
 */
function comingSoon(path: string, label: string, icon: IconName, menu?: MenuGroup): AppRoute {
  const route: AppRoute = {
    path,
    tag: 'apm-route-placeholder',
    load: loadPlaceholder,
    label,
    icon,
    props: { heading: label, icon },
  };
  if (menu) route.menu = menu;
  return route;
}

/** The pages of the app, in menu order. One line each. */
export const routes: readonly AppRoute[] = [
  comingSoon('/browse', 'Browse', 'images', 'tools'),
  comingSoon('/duplicates', 'Duplicate finder', 'copy', 'tools'),
  comingSoon('/move', 'Asset move', 'folder-input', 'tools'),
  comingSoon('/format', 'Folder formatting', 'folder-tree', 'tools'),
  comingSoon('/cleanup', 'Empty folder cleanup', 'folder-x', 'tools'),
  comingSoon('/trash', 'Trash', 'trash-2', 'footer'),
  comingSoon('/settings', 'Settings', 'settings', 'footer'),
];

/** Shown for any address that no route matches. */
export const notFound: AppRoute = {
  path: '/*',
  tag: 'apm-route-placeholder',
  load: loadPlaceholder,
  label: 'Page not found',
  icon: 'circle-question-mark',
  props: {
    heading: 'Page not found',
    message: 'There is nothing at this address.',
    icon: 'circle-question-mark',
    actionHref: '/browse',
    actionLabel: 'Go to Browse',
  },
};

/** Addresses that forward to another route (the address bar is rewritten, not pushed). */
export const redirects: Readonly<Record<string, string>> = { '/': '/browse' };

/** The route for a normalised path (see `normalisePath`), or `undefined` when there is none. */
export function routeAt(path: string, table: readonly AppRoute[] = routes): AppRoute | undefined {
  return table.find((route) => route.path === path);
}
