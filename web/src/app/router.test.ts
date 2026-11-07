import { html } from 'lit';
import { afterEach, beforeEach, describe, expect, it, type Mock, vi } from 'vitest';
import { type AppRouter, type AppRouterOptions, createAppRouter } from '@/app/router';
import type { AppRoute } from '@/app/routes';
import { ApmElement } from '@/base/apm-element';
import { currentPath, locations, navigate, syncFromLocation } from '@/stores/nav';
import { mount } from '@/test/helpers';

class RouterHost extends ApmElement {
  static override properties = { options: { attribute: false } };

  declare options: AppRouterOptions;
  router: AppRouter | undefined;

  constructor() {
    super();
    this.options = {};
  }

  override connectedCallback() {
    super.connectedCallback();
    this.router = createAppRouter(this, this.options);
  }

  override disconnectedCallback() {
    super.disconnectedCallback();
    this.router?.dispose();
    this.router = undefined;
  }

  protected override render() {
    return html`${this.router?.outlet()}`;
  }
}

customElements.define('apm-router-host', RouterHost);

declare global {
  interface HTMLElementTagNameMap {
    'apm-router-host': RouterHost;
  }
}

class StubPage extends HTMLElement {
  note = '';

  connectedCallback() {
    connections.push({ tag: this.localName, note: this.note });
  }
}

const connections: { tag: string; note: string }[] = [];
let serial = 0;

function definePage(tag: string) {
  if (!customElements.get(tag)) customElements.define(tag, class extends StubPage {});
}

type StubRoute = AppRoute & { load: Mock<() => Promise<void>> };

/** A route whose loader defines its (unique) tag, like a real lazy module does. */
function stubRoute(path: string, overrides: Partial<AppRoute> = {}): StubRoute {
  const tag = overrides.tag ?? `apm-stub-${serial++}`;
  const load = vi.fn(overrides.load ?? (async () => definePage(tag)));
  return { path, tag, label: path.slice(1), icon: 'file', ...overrides, load } as StubRoute;
}

function deferred<T = void>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const until = (check: () => unknown) => vi.waitFor(check, { interval: 1, timeout: 2000 });

/** Lets every pending promise and timer run, then the host's render. */
async function idle(host: RouterHost) {
  await new Promise((resolve) => setTimeout(resolve));
  await host.updateComplete;
}

function visit(url: string) {
  window.history.replaceState(null, '', url);
  syncFromLocation();
}

async function mountAt(url: string, options: AppRouterOptions) {
  visit(url);
  return mount('apm-router-host', { options: { redirects: {}, ...options } });
}

const shows = (host: RouterHost, route: StubRoute) => host.querySelector(route.tag) !== null;

let consoleError: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  connections.length = 0;
  consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
  visit('/');
});

afterEach(() => {
  consoleError.mockRestore();
});

describe('rendering pages', () => {
  it('renders the page of the current address once its module has loaded', async () => {
    const a = stubRoute('/a');
    const b = stubRoute('/b');

    const host = await mountAt('/a', { routes: [a, b] });
    await until(() => expect(shows(host, a)).toBe(true));

    expect(host.querySelector(a.tag)?.className).toBe('block h-full min-h-0');
    expect(shows(host, b)).toBe(false);
  });

  it('imports a page only when it is visited, and only once', async () => {
    const a = stubRoute('/a');
    const b = stubRoute('/b');

    const host = await mountAt('/a', { routes: [a, b] });
    await until(() => expect(shows(host, a)).toBe(true));
    expect(b.load).not.toHaveBeenCalled();

    navigate('/b');
    await until(() => expect(shows(host, b)).toBe(true));
    navigate('/a');
    await until(() => expect(shows(host, a)).toBe(true));
    navigate('/b');
    await until(() => expect(shows(host, b)).toBe(true));

    expect(a.load).toHaveBeenCalledTimes(1);
    expect(b.load).toHaveBeenCalledTimes(1);
  });

  it('renders nothing until the first page has loaded', async () => {
    const gate = deferred();
    const a = stubRoute('/a');
    a.load.mockImplementation(async () => {
      await gate.promise;
      definePage(a.tag);
    });

    const host = await mountAt('/a', { routes: [a] });
    await idle(host);
    expect(host.children).toHaveLength(0);

    gate.resolve();
    await until(() => expect(shows(host, a)).toBe(true));
  });

  it('assigns the route props before the page connects', async () => {
    const a = stubRoute('/a', { props: { note: 'hello' } });

    const host = await mountAt('/a', { routes: [a] });
    await until(() => expect(shows(host, a)).toBe(true));

    expect(connections).toEqual([{ tag: a.tag, note: 'hello' }]);
  });

  it('gives each route its own element even when routes share a tag', async () => {
    const first = stubRoute('/first', { tag: 'apm-stub-shared', props: { note: 'one' } });
    const second = stubRoute('/second', { tag: 'apm-stub-shared', props: { note: 'two' } });

    const host = await mountAt('/first', { routes: [first, second] });
    await until(() => expect(connections).toHaveLength(1));
    const before = host.querySelector('apm-stub-shared');
    navigate('/second');
    await until(() => expect(connections).toHaveLength(2));

    expect(host.querySelectorAll('apm-stub-shared')).toHaveLength(1);
    expect(host.querySelector('apm-stub-shared')).not.toBe(before);
    expect(connections.map((connection) => connection.note)).toEqual(['one', 'two']);
  });

  it('keeps the element when only the query changes', async () => {
    const a = stubRoute('/a');

    const host = await mountAt('/a', { routes: [a] });
    await until(() => expect(shows(host, a)).toBe(true));
    const page = host.querySelector(a.tag);
    navigate('/a?src=photo');
    await idle(host);

    expect(host.querySelector(a.tag)).toBe(page);
    expect(connections).toHaveLength(1);
    expect(locations.source.value.volume).toBe('photo');
  });

  it('accepts a trailing slash', async () => {
    const a = stubRoute('/a');

    const host = await mountAt('/a/', { routes: [a] });
    await until(() => expect(shows(host, a)).toBe(true));

    expect(currentPath.value).toBe('/a');
  });
});

describe('redirects', () => {
  it('forwards / and keeps the query and the hash', async () => {
    const a = stubRoute('/a');

    const host = await mountAt('/?src=photo/2024#top', { routes: [a], redirects: { '/': '/a' } });
    await until(() => expect(shows(host, a)).toBe(true));

    expect(window.location.pathname).toBe('/a');
    expect(window.location.search).toBe('?src=photo/2024');
    expect(window.location.hash).toBe('#top');
    expect(currentPath.value).toBe('/a');
    expect(locations.source.value).toEqual({ volume: 'photo', dir: '2024' });
  });

  it('replaces the history entry instead of adding one', async () => {
    const a = stubRoute('/a');
    visit('/');
    const before = window.history.length;

    const host = await mount('apm-router-host', {
      options: { routes: [a], redirects: { '/': '/a' } },
    });
    await until(() => expect(shows(host, a)).toBe(true));

    expect(window.history.length).toBe(before);
  });

  it('forwards to the browser by default', async () => {
    const browse = stubRoute('/browse');

    visit('/');
    const host = await mount('apm-router-host', { options: { routes: [browse] } });
    await until(() => expect(shows(host, browse)).toBe(true));

    expect(window.location.pathname).toBe('/browse');
  });
});

describe('navigation', () => {
  it('follows Back and Forward and keeps the nav store in step', async () => {
    const a = stubRoute('/a');
    const b = stubRoute('/b');

    const host = await mountAt('/a', { routes: [a, b] });
    await until(() => expect(shows(host, a)).toBe(true));
    navigate('/b?src=photo');
    await until(() => expect(shows(host, b)).toBe(true));

    expect(currentPath.value).toBe('/b');
    expect(locations.source.value.volume).toBe('photo');

    window.history.replaceState(null, '', '/a');
    window.dispatchEvent(new PopStateEvent('popstate'));
    await until(() => expect(shows(host, a)).toBe(true));

    expect(currentPath.value).toBe('/a');
    expect(locations.source.value.volume).toBeNull();
  });

  it('turns clicks on plain links into navigations', async () => {
    const a = stubRoute('/a');
    const b = stubRoute('/b');
    const host = await mountAt('/a', { routes: [a, b] });
    await until(() => expect(shows(host, a)).toBe(true));
    const link = document.createElement('a');
    link.href = '/b?src=backup/2023';
    document.body.append(link);

    link.click();
    await until(() => expect(shows(host, b)).toBe(true));
    link.remove();

    expect(window.location.pathname).toBe('/b');
    expect(currentPath.value).toBe('/b');
    expect(locations.source.value).toEqual({ volume: 'backup', dir: '2023' });
  });

  it('stops listening once the host is gone', async () => {
    const a = stubRoute('/a');
    const b = stubRoute('/b');
    const host = await mountAt('/a', { routes: [a, b] });
    await until(() => expect(shows(host, a)).toBe(true));

    host.remove();
    navigate('/b');
    await new Promise((resolve) => setTimeout(resolve));

    expect(b.load).not.toHaveBeenCalled();
  });

  it('drops a page that finishes loading after the user moved on', async () => {
    const gate = deferred();
    const slow = stubRoute('/slow');
    slow.load.mockImplementation(async () => {
      await gate.promise;
      definePage(slow.tag);
    });
    const fast = stubRoute('/fast');

    const host = await mountAt('/slow', { routes: [slow, fast] });
    navigate('/fast');
    await until(() => expect(shows(host, fast)).toBe(true));
    gate.resolve();
    await idle(host);

    expect(shows(host, fast)).toBe(true);
    expect(shows(host, slow)).toBe(false);
    expect(currentPath.value).toBe('/fast');
  });
});

describe('not found', () => {
  it('shows a "Page not found" state with a way back', async () => {
    const a = stubRoute('/a');

    const host = await mountAt('/nowhere', { routes: [a] });
    await until(() => expect(host.querySelector('apm-route-placeholder')).not.toBeNull());
    const state = host.querySelector('apm-route-placeholder');
    await state?.updateComplete;

    expect(state?.querySelector('h2')?.textContent).toBe('Page not found');
    expect(state?.querySelector('a.btn')?.getAttribute('href')).toBe('/browse');
    expect(shows(host, a)).toBe(false);
  });

  it('also covers deeper addresses of a known page', async () => {
    const a = stubRoute('/a');

    const host = await mountAt('/a/extra', { routes: [a] });
    await until(() => expect(host.querySelector('apm-route-placeholder')).not.toBeNull());

    expect(shows(host, a)).toBe(false);
  });

  it('leaves the not-found state when the user follows a link', async () => {
    const a = stubRoute('/a');
    const host = await mountAt('/nowhere', { routes: [a] });
    await until(() => expect(host.querySelector('apm-route-placeholder')).not.toBeNull());

    navigate('/a');
    await until(() => expect(shows(host, a)).toBe(true));

    expect(host.querySelector('apm-route-placeholder')).toBeNull();
  });
});

describe('load failures', () => {
  const alertOf = (host: RouterHost) => host.querySelector('[role="alert"]');
  const retryButton = (host: RouterHost) =>
    [...host.querySelectorAll('button')].find((button) => button.textContent?.trim() === 'Retry');

  it('shows an alert with a Retry button that loads the page again', async () => {
    const a = stubRoute('/a');
    a.load.mockRejectedValueOnce(new Error('offline'));

    const host = await mountAt('/a', { routes: [a] });
    await until(() => expect(alertOf(host)).not.toBeNull());

    expect(alertOf(host)?.textContent).toContain('a could not be loaded');
    expect(consoleError).toHaveBeenCalled();
    expect(shows(host, a)).toBe(false);

    retryButton(host)?.click();
    await until(() => expect(shows(host, a)).toBe(true));

    expect(alertOf(host)).toBeNull();
    expect(a.load).toHaveBeenCalledTimes(2);
  });

  it('keeps the alert, and the Retry button, when the retry fails too', async () => {
    const a = stubRoute('/a');
    a.load.mockRejectedValueOnce(new Error('offline')).mockRejectedValueOnce(new Error('still'));

    const host = await mountAt('/a', { routes: [a] });
    await until(() => expect(alertOf(host)).not.toBeNull());
    retryButton(host)?.click();
    await until(() => expect(a.load).toHaveBeenCalledTimes(2));
    await idle(host);

    expect(alertOf(host)).not.toBeNull();
    expect(retryButton(host)?.disabled).toBe(false);

    retryButton(host)?.click();
    await until(() => expect(shows(host, a)).toBe(true));
  });

  it('shows that it is retrying, and disables the button meanwhile', async () => {
    const gate = deferred();
    const a = stubRoute('/a');
    a.load.mockRejectedValueOnce(new Error('offline'));
    const host = await mountAt('/a', { routes: [a] });
    await until(() => expect(alertOf(host)).not.toBeNull());
    a.load.mockImplementationOnce(async () => {
      await gate.promise;
      definePage(a.tag);
    });

    retryButton(host)?.click();
    await until(() => expect(retryButton(host)?.disabled).toBe(true));
    expect(retryButton(host)?.querySelector('.loading')).not.toBeNull();

    gate.resolve();
    await until(() => expect(shows(host, a)).toBe(true));
  });

  it('treats a module that does not define its tag as a failure', async () => {
    const a = stubRoute('/a', { tag: 'apm-stub-never-defined', load: async () => undefined });

    const host = await mountAt('/a', { routes: [a] });
    await until(() => expect(alertOf(host)).not.toBeNull());

    expect(String(consoleError.mock.calls[0]?.[1])).toContain(
      'did not define <apm-stub-never-defined>',
    );
  });

  it('does not poison other pages', async () => {
    const broken = stubRoute('/broken');
    broken.load.mockRejectedValue(new Error('nope'));
    const fine = stubRoute('/fine');

    const host = await mountAt('/broken', { routes: [broken, fine] });
    await until(() => expect(alertOf(host)).not.toBeNull());
    navigate('/fine');
    await until(() => expect(shows(host, fine)).toBe(true));

    expect(alertOf(host)).toBeNull();
  });

  it('ignores the failure of a page the user has already left', async () => {
    const gate = deferred();
    const slow = stubRoute('/slow');
    slow.load.mockImplementation(async () => {
      await gate.promise;
      throw new Error('late');
    });
    const fast = stubRoute('/fast');

    const host = await mountAt('/slow', { routes: [slow, fast] });
    navigate('/fast');
    await until(() => expect(shows(host, fast)).toBe(true));
    gate.resolve();
    await idle(host);

    expect(alertOf(host)).toBeNull();
    expect(shows(host, fast)).toBe(true);
  });
});

describe('setup', () => {
  it('rejects a route whose tag is not a custom element name', () => {
    const host = new RouterHost();

    expect(() =>
      createAppRouter(host, { routes: [stubRoute('/x', { tag: 'Not A Tag' })] }),
    ).toThrow(/"Not A Tag" is not a custom element name/);
  });

  it('rejects a tag that could break out of the template', () => {
    const host = new RouterHost();

    expect(() =>
      createAppRouter(host, { routes: [stubRoute('/x', { tag: 'a-b><script' })] }),
    ).toThrow(/not a custom element name/);
  });
});
