import { describe, expect, it } from 'vitest';
import { type AppRoute, notFound, redirects, routeAt, routes } from '@/app/routes';
import { isIconName } from '@/components/opm-icon';
import { TOOLS } from '@/stores/nav';

describe('route table', () => {
  it('has exactly the pages of the app, in menu order', () => {
    expect(routes.map((route) => route.path)).toEqual([
      '/browse',
      '/duplicates',
      '/move',
      '/format',
      '/cleanup',
      '/rename',
      '/trash',
      '/settings',
    ]);
  });

  it('lists the tools in the tools menu and trash and settings in the footer', () => {
    expect(routes.filter((route) => route.menu === 'tools').map((route) => route.label)).toEqual([
      'Browse',
      'Duplicate finder',
      'Asset move',
      'Folder formatting',
      'Empty folder cleanup',
      'Batch rename',
    ]);
    expect(routes.filter((route) => route.menu === 'footer').map((route) => route.label)).toEqual([
      'Trash',
      'Settings',
    ]);
  });

  it('keeps every tool of the nav store routable: the first path segment is the tool id', () => {
    const toolPaths = routes.filter((route) => route.menu === 'tools').map((route) => route.path);

    expect(toolPaths.map((path) => path.split('/')[1])).toEqual([...TOOLS]);
  });

  it('has unique, absolute paths without a trailing slash', () => {
    const paths = routes.map((route) => route.path);

    expect(new Set(paths).size).toBe(paths.length);
    for (const path of paths) expect(path).toMatch(/^\/[a-z0-9-]+$/);
  });

  it('uses valid custom element names', () => {
    for (const route of [...routes, notFound]) {
      expect(route.tag).toMatch(/^[a-z][a-z0-9]*(-[a-z0-9]+)+$/);
    }
  });

  it('gives every route a label, a known icon and a loader', () => {
    for (const route of [...routes, notFound]) {
      expect(route.label).not.toBe('');
      expect(isIconName(route.icon)).toBe(true);
      expect(typeof route.load).toBe('function');
    }
  });

  it('forwards / to the browser and nothing else', () => {
    expect(redirects).toEqual({ '/': '/browse' });
    expect(routes.some((route) => route.path === '/')).toBe(false);
  });

  it('only redirects to routes that exist', () => {
    const paths = new Set(routes.map((route) => route.path));

    for (const target of Object.values(redirects)) expect(paths.has(target)).toBe(true);
  });
});

describe('placeholders', () => {
  const placeholders = routes.filter((route) => route.tag === 'opm-route-placeholder');

  it('load the placeholder module lazily, and it defines the tag', async () => {
    expect(customElements.get('opm-route-placeholder')).toBeUndefined();

    await notFound.load();

    expect(customElements.get('opm-route-placeholder')).toBeDefined();
  });

  it('share one loader, so the placeholder is fetched once for all of them', () => {
    expect(new Set([...placeholders, notFound].map((route) => route.load)).size).toBe(1);
  });

  it('introduce themselves with their label and icon', () => {
    for (const route of placeholders) {
      expect(route.props).toEqual({ heading: route.label, icon: route.icon });
    }
  });
});

describe('settings', () => {
  it('opens the settings page from the footer menu, and loads it on demand', async () => {
    const route = routeAt('/settings');

    expect(route).toMatchObject({
      tag: 'opm-settings-page',
      label: 'Settings',
      icon: 'settings',
      menu: 'footer',
    });
    expect(customElements.get('opm-settings-page')).toBeUndefined();

    await route?.load();

    expect(customElements.get('opm-settings-page')).toBeDefined();
  });
});

describe('not found', () => {
  it('has a message, a way back and no menu entry', () => {
    expect(notFound.props).toMatchObject({
      heading: 'Page not found',
      actionHref: '/browse',
      actionLabel: 'Go to Browse',
    });
    expect(notFound.menu).toBeUndefined();
  });

  it('leads back to a page that exists', () => {
    expect(routeAt(String(notFound.props?.actionHref))).toBeDefined();
  });
});

describe('routeAt', () => {
  it('finds a route by its exact path', () => {
    expect(routeAt('/duplicates')?.label).toBe('Duplicate finder');
  });

  it('finds nothing for unknown paths, the root or a trailing slash', () => {
    expect(routeAt('/nope')).toBeUndefined();
    expect(routeAt('/')).toBeUndefined();
    expect(routeAt('/browse/')).toBeUndefined();
  });

  it('searches the table it is given', () => {
    const stub = { ...routes[0], path: '/stub' } as AppRoute;

    expect(routeAt('/stub', [stub])).toBe(stub);
    expect(routeAt('/browse', [stub])).toBeUndefined();
  });
});
