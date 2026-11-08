import type { Signal } from '@preact/signals-core';
import { afterEach, beforeEach, describe, expect, it, type MockInstance, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { IndexStatusDto } from '@/api/generated/IndexStatusDto';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { VolumeDto } from '@/api/generated/VolumeDto';
import { type ConnectionState, events } from '@/api/sse';
import { requestReindex } from '@/app/app-actions';
import type { AppRoute } from '@/app/routes';
import { resetIndexStatusForTests } from '@/stores/index-status';
import { currentPath, navigate } from '@/stores/nav';
import { theme } from '@/stores/prefs';
import { resetTasksForTests, tasks } from '@/stores/tasks';
import { clearToasts, toasts } from '@/stores/toasts';
import { volumeIdByName } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { pressConfirm, shownConfirm } from '@/test/confirm';
import { FakeEventSource } from '@/test/fake-event-source';
import { mount } from '@/test/helpers';
import { makeTask, taskEvent } from '@/test/task-fixtures';
import '@/app/apm-app';

type App = HTMLElementTagNameMap['apm-app'];

const TITLE_SUFFIX = ' · Advanced Photo Manager';

const until = (check: () => unknown) => vi.waitFor(check, { interval: 1, timeout: 2000 });

async function mountApp(url: string, props: Partial<App> = {}): Promise<App> {
  window.history.replaceState(null, '', url);
  return mount('apm-app', props);
}

const photos: VolumeDto = {
  id: 1,
  name: 'photos',
  writable: true,
  online: true,
  fs_type: null,
  case_insensitive: null,
  watch_mode: 'watching',
  watch_reason: null,
};
const IDLE: IndexStatusDto = {
  held_removals: [],
  active_task_id: null,
  watcher: null,
  warnings: [],
};
const RESCAN = '/api/index/rescan';

interface Answers {
  volumes: readonly VolumeDto[];
  status: IndexStatusDto;
}

/** Makes `api.get` answer the volume list and the index status (and `[]` to anything else). */
function serve(initial: Partial<Answers> = {}): Answers {
  const answers: Answers = { volumes: [], status: IDLE, ...initial };
  vi.spyOn(api, 'get').mockImplementation(async (path: string) => {
    if (path === '/api/volumes') return answers.volumes;
    if (path === '/api/index/status') return answers.status;
    return [];
  });
  return answers;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
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
  // The Browse page is the real one: its panes ask for the folders, and no server is there.
  const nothing = { folders: [], counts: { in_folder: 0, in_subfolders: 0 } };
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(nothing))),
  );
  clearToasts();
});

afterEach(() => {
  events.stop();
  resetTasksForTests();
  resetIndexStatusForTests();
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
    const soon: AppRoute = {
      path: '/soon',
      tag: 'apm-route-placeholder',
      load: () => import('@/components/apm-route-placeholder'),
      label: 'Coming soon',
      icon: 'file',
      props: { heading: 'Coming soon', icon: 'file' },
    };
    const app = await mountApp('/soon', { routes: [soon] });

    await until(() => expect(app.querySelector('main apm-route-placeholder')).not.toBeNull());
    expect(text(app.querySelector('main h2'))).toBe('Coming soon');
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
    const settings = stubRoute('/settings', { label: 'Settings', menu: 'footer' });
    const app = await mountApp('/settings', { routes: [settings] });

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
    const first = stubRoute('/first', { label: 'First page' });
    const second = stubRoute('/second', { label: 'Second page' });
    const app = await mountApp('/first', { routes: [first, second] });

    expect(heading(app)).toBe('First page');
    expect(document.title).toBe(`First page${TITLE_SUFFIX}`);

    navigate('/second');
    await app.updateComplete;

    expect(heading(app)).toBe('Second page');
    expect(document.title).toBe(`Second page${TITLE_SUFFIX}`);
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
  const queued = makeTask({ id: 7, kind: 'index', state: 'queued' });
  const messages = () => toasts.value.map((t) => [t.kind, t.message]);
  const reindexEntries = (app: App) => [
    entry(app, 'Re-index current folders') as HTMLButtonElement,
    entry(app, 'Re-index all assets') as HTMLButtonElement,
  ];

  let post: MockInstance<typeof api.post>;
  /** What the server answers to the rescan request; a test replaces it to refuse or to wait. */
  let answer: () => Promise<TaskDto>;
  /** The rescan requests only: the Browse panes post their own queries through the same client. */
  const rescans = () => post.mock.calls.filter(([path]) => path === RESCAN);

  beforeEach(() => {
    // A pane on a folder asks for its files too, which the shell's empty answer cannot serve.
    stubBrowseServer({ volumes: [photos] });
    answer = () => Promise.resolve(queued);
    const realPost = api.post;
    post = vi
      .spyOn(api, 'post')
      .mockImplementation(((path, options) =>
        path === RESCAN ? answer() : realPost(path, options)) as typeof api.post);
  });

  /** The entries are off while the event stream is down, so open it before looking at them. */
  async function mountConnected(url: string): Promise<App> {
    serve({ volumes: [photos] });
    const app = await mountApp(url);
    FakeEventSource.last().open();
    await until(() => expect(volumeIdByName('photos')).toBe(1));
    await app.updateComplete;
    return app;
  }

  it('re-indexes the folders of the panes, with their subfolders', async () => {
    const app = await mountConnected('/browse?src=photos/trips&cmp=photos/trips/2024');

    entry(app, 'Re-index current folders').click();

    await until(() => expect(rescans()).toHaveLength(1));
    expect(rescans()[0]?.[1]).toMatchObject({
      body: {
        targets: [
          { volume_id: 1, dir: 'trips', recursive: true },
          { volume_id: 1, dir: 'trips/2024', recursive: true },
        ],
        force: true,
        confirm_removals: false,
      },
    });
    await until(() => expect(messages()).toEqual([['success', 'Re-indexing 2 folders']]));
  });

  it('asks the user to open a folder first when the panes show none', async () => {
    const app = await mountConnected('/browse');

    entry(app, 'Re-index current folders').click();

    await until(() => expect(messages()).toEqual([['info', 'Open a folder first']]));
    expect(rescans()).toEqual([]);
  });

  it('asks before re-indexing all assets, and sends nothing when the user declines', async () => {
    const app = await mountConnected('/browse');

    entry(app, 'Re-index all assets').click();
    const dialog = await shownConfirm();
    expect(dialog.title).toBe('Re-index all assets?');
    await pressConfirm('Cancel');
    await until(() => expect(reindexEntries(app).some((item) => item.disabled)).toBe(false));

    expect(rescans()).toEqual([]);
    expect(messages()).toEqual([]);

    entry(app, 'Re-index all assets').click();
    await pressConfirm('Re-index all assets');

    await until(() => expect(rescans()).toHaveLength(1));
    expect(rescans()[0]?.[1]).toMatchObject({
      body: { targets: [], force: true, confirm_removals: false },
    });
    await until(() => expect(messages()).toEqual([['success', 'Re-indexing all assets']]));
  });

  it('shows the message of the server when the request is refused', async () => {
    const app = await mountConnected('/browse?src=photos/trips');
    answer = () =>
      Promise.reject(
        new ApiError({ status: 409, code: 'volume_offline', message: 'Volume photos is offline' }),
      );

    entry(app, 'Re-index current folders').click();

    await until(() => expect(messages()).toEqual([['error', 'Volume photos is offline']]));
  });

  it('turns both entries off, with a spinner on the one pressed, while the request is sent', async () => {
    const app = await mountConnected('/browse?src=photos/trips');
    const reply = deferred<TaskDto>();
    answer = () => reply.promise;

    entry(app, 'Re-index current folders').click();
    await app.updateComplete;

    for (const item of reindexEntries(app)) {
      expect(item.disabled).toBe(true);
      expect(item.closest('li')?.classList.contains('menu-disabled')).toBe(true);
    }
    expect(entry(app, 'Re-index current folders').querySelector('.loading')).not.toBeNull();
    expect(entry(app, 'Re-index current folders').querySelector('apm-icon')).toBeNull();
    expect(entry(app, 'Re-index all assets').querySelector('.loading')).toBeNull();
    expect(entry(app, 'Re-index all assets').querySelector('apm-icon')).not.toBeNull();

    reply.resolve(queued);
    await until(() => expect(reindexEntries(app).some((item) => item.disabled)).toBe(false));

    for (const item of reindexEntries(app)) {
      expect(item.closest('li')?.classList.contains('menu-disabled')).toBe(false);
      expect(item.querySelector('.loading')).toBeNull();
    }
    expect(rescans()).toHaveLength(1);
  });

  it('turns both entries off while the event stream is down, and back on when it is back', async () => {
    const app = await mountConnected('/browse?src=photos/trips');

    FakeEventSource.last().error();
    await app.updateComplete;

    expect(events.connection.value).toBe('reconnecting');
    for (const item of reindexEntries(app)) {
      expect(item.disabled).toBe(true);
      expect(item.closest('li')?.classList.contains('menu-disabled')).toBe(true);
      item.click();
    }
    expect(rescans()).toEqual([]);
    expect(document.querySelector('apm-confirm-dialog')).toBeNull();
    expect(toasts.value).toEqual([]);

    events.stop();
    events.start();
    FakeEventSource.last().open();
    await app.updateComplete;

    expect(reindexEntries(app).some((item) => item.disabled)).toBe(false);
  });

  it('hands the menu back to the "not available" notice once the shell is gone', async () => {
    const app = await mountConnected('/browse');

    app.remove();
    requestReindex('all');

    expect(messages()).toEqual([['info', 'Re-indexing is not available yet.']]);
    expect(rescans()).toEqual([]);
  });
});

describe('index notices', () => {
  const held: IndexStatusDto = {
    held_removals: [{ volume_id: 1, volume_name: 'photos', files: 12_345 }],
    active_task_id: null,
    watcher: null,
    warnings: [],
  };
  const notices = (app: App) => app.querySelector('apm-index-notices');
  const alertOf = (app: App) => notices(app)?.querySelector('[role="alert"]');

  /** The connection banner would sit between the top bar and the notices; keep it out of the way. */
  async function mountConnected(): Promise<App> {
    const app = await mountApp('/browse');
    FakeEventSource.last().open();
    await app.updateComplete;
    return app;
  }

  it('stay out of the way while the scanner holds nothing back', async () => {
    serve({ status: IDLE });
    const app = await mountConnected();
    await until(() => expect(api.get).toHaveBeenCalledWith('/api/index/status', expect.anything()));

    expect(alertOf(app)).toBeNull();
  });

  it('warn above the page about the removals the scanner holds back, until a scan clears them', async () => {
    const answers = serve({ status: held });
    const app = await mountConnected();

    await until(() => expect(alertOf(app)).not.toBeNull());
    expect(text(notices(app))).toContain('Volume photos:');
    expect(notices(app)?.previousElementSibling?.localName).toBe('header');
    expect(notices(app)?.nextElementSibling?.localName).toBe('main');

    answers.status = IDLE;
    FakeEventSource.last().message(taskEvent({ kind: 'index', state: 'done', finished_at: 2 }));

    await until(() => expect(alertOf(app)).toBeNull());
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
    expect(banner(app)?.nextElementSibling?.localName).toBe('apm-index-notices');
    expect(banner(app)?.nextElementSibling?.nextElementSibling?.localName).toBe('main');
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
  it('connects to the server events and loads the volumes, the tasks and the index status', async () => {
    await mountApp('/browse');

    expect(FakeEventSource.instances.map((source) => source.url)).toEqual(['/api/events']);
    const loaded = vi.mocked(api.get).mock.calls.map((call) => call[0]);
    expect(loaded.sort()).toEqual([
      '/api/immich/status',
      '/api/index/status',
      '/api/tasks',
      '/api/volumes',
    ]);
  });

  it('shows the tasks the server reports in the indicator of the top bar', async () => {
    const app = await mountApp('/browse');
    const indicator = () => app.querySelector('header apm-task-indicator');
    expect(indicator()?.querySelector('button')).toBeNull();

    FakeEventSource.last().open();
    FakeEventSource.last().message(taskEvent({ items_done: 3, items_total: 4 }));

    await until(() =>
      expect(indicator()?.querySelector('button')?.getAttribute('aria-label')).toBe(
        'Move 3 files running, 75 percent',
      ),
    );
  });

  it('stops following the tasks when it is removed', async () => {
    const app = await mountApp('/browse');
    events.emit(taskEvent({ id: 1 }));
    expect(tasks.value.map((task) => task.id)).toEqual([1]);

    app.remove();
    events.emit(taskEvent({ id: 2 }));

    expect(tasks.value.map((task) => task.id)).toEqual([1]);
  });

  it('stops following the index status when it is removed', async () => {
    const app = await mountApp('/browse');
    await until(() => expect(api.get).toHaveBeenCalledWith('/api/index/status', expect.anything()));

    app.remove();
    vi.mocked(api.get).mockClear();
    events.emit(taskEvent({ kind: 'index', state: 'done', finished_at: 2 }));
    await new Promise((resolve) => setTimeout(resolve, 5));

    expect(api.get).not.toHaveBeenCalled();
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
    const alpha = stubRoute('/alpha', { label: 'Alpha', menu: 'tools' });
    const beta = stubRoute('/beta', { label: 'Beta', menu: 'tools' });
    const app = await mountApp('/alpha', { routes: [alpha, beta] });

    window.history.pushState(null, '', '/beta');
    window.dispatchEvent(new PopStateEvent('popstate'));
    await app.updateComplete;

    expect(currentPath.value).toBe('/beta');
    expect(heading(app)).toBe('Beta');
    await until(() => expect(app.querySelector(beta.tag)).not.toBeNull());
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
