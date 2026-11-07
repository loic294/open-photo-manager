import type { Signal } from '@preact/signals-core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { type ConnectionState, events } from '@/api/sse';
import { onReindex } from '@/app/app-actions';
import type { AppRoute } from '@/app/routes';
import { currentPath, navigate } from '@/stores/nav';
import { theme } from '@/stores/prefs';
import { clearToasts, toasts } from '@/stores/toasts';
import { FakeEventSource } from '@/test/fake-event-source';
import { mount } from '@/test/helpers';
import '@/app/apm-app';

type App = HTMLElementTagNameMap['apm-app'];

const TITLE_SUFFIX = ' · Advanced Photo Manager';

const until = (check: () => unknown) => vi.waitFor(check, { interval: 1, timeout: 2000 });

async function mountApp(url: string, props: Partial<App> = {}): Promise<App> {
  window.history.replaceState(null, '', url);
  return mount('apm-app', props);
}

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim();
const classesOf = (element: Element | null) => [...(element?.classList ?? [])];
const heading = (app: App) => text(app.querySelector('h1'));

const menuLists = (app: App) => [...app.querySelectorAll('nav[aria-label="Main"] > ul')];
const entriesOf = (list: Element | undefined) => [
  ...(list?.querySelectorAll<HTMLElement>('li > a, li > button') ?? []),
];
const entry = (app: App, label: string) => {
  const found = entriesOf(menuLists(app)[0])
    .concat(entriesOf(menuLists(app)[1]))
    .find((item) => text(item) === label);
  if (!found) throw new Error(`No menu entry "${label}"`);
  return found;
};

const menuButton = (app: App) =>
  app.querySelector<HTMLButtonElement>('button[aria-controls="apm-menu"]');
const drawerToggle = (app: App) => app.querySelector<HTMLInputElement>('input.drawer-toggle');

function press(target: Element | null | undefined, key: string) {
  target?.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
}

async function openMenu(app: App) {
  menuButton(app)?.click();
  await app.updateComplete;
}

/** A route whose loader defines a unique, empty page element. */
function stubRoute(path: string, overrides: Partial<AppRoute> = {}) {
  const tag = `apm-shell-test${path.replaceAll('/', '-')}`;
  const load = vi.fn(async () => {
    if (!customElements.get(tag)) customElements.define(tag, class extends HTMLElement {});
  });
  const route: AppRoute = { path, tag, load, label: path.slice(1), icon: 'file', ...overrides };
  return route as AppRoute & { load: typeof load };
}

beforeEach(() => {
  FakeEventSource.reset();
  vi.stubGlobal('EventSource', FakeEventSource);
  vi.spyOn(api, 'get').mockResolvedValue([]);
  clearToasts();
});

afterEach(() => {
  events.stop();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.documentElement.removeAttribute('data-theme');
  theme.value = 'system';
  window.history.replaceState(null, '', '/');
  clearToasts();
});

describe('layout', () => {
  it('fills the viewport and never scrolls the page itself', async () => {
    const app = await mountApp('/browse');

    expect(app.classList.contains('block')).toBe(true);
    const root = app.querySelector('.drawer');
    expect(classesOf(root)).toEqual(
      expect.arrayContaining(['h-screen', 'w-screen', 'overflow-hidden', 'grid-rows-1']),
    );
    expect(classesOf(app.querySelector('.drawer-content'))).toEqual(
      expect.arrayContaining(['flex', 'flex-col', 'min-h-0', 'min-w-0']),
    );
    expect(classesOf(app.querySelector('main'))).toEqual(
      expect.arrayContaining(['min-h-0', 'flex-1', 'overflow-hidden']),
    );
  });

  it('keeps the menu permanently open from the lg breakpoint up', async () => {
    const app = await mountApp('/browse');

    expect(classesOf(app.querySelector('.drawer'))).toContain('lg:drawer-open');
    expect(classesOf(menuButton(app))).toContain('lg:hidden');
  });

  it('owns the only h1, one main landmark and one toast host outside the drawer', async () => {
    const app = await mountApp('/browse');

    expect(app.querySelectorAll('h1')).toHaveLength(1);
    expect(app.querySelectorAll('main')).toHaveLength(1);
    const hosts = app.querySelectorAll('apm-toast-host');
    expect(hosts).toHaveLength(1);
    expect(app.querySelector('.drawer')?.contains(hosts[0] ?? null)).toBe(false);
  });

  it('shows the page of the current address inside main', async () => {
    const app = await mountApp('/trash');

    await until(() => expect(app.querySelector('main apm-route-placeholder')).not.toBeNull());
    expect(text(app.querySelector('main h2'))).toBe('Trash');
  });

  it('hosts the task indicator in the right-aligned part of the top bar', async () => {
    const app = await mountApp('/browse');

    const right = app.querySelector('header > div:last-child');
    expect(classesOf(right)).toContain('flex-none');
    expect(right?.querySelector('apm-task-indicator')).not.toBeNull();
    expect(right?.querySelector('apm-theme-toggle')).not.toBeNull();
  });
});

describe('main menu', () => {
  it('lists the tools in order, then the re-index, trash and settings entries below a divider', async () => {
    const app = await mountApp('/browse');

    const [tools, footer] = menuLists(app);
    expect(entriesOf(tools).map(text)).toEqual([
      'Browse',
      'Duplicate finder',
      'Asset move',
      'Folder formatting',
      'Empty folder cleanup',
    ]);
    expect(entriesOf(footer).map(text)).toEqual([
      'Re-index current folders',
      'Re-index all assets',
      'Trash',
      'Settings',
    ]);
    expect(entriesOf(footer).map((item) => item.localName)).toEqual(['button', 'button', 'a', 'a']);
    expect(tools?.nextElementSibling?.classList.contains('divider')).toBe(true);
    expect(classesOf(tools ?? null)).toEqual(
      expect.arrayContaining(['flex-1', 'flex-nowrap', 'overflow-y-auto']),
    );
  });

  it('links every entry to its route', async () => {
    const app = await mountApp('/browse');

    const hrefs = [...app.querySelectorAll('aside a')].map((a) => a.getAttribute('href'));
    expect(hrefs).toEqual([
      '/browse',
      '/duplicates',
      '/move',
      '/format',
      '/cleanup',
      '/trash',
      '/settings',
    ]);
  });

  it('carries both pane locations in the links, so moving between tools keeps them', async () => {
    const app = await mountApp('/browse?src=photos/2024&cmp=backup');

    for (const a of app.querySelectorAll('aside a')) {
      const href = a.getAttribute('href') ?? '';
      expect(href).toContain('src=photos/2024');
      expect(href).toContain('cmp=backup');
    }

    entry(app, 'Asset move').click();
    await app.updateComplete;

    expect(window.location.pathname).toBe('/move');
    expect(window.location.search).toContain('src=photos/2024');
    expect(window.location.search).toContain('cmp=backup');
  });

  it('marks the current page and follows the navigation', async () => {
    const app = await mountApp('/duplicates');
    const current = () => [...app.querySelectorAll('aside [aria-current="page"]')].map(text);
    expect(current()).toEqual(['Duplicate finder']);
    expect(text(app.querySelector('aside .menu-active'))).toBe('Duplicate finder');

    entry(app, 'Asset move').click();
    await app.updateComplete;

    expect(current()).toEqual(['Asset move']);
    expect(app.querySelectorAll('aside .menu-active')).toHaveLength(1);
    expect(window.location.pathname).toBe('/move');
  });

  it('marks a footer entry as current too', async () => {
    const app = await mountApp('/settings');

    expect(text(app.querySelector('aside [aria-current="page"]'))).toBe('Settings');
  });

  it('marks no entry for an unknown address', async () => {
    const app = await mountApp('/nope');

    expect(app.querySelector('aside [aria-current]')).toBeNull();
    expect(app.querySelector('aside .menu-active')).toBeNull();
  });

  it('builds the menu from the routes it is given and loads a page only when it is visited', async () => {
    const alpha = stubRoute('/alpha', { label: 'Alpha', menu: 'tools' });
    const beta = stubRoute('/beta', { label: 'Beta', menu: 'tools' });
    const gamma = stubRoute('/gamma', { label: 'Gamma', menu: 'footer' });
    const hidden = stubRoute('/hidden', { label: 'Hidden' });

    const app = await mountApp('/alpha', { routes: [alpha, beta, gamma, hidden] });

    const [tools, footer] = menuLists(app);
    expect(entriesOf(tools).map(text)).toEqual(['Alpha', 'Beta']);
    expect(entriesOf(footer).map(text)).toEqual([
      'Re-index current folders',
      'Re-index all assets',
      'Gamma',
    ]);
    await until(() => expect(app.querySelector(alpha.tag)).not.toBeNull());
    expect(alpha.load).toHaveBeenCalledTimes(1);
    expect(beta.load).not.toHaveBeenCalled();
    expect(gamma.load).not.toHaveBeenCalled();

    entry(app, 'Beta').click();
    await until(() => expect(app.querySelector(beta.tag)).not.toBeNull());

    expect(beta.load).toHaveBeenCalledTimes(1);
    expect(app.querySelector(alpha.tag)).toBeNull();
  });
});

describe('page heading and title', () => {
  it('shows the label of the current route as the heading and in the document title', async () => {
    const app = await mountApp('/duplicates');

    expect(heading(app)).toBe('Duplicate finder');
    expect(document.title).toBe(`Duplicate finder${TITLE_SUFFIX}`);

    navigate('/trash');
    await app.updateComplete;

    expect(heading(app)).toBe('Trash');
    expect(document.title).toBe(`Trash${TITLE_SUFFIX}`);
  });

  it('sends the root address to Browse without showing a not-found page on the way', async () => {
    const app = await mountApp('/');

    expect(heading(app)).toBe('Browse');
    await until(() => expect(window.location.pathname).toBe('/browse'));
    expect(heading(app)).toBe('Browse');
    expect(document.title).toBe(`Browse${TITLE_SUFFIX}`);
  });

  it('explains an unknown address and offers a way back', async () => {
    const app = await mountApp('/nope');

    expect(heading(app)).toBe('Page not found');
    expect(document.title).toBe(`Page not found${TITLE_SUFFIX}`);
    await until(() => expect(app.querySelector('main apm-route-placeholder a')).not.toBeNull());
    expect(app.querySelector('main apm-route-placeholder a')?.getAttribute('href')).toBe('/browse');
  });
});

describe('re-index actions', () => {
  it('asks the app to re-index the current folders or all assets', async () => {
    const app = await mountApp('/browse');
    const handler = vi.fn();
    const stop = onReindex(handler);

    try {
      entry(app, 'Re-index current folders').click();
      entry(app, 'Re-index all assets').click();
    } finally {
      stop();
    }

    expect(handler.mock.calls).toEqual([['current'], ['all']]);
    expect(toasts.value).toEqual([]);
  });

  it('tells the user when nothing can re-index yet', async () => {
    const app = await mountApp('/browse');

    entry(app, 'Re-index all assets').click();

    expect(toasts.value.map((t) => [t.kind, t.message])).toEqual([
      ['info', 'Re-indexing is not available yet.'],
    ]);
  });
});

describe('connection banner', () => {
  const banner = (app: App) => app.querySelector('[role="alert"]');

  // The shared client remembers earlier tests (it restarts as "reconnecting" after a stop), so
  // tests that need one exact state set it directly. Its own transitions are covered by sse.test.ts.
  const setConnection = (state: ConnectionState) => {
    (events.connection as Signal<ConnectionState>).value = state;
  };

  it('stays away while connecting and while connected', async () => {
    const app = await mountApp('/browse');

    setConnection('connecting');
    await app.updateComplete;
    expect(banner(app)).toBeNull();

    setConnection('open');
    await app.updateComplete;
    expect(banner(app)).toBeNull();
  });

  it('warns that the connection was lost and clears once it is back', async () => {
    const app = await mountApp('/browse');
    FakeEventSource.last().open();
    FakeEventSource.last().error();
    await app.updateComplete;

    expect(events.connection.value).toBe('reconnecting');
    expect(classesOf(banner(app))).toEqual(
      expect.arrayContaining(['alert', 'alert-warning', 'rounded-none']),
    );
    expect(text(banner(app))).toBe('Connection to the server lost. Reconnecting…');

    events.stop();
    events.start();
    FakeEventSource.last().open();
    await app.updateComplete;

    expect(events.connection.value).toBe('open');
    expect(banner(app)).toBeNull();
  });

  it('says so when the connection is closed', async () => {
    const app = await mountApp('/browse');

    events.stop();
    await app.updateComplete;

    expect(text(banner(app))).toBe('Not connected to the server.');
  });

  it('sits between the top bar and the page, outside the scrolling area', async () => {
    const app = await mountApp('/browse');
    events.stop();
    await app.updateComplete;

    expect(banner(app)?.previousElementSibling?.localName).toBe('header');
    expect(banner(app)?.nextElementSibling?.localName).toBe('main');
    expect(classesOf(banner(app))).toContain('flex-none');
  });
});

describe('small screens', () => {
  it('opens from the menu button and tells assistive technology', async () => {
    const app = await mountApp('/browse');
    const button = menuButton(app);
    expect(button?.getAttribute('aria-controls')).toBe(app.querySelector('aside')?.id);
    expect(button?.getAttribute('aria-expanded')).toBe('false');
    expect(drawerToggle(app)?.checked).toBe(false);

    await openMenu(app);

    expect(button?.getAttribute('aria-expanded')).toBe('true');
    expect(drawerToggle(app)?.checked).toBe(true);

    await openMenu(app);

    expect(button?.getAttribute('aria-expanded')).toBe('false');
    expect(drawerToggle(app)?.checked).toBe(false);
  });

  it('closes when the overlay is clicked', async () => {
    const app = await mountApp('/browse');
    await openMenu(app);

    app.querySelector<HTMLElement>('.drawer-overlay')?.click();
    await app.updateComplete;

    expect(menuButton(app)?.getAttribute('aria-expanded')).toBe('false');
    expect(drawerToggle(app)?.checked).toBe(false);
  });

  it('closes on Escape and puts the focus back on the menu button', async () => {
    const app = await mountApp('/browse');
    await openMenu(app);

    press(entry(app, 'Browse'), 'Escape');
    await app.updateComplete;

    expect(menuButton(app)?.getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(menuButton(app));
  });

  it('leaves Escape alone when it is already taken or the menu is closed', async () => {
    const app = await mountApp('/browse');
    press(entry(app, 'Browse'), 'Escape');
    expect(document.activeElement).not.toBe(menuButton(app));

    await openMenu(app);
    const taken = (event: Event) => event.preventDefault();
    entry(app, 'Browse').addEventListener('keydown', taken);
    press(entry(app, 'Browse'), 'Escape');
    await app.updateComplete;

    expect(menuButton(app)?.getAttribute('aria-expanded')).toBe('true');
  });

  it('closes once an entry is chosen and returns the focus to the menu button', async () => {
    const app = await mountApp('/browse');
    await openMenu(app);

    entry(app, 'Trash').click();
    await app.updateComplete;

    expect(menuButton(app)?.getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(menuButton(app));
    expect(heading(app)).toBe('Trash');
  });

  it('does not take the focus on a link click while the menu is closed', async () => {
    const app = await mountApp('/browse');

    entry(app, 'Trash').click();
    await app.updateComplete;

    expect(document.activeElement).not.toBe(menuButton(app));
  });

  it('moves the focus into the opened menu, onto the current page', async () => {
    const app = await mountApp('/move');
    vi.useFakeTimers();

    await openMenu(app);
    vi.advanceTimersByTime(200);

    expect(document.activeElement).toBe(entry(app, 'Asset move'));
  });

  it('moves the focus to the first entry when no page is current', async () => {
    const app = await mountApp('/nope');
    vi.useFakeTimers();

    await openMenu(app);
    vi.advanceTimersByTime(200);

    expect(document.activeElement).toBe(entry(app, 'Browse'));
  });

  it('does not move the focus into a menu that was closed again in the meantime', async () => {
    const app = await mountApp('/move');
    vi.useFakeTimers();

    await openMenu(app);
    await openMenu(app);
    vi.advanceTimersByTime(200);

    expect(document.activeElement).toBe(menuButton(app));
  });
});

describe('app services', () => {
  it('connects to the server events and loads the volumes', async () => {
    await mountApp('/browse');

    expect(FakeEventSource.instances.map((source) => source.url)).toEqual(['/api/events']);
    expect(vi.mocked(api.get).mock.calls.map((call) => call[0])).toEqual(['/api/volumes']);
  });

  it('applies the theme preference while it is mounted', async () => {
    theme.value = 'dark';
    const app = await mountApp('/browse');
    expect(document.documentElement.dataset.theme).toBe('dark');

    theme.value = 'light';
    expect(document.documentElement.dataset.theme).toBe('light');

    theme.value = 'system';
    expect(document.documentElement.dataset.theme).toBeUndefined();

    theme.value = 'dark';
    app.remove();
    theme.value = 'light';
    expect(document.documentElement.dataset.theme).toBe('dark');
  });

  it('follows the address bar: Back and Forward change the page', async () => {
    const app = await mountApp('/browse');

    window.history.pushState(null, '', '/settings');
    window.dispatchEvent(new PopStateEvent('popstate'));
    await app.updateComplete;

    expect(currentPath.value).toBe('/settings');
    expect(heading(app)).toBe('Settings');
    await until(() => expect(text(app.querySelector('main h2'))).toBe('Settings'));
  });

  it('stops everything it started once it is removed', async () => {
    const app = await mountApp('/browse');
    const source = FakeEventSource.last();
    source.open();
    await app.updateComplete;

    app.remove();

    expect(source.closed).toBe(true);
    expect(events.connection.value).toBe('closed');

    navigate('/trash');
    expect(document.title).toBe(`Browse${TITLE_SUFFIX}`);
  });

  it('can be mounted again after it was removed', async () => {
    const first = await mountApp('/browse');
    first.remove();

    const second = await mountApp('/duplicates');

    expect(FakeEventSource.instances).toHaveLength(2);
    expect(FakeEventSource.last().closed).toBe(false);
    expect(heading(second)).toBe('Duplicate finder');
    await until(() => expect(text(second.querySelector('main h2'))).toBe('Duplicate finder'));
  });
});
