import { afterEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@/test/helpers';
import { sampleSettings } from '@/test/settings-fixtures';
import type { ApmSettingsPage } from '@/tools/settings/apm-settings-page';
import '@/tools/settings/apm-settings-page';
import type { SettingsSection } from '@/tools/settings/sections';

for (const name of ['alpha', 'beta', 'gamma']) {
  customElements.define(`apm-test-${name}`, class extends HTMLElement {});
}

function section(
  name: string,
  load: SettingsSection['load'] = vi.fn(async () => undefined),
): SettingsSection {
  return { id: name, label: name.toUpperCase(), icon: 'settings', tag: `apm-test-${name}`, load };
}

function deferred() {
  let resolve!: () => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** Lets the imports of the sections finish and the page draw what they brought. */
async function settle(page: ApmSettingsPage): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await page.updateComplete;
}

async function open(sections: readonly SettingsSection[], address = '/settings') {
  window.history.replaceState(null, '', address);
  const page = await mount('apm-settings-page', { sections });
  await settle(page);
  return page;
}

const tabs = (page: HTMLElement) => [...page.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
const tab = (page: HTMLElement, name: string) =>
  page.querySelector<HTMLButtonElement>(`#settings-tab-${name}`) as HTMLButtonElement;
const panel = (page: HTMLElement, name: string) =>
  page.querySelector<HTMLElement>(`#settings-panel-${name}`);
const selected = (page: HTMLElement) =>
  tabs(page)
    .filter((candidate) => candidate.getAttribute('aria-selected') === 'true')
    .map((candidate) => candidate.id.replace('settings-tab-', ''));
const shown = (page: HTMLElement) =>
  [...page.querySelectorAll<HTMLElement>('[role="tabpanel"]')]
    .filter((candidate) => !candidate.hidden)
    .map((candidate) => candidate.id.replace('settings-panel-', ''));

async function choose(page: ApmSettingsPage, name: string): Promise<void> {
  tab(page, name).click();
  await settle(page);
}

function press(page: HTMLElement, name: string, key: string, init: KeyboardEventInit = {}) {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
  tab(page, name).dispatchEvent(event);
  return event;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.history.replaceState(null, '', '/');
});

describe('apm-settings-page', () => {
  describe('layout', () => {
    it('has a title, and the shell keeps the only h1', async () => {
      const page = await open([section('alpha')]);

      expect(page.querySelector('h2')?.textContent).toBe('Server settings');
      expect(page.querySelector('h1')).toBeNull();
    });

    it('fills the area the router gives it, and only its body scrolls', async () => {
      const page = await open([section('alpha')]);

      expect(['block', 'h-full', 'min-h-0'].every((name) => page.classList.contains(name))).toBe(
        true,
      );
      const scrolling = [...page.querySelectorAll('div')].filter((element) =>
        element.classList.contains('overflow-y-auto'),
      );
      expect(scrolling).toHaveLength(1);
      expect(scrolling[0]?.contains(panel(page, 'alpha'))).toBe(true);
      expect(scrolling[0]?.contains(page.querySelector('[role="tablist"]'))).toBe(false);
    });
  });

  describe('tabs', () => {
    it('are built from the sections, in order, with their label and icon', async () => {
      const page = await open([section('alpha'), section('beta'), section('gamma')]);

      expect(tabs(page).map((candidate) => candidate.textContent?.trim())).toEqual([
        'ALPHA',
        'BETA',
        'GAMMA',
      ]);
      expect(tabs(page).map((candidate) => candidate.id)).toEqual([
        'settings-tab-alpha',
        'settings-tab-beta',
        'settings-tab-gamma',
      ]);
      expect(tabs(page).every((candidate) => candidate.classList.contains('tab'))).toBe(true);
      expect(
        tabs(page).map((candidate) => candidate.querySelector('apm-icon')?.getAttribute('name')),
      ).toEqual(['settings', 'settings', 'settings']);
    });

    it('are a DaisyUI border tablist with a name', async () => {
      const page = await open([section('alpha')]);

      const list = page.querySelector('[role="tablist"]');
      expect(list?.classList.contains('tabs')).toBe(true);
      expect(list?.classList.contains('tabs-border')).toBe(true);
      expect(list?.getAttribute('aria-label')).toBe('Settings sections');
    });

    it('mark the open one, and only that one is in the tab order', async () => {
      const page = await open([section('alpha'), section('beta')], '/settings?section=beta');

      expect(selected(page)).toEqual(['beta']);
      expect(tab(page, 'beta').classList.contains('tab-active')).toBe(true);
      expect(tab(page, 'alpha').classList.contains('tab-active')).toBe(false);
      expect(tab(page, 'beta').getAttribute('tabindex')).toBe('0');
      expect(tab(page, 'alpha').getAttribute('tabindex')).toBe('-1');
    });

    it('point at the panel they open, and the panel at its tab', async () => {
      const page = await open([section('alpha'), section('beta')]);

      expect(tab(page, 'alpha').getAttribute('aria-controls')).toBe('settings-panel-alpha');
      expect(tab(page, 'beta').hasAttribute('aria-controls')).toBe(false);
      expect(panel(page, 'alpha')?.getAttribute('role')).toBe('tabpanel');
      expect(panel(page, 'alpha')?.getAttribute('aria-labelledby')).toBe('settings-tab-alpha');
    });

    it('are buttons that never submit a form', async () => {
      const page = await open([section('alpha'), section('beta')]);

      for (const candidate of tabs(page)) {
        expect(candidate.tagName).toBe('BUTTON');
        expect(candidate.getAttribute('type')).toBe('button');
      }
    });
  });

  describe('the open section', () => {
    it('is the first one when the address names none', async () => {
      const first = section('alpha');
      const second = section('beta');
      const page = await open([first, second], '/settings');

      expect(selected(page)).toEqual(['alpha']);
      expect(shown(page)).toEqual(['alpha']);
      expect(first.load).toHaveBeenCalledTimes(1);
      expect(second.load).not.toHaveBeenCalled();
    });

    it('is the one named by ?section=', async () => {
      const first = section('alpha');
      const second = section('beta');
      const page = await open([first, second], '/settings?section=beta');

      expect(selected(page)).toEqual(['beta']);
      expect(shown(page)).toEqual(['beta']);
      expect(panel(page, 'alpha')).toBeNull();
      expect(first.load).not.toHaveBeenCalled();
      expect(second.load).toHaveBeenCalledTimes(1);
    });

    it('is the first one when the id is unknown', async () => {
      const first = section('alpha');
      const page = await open([first, section('beta')], '/settings?section=nope');

      expect(selected(page)).toEqual(['alpha']);
      expect(first.load).toHaveBeenCalledTimes(1);
    });

    it('follows Back and Forward', async () => {
      const page = await open([section('alpha'), section('beta')]);

      window.history.replaceState(null, '', '/settings?section=beta');
      window.dispatchEvent(new PopStateEvent('popstate'));
      await settle(page);
      expect(selected(page)).toEqual(['beta']);

      window.history.replaceState(null, '', '/settings');
      window.dispatchEvent(new PopStateEvent('popstate'));
      await settle(page);
      expect(selected(page)).toEqual(['alpha']);
    });

    it('stops listening to the address when the page is gone', async () => {
      const page = await open([section('alpha'), section('beta')]);
      page.remove();

      window.history.replaceState(null, '', '/settings?section=beta');
      window.dispatchEvent(new PopStateEvent('popstate'));

      expect(page.requested).toBeNull();
    });

    it('shows nothing, and does not fail, when there are no sections', async () => {
      const page = await open([]);

      expect(tabs(page)).toEqual([]);
      expect(page.querySelectorAll('[role="tabpanel"]')).toHaveLength(0);
    });
  });

  describe('switching', () => {
    it('opens the clicked tab and writes it to the address without adding a history entry', async () => {
      const page = await open([section('alpha'), section('beta')]);
      const entries = window.history.length;

      await choose(page, 'beta');

      expect(selected(page)).toEqual(['beta']);
      expect(window.location.pathname).toBe('/settings');
      expect(new URLSearchParams(window.location.search).get('section')).toBe('beta');
      expect(window.history.length).toBe(entries);
    });

    it('keeps the other query keys and the hash, and the slashes readable', async () => {
      const page = await open(
        [section('alpha'), section('beta')],
        '/settings?src=photo/2024/trip&cmp=backup&section=alpha#top',
      );

      await choose(page, 'beta');

      expect(window.location.search).toBe('?src=photo/2024/trip&cmp=backup&section=beta');
      expect(window.location.hash).toBe('#top');
    });

    it('keeps the history state of the page', async () => {
      const page = await open([section('alpha'), section('beta')]);
      window.history.replaceState({ scroll: 3 }, '', '/settings');

      await choose(page, 'beta');

      expect(window.history.state).toEqual({ scroll: 3 });
    });

    it('does nothing when the open tab is clicked again', async () => {
      const replace = vi.spyOn(window.history, 'replaceState');
      const first = section('alpha');
      const page = await open([first, section('beta')]);

      await choose(page, 'alpha');

      expect(replace).toHaveBeenCalledTimes(1);
      expect(first.load).toHaveBeenCalledTimes(1);
    });

    it('imports each section once, however often its tab is opened', async () => {
      const first = section('alpha');
      const second = section('beta');
      const page = await open([first, second]);

      await choose(page, 'beta');
      await choose(page, 'alpha');
      await choose(page, 'beta');
      await choose(page, 'alpha');

      expect(first.load).toHaveBeenCalledTimes(1);
      expect(second.load).toHaveBeenCalledTimes(1);
    });

    it('renders a section it has never opened only after its tab is chosen', async () => {
      const page = await open([section('alpha'), section('beta'), section('gamma')]);
      expect(page.querySelector('apm-test-beta')).toBeNull();

      await choose(page, 'beta');

      expect(page.querySelector('apm-test-beta')).not.toBeNull();
      expect(page.querySelector('apm-test-gamma')).toBeNull();
    });

    it('renders the tag of each section inside its own panel, with nothing passed to it', async () => {
      const page = await open([section('alpha'), section('beta')]);
      await choose(page, 'beta');

      for (const name of ['alpha', 'beta']) {
        const element = panel(page, name)?.querySelector(`apm-test-${name}`);
        expect(element, name).not.toBeNull();
        expect(element?.attributes).toHaveLength(0);
        expect(element?.childNodes).toHaveLength(0);
      }
    });

    it('keeps a section that was left in the page, hidden, so that its edits survive', async () => {
      const page = await open([section('alpha'), section('beta')]);
      const original = page.querySelector('apm-test-alpha') as HTMLElement;
      original.setAttribute('data-edit', 'unsaved');

      await choose(page, 'beta');

      expect(panel(page, 'alpha')?.hidden).toBe(true);
      expect(panel(page, 'beta')?.hidden).toBe(false);
      expect(shown(page)).toEqual(['beta']);
      expect(page.querySelector('apm-test-alpha')).toBe(original);

      await choose(page, 'alpha');

      expect(shown(page)).toEqual(['alpha']);
      expect(page.querySelector('apm-test-alpha')).toBe(original);
      expect(original.getAttribute('data-edit')).toBe('unsaved');
      expect(panel(page, 'beta')?.hidden).toBe(true);
    });

    it('does not rebuild the tabs when switching', async () => {
      const page = await open([section('alpha'), section('beta')]);
      const buttons = tabs(page);

      await choose(page, 'beta');

      expect(tabs(page)).toEqual(buttons);
    });
  });

  describe('keyboard', () => {
    it('moves to the next tab with ArrowRight, focuses it and wraps at the end', async () => {
      const page = await open([section('alpha'), section('beta'), section('gamma')]);

      const first = press(page, 'alpha', 'ArrowRight');
      await settle(page);
      expect(first.defaultPrevented).toBe(true);
      expect(selected(page)).toEqual(['beta']);
      expect(document.activeElement).toBe(tab(page, 'beta'));

      press(page, 'beta', 'ArrowRight');
      await settle(page);
      expect(selected(page)).toEqual(['gamma']);

      press(page, 'gamma', 'ArrowRight');
      await settle(page);
      expect(selected(page)).toEqual(['alpha']);
      expect(document.activeElement).toBe(tab(page, 'alpha'));
    });

    it('moves to the previous tab with ArrowLeft and wraps at the start', async () => {
      const page = await open([section('alpha'), section('beta'), section('gamma')]);

      press(page, 'alpha', 'ArrowLeft');
      await settle(page);
      expect(selected(page)).toEqual(['gamma']);
      expect(document.activeElement).toBe(tab(page, 'gamma'));

      press(page, 'gamma', 'ArrowLeft');
      await settle(page);
      expect(selected(page)).toEqual(['beta']);
    });

    it('goes to the first and the last tab with Home and End', async () => {
      const page = await open([section('alpha'), section('beta'), section('gamma')]);

      press(page, 'alpha', 'End');
      await settle(page);
      expect(selected(page)).toEqual(['gamma']);

      press(page, 'gamma', 'Home');
      await settle(page);
      expect(selected(page)).toEqual(['alpha']);
      expect(document.activeElement).toBe(tab(page, 'alpha'));
    });

    it('writes the address as a click does', async () => {
      const page = await open([section('alpha'), section('beta')]);

      press(page, 'alpha', 'ArrowRight');
      await settle(page);

      expect(new URLSearchParams(window.location.search).get('section')).toBe('beta');
    });

    it('leaves other keys and the browser shortcuts alone', async () => {
      const page = await open([section('alpha'), section('beta')]);

      const keys = [
        press(page, 'alpha', 'Tab'),
        press(page, 'alpha', 'Enter'),
        press(page, 'alpha', 'ArrowLeft', { altKey: true }),
        press(page, 'alpha', 'ArrowRight', { metaKey: true }),
        press(page, 'alpha', 'ArrowRight', { ctrlKey: true }),
      ];
      await settle(page);

      expect(keys.map((event) => event.defaultPrevented)).toEqual([
        false,
        false,
        false,
        false,
        false,
      ]);
      expect(selected(page)).toEqual(['alpha']);
    });

    it('stays on the only tab', async () => {
      const page = await open([section('alpha')]);

      press(page, 'alpha', 'ArrowRight');
      await settle(page);

      expect(selected(page)).toEqual(['alpha']);
    });
  });

  describe('loading a section', () => {
    it('shows a spinner for the open section until its module is in', async () => {
      const gate = deferred();
      const page = await open([section('alpha', () => gate.promise)]);

      const status = panel(page, 'alpha')?.querySelector('[role="status"]');
      expect(status?.getAttribute('aria-label')).toBe('Loading ALPHA');
      expect(status?.querySelector('.loading')).not.toBeNull();
      expect(page.querySelector('apm-test-alpha')).toBeNull();

      gate.resolve();
      await settle(page);

      expect(panel(page, 'alpha')?.querySelector('[role="status"]')).toBeNull();
      expect(page.querySelector('apm-test-alpha')).not.toBeNull();
    });

    it('does not import a section again while it is still loading', async () => {
      const gate = deferred();
      const slow = section(
        'alpha',
        vi.fn(() => gate.promise),
      );
      const page = await open([slow, section('beta')]);

      await choose(page, 'beta');
      await choose(page, 'alpha');

      expect(slow.load).toHaveBeenCalledTimes(1);
      gate.resolve();
    });

    it('says which section failed, with a Retry that imports it again', async () => {
      const error = vi.spyOn(console, 'error').mockImplementation(() => {});
      const load = vi.fn<() => Promise<unknown>>().mockRejectedValueOnce(new Error('offline'));
      load.mockResolvedValue(undefined);
      const page = await open([section('alpha', load)]);

      const alert = panel(page, 'alpha')?.querySelector('[role="alert"]');
      expect(alert?.classList.contains('alert-error')).toBe(true);
      expect(alert?.textContent).toContain('ALPHA could not be loaded');
      expect(page.querySelector('apm-test-alpha')).toBeNull();
      expect(error).toHaveBeenCalledTimes(1);

      const retry = [...(alert?.querySelectorAll('button') ?? [])].find(
        (button) => button.textContent?.trim() === 'Retry',
      );
      retry?.click();
      await settle(page);

      expect(load).toHaveBeenCalledTimes(2);
      expect(panel(page, 'alpha')?.querySelector('[role="alert"]')).toBeNull();
      expect(page.querySelector('apm-test-alpha')).not.toBeNull();
    });

    it('does not try a failed section again by itself', async () => {
      vi.spyOn(console, 'error').mockImplementation(() => {});
      const load = vi.fn<() => Promise<unknown>>().mockRejectedValue(new Error('offline'));
      const page = await open([section('alpha', load), section('beta')]);

      await choose(page, 'beta');
      await choose(page, 'alpha');
      page.requestUpdate();
      await settle(page);

      expect(load).toHaveBeenCalledTimes(1);
      expect(panel(page, 'alpha')?.querySelector('[role="alert"]')).not.toBeNull();
    });

    it('keeps the other tabs usable when one section fails', async () => {
      vi.spyOn(console, 'error').mockImplementation(() => {});
      const load = vi.fn<() => Promise<unknown>>().mockRejectedValue(new Error('offline'));
      const page = await open([section('alpha', load), section('beta')]);

      await choose(page, 'beta');

      expect(page.querySelector('apm-test-beta')).not.toBeNull();
      expect(shown(page)).toEqual(['beta']);
    });

    it('treats a module that does not define its tag as a failure', async () => {
      vi.spyOn(console, 'error').mockImplementation(() => {});
      const lazy = { ...section('alpha'), tag: 'apm-test-undefined' };
      const page = await open([lazy]);

      expect(panel(page, 'alpha')?.querySelector('[role="alert"]')).not.toBeNull();
      expect(page.querySelector('apm-test-undefined')).toBeNull();
    });

    it('refuses a tag that is not a custom element name, without importing anything', async () => {
      vi.spyOn(console, 'error').mockImplementation(() => {});
      const load = vi.fn(async () => undefined);
      const page = await open([{ ...section('alpha'), tag: 'div onload=x', load }]);

      expect(load).not.toHaveBeenCalled();
      expect(panel(page, 'alpha')?.querySelector('[role="alert"]')).not.toBeNull();
      expect(page.querySelector('div[onload]')).toBeNull();
    });
  });

  describe('the registry', () => {
    it('opens the general section from the registry, and imports its module on demand', async () => {
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => new Response(JSON.stringify(sampleSettings()))),
      );
      expect(customElements.get('apm-settings-general')).toBeUndefined();

      const page = await mount('apm-settings-page');
      await vi.waitFor(() => expect(page.querySelector('apm-settings-general')).not.toBeNull());
      await vi.waitFor(() => expect(page.querySelector('form')).not.toBeNull());

      expect(customElements.get('apm-settings-general')).toBeDefined();
      expect(tabs(page).map((candidate) => candidate.textContent?.trim())).toEqual([
        'General',
        'Immich',
      ]);
      expect(selected(page)).toEqual(['general']);
      expect(panel(page, 'general')?.querySelector('apm-settings-general')).not.toBeNull();
    });
  });
});
