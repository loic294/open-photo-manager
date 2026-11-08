import type { IconName } from '@/components/apm-icon';
import { immichAttention } from '@/stores/immich';

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
  attention?: () => boolean;
}

const loadPlaceholder = () => import('@/components/apm-route-placeholder');

/** The pages of the app, in menu order. One line each. */
export const routes: readonly AppRoute[] = [
  {
    path: '/browse',
    tag: 'apm-browse-page',
    load: () => import('@/tools/browse/apm-browse-page'),
    label: 'Browse',
    icon: 'images',
    menu: 'tools',
  },
  {
    path: '/duplicates',
    tag: 'apm-duplicates-page',
    label: 'Duplicate finder',
    icon: 'copy',
    load: () => import('@/tools/duplicates/apm-duplicates-page'),
    menu: 'tools',
  },
  {
    path: '/move',
    tag: 'apm-move-page',
    label: 'Asset move',
    icon: 'folder-input',
    load: () => import('@/tools/move/apm-move-page'),
    menu: 'tools',
  },
  {
    path: '/format',
    tag: 'apm-format-page',
    label: 'Folder formatting',
    icon: 'folder-tree',
    load: () => import('@/tools/format/apm-format-page'),
    menu: 'tools',
  },
  {
    path: '/cleanup',
    tag: 'apm-cleanup-page',
    label: 'Empty folder cleanup',
    icon: 'folder-x',
    menu: 'tools',
    load: () => import('@/tools/cleanup/apm-cleanup-page'),
  },
  {
    path: '/rename',
    tag: 'apm-rename-page',
    label: 'Batch rename',
    icon: 'pencil',
    menu: 'tools',
    load: () => import('@/tools/rename/apm-rename-page'),
  },
  {
    path: '/trash',
    tag: 'apm-trash-page',
    load: () => import('@/tools/trash/apm-trash-page'),
    label: 'Trash',
    icon: 'trash-2',
    menu: 'footer',
  },
  {
    path: '/settings',
    tag: 'apm-settings-page',
    load: () => import('@/tools/settings/apm-settings-page'),
    label: 'Settings',
    icon: 'settings',
    menu: 'footer',
    attention: () => immichAttention.value,
  },
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
