import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { events } from '@/api/sse';
import { routeAt } from '@/app/routes';
import type { OpmBottomBar } from '@/components/opm-bottom-bar';
import type { OpmPane } from '@/components/opm-pane';
import { filterScopes, resetFilters } from '@/stores/filters';
import { PANES, syncFromLocation } from '@/stores/nav';
import { GRID_SIZE, setGridSize, setSort } from '@/stores/prefs';
import { resetSelectionForTests } from '@/stores/selection';
import { clearToasts } from '@/stores/toasts';
import { resetToolStateForTests } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { type BrowseServer, stubBrowseServer } from '@/test/browse-server';
import { mount } from '@/test/helpers';
import type { OpmBrowsePage } from '@/tools/browse/opm-browse-page';
import { browseTool } from '@/tools/browse/tool';
import type { OpmToolView } from '@/tools/framework/opm-tool-view';
import { checkToolConfig } from '@/tools/framework/tool-config';
import { EMPTY_FILTER } from '@/util/filter';

let server: BrowseServer;

beforeEach(async () => {
  window.history.replaceState(null, '', '/');
  syncFromLocation();
  server = stubBrowseServer();
  await loadVolumes();
});

afterEach(() => {
  // The page goes first: a mounted pane would fetch again for the resets below.
  for (const page of document.querySelectorAll('opm-browse-page')) page.remove();
  events.stop();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  clearToasts();
  resetSelectionForTests();
  resetToolStateForTests();
  resetFilters();
  filterScopes.value = {};
  for (const pane of PANES) {
    setSort(pane, { key: 'name', order: 'asc' });
    setGridSize(pane, GRID_SIZE.default);
  }
  window.history.replaceState(null, '', '/');
  syncFromLocation();
});

type LitElement = Element & { updateComplete: Promise<unknown> };

const litElementsIn = (root: Element): LitElement[] =>
  [root, ...root.querySelectorAll('*')].filter(
    (element): element is LitElement => 'updateComplete' in element,
  );

/** Lets the stubbed server answer and the page and everything in it render what came back. */
async function settle(page: Element, rounds = 6): Promise<void> {
  for (let round = 0; round < rounds; round += 1) {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await Promise.all(litElementsIn(page).map((element) => element.updateComplete));
  }
}

async function mountPage(url = '/browse'): Promise<OpmBrowsePage> {
  window.history.replaceState(null, '', url);
  syncFromLocation();
  const page = await mount('opm-browse-page');
  await settle(page);
  return page;
}

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

const panesOf = (page: Element) => [...page.querySelectorAll<OpmPane>('opm-pane')];

describe('the Browse route', () => {
  const route = routeAt('/browse');

  it('is the first page of the tools menu', () => {
    expect(route).toMatchObject({
      path: '/browse',
      tag: 'opm-browse-page',
      label: 'Browse',
      icon: 'images',
      menu: 'tools',
    });
  });

  it('loads the module that defines its page element, on the first visit', async () => {
    const module = (await route?.load()) as typeof import('@/tools/browse/opm-browse-page');

    expect(customElements.get('opm-browse-page')).toBe(module.OpmBrowsePage);
  });
});

describe('the Browse page', () => {
  it('shows the tool view with the Browse tool, filling the height it is given', async () => {
    const page = await mountPage();

    const view = page.querySelector<OpmToolView>('opm-tool-view');
    expect(view?.config).toBe(browseTool);
    expect(page.classList.contains('block')).toBe(true);
    expect(page.classList.contains('h-full')).toBe(true);
    expect(page.classList.contains('min-h-0')).toBe(true);
  });

  it('is headed Browse and says what the page is for', async () => {
    const page = await mountPage();

    expect(page.querySelector('h2')).toBeNull();
    expect(page.querySelector('opm-modal')?.heading).toBe('Browse');
    expect(text(page.querySelector('opm-modal'))).toContain(browseTool.summary);
  });

  it('shows the source pane and the comparison pane', async () => {
    const page = await mountPage();

    const panes = panesOf(page);
    expect(panes.map((pane) => pane.pane)).toEqual(['source', 'comparison']);
    expect(panes.map((pane) => pane.heading)).toEqual(['Source', 'Comparison']);
  });

  it('puts each pane where its address says', async () => {
    await mountPage('/browse?src=photos/trip&cmp=backup/old');

    expect(server.calls.folders).toEqual([
      { volume_id: 1, dir: 'trip', filter: EMPTY_FILTER, plan: null },
      { volume_id: 2, dir: 'old', filter: EMPTY_FILTER, plan: null },
    ]);
  });

  it('counts both panes in the bottom bar', async () => {
    server.state.folders = () => ({ folders: [], counts: { in_folder: 3, in_subfolders: 1 } });
    const page = await mountPage('/browse?src=photos/trip&cmp=backup/old');

    const bar = page.querySelector<OpmBottomBar>('opm-bottom-bar');
    expect(bar?.source.counts).toEqual({ in_folder: 3, in_subfolders: 1 });
    expect(bar?.comparison?.counts).toEqual({ in_folder: 3, in_subfolders: 1 });
  });

  it('is the framework with nothing added: no plan, no actions, no extra controls', async () => {
    const page = await mountPage('/browse?src=photos/trip&cmp=backup/old');

    expect(text(page)).not.toContain('Include subfolders');
    expect(page.querySelector('header [role="status"]')).toBeNull();
    expect(page.querySelector('opm-bottom-bar opm-split-button')).toBeNull();
    expect(page.querySelector('opm-bottom-bar .btn-lg')).toBeNull();
    for (const pane of panesOf(page)) {
      expect(pane.baseMode).toBe('none');
      expect(pane.showMatches).toBe(false);
      expect(pane.plan).toBeNull();
    }
  });
});

describe('the Browse tool', () => {
  it('is a valid tool config', () => {
    expect(checkToolConfig(browseTool)).toBeNull();
  });

  it('has two plain panes: no base folder, no matches, no plan side', () => {
    for (const pane of PANES) {
      expect(browseTool.panes[pane]).toMatchObject({
        baseMode: 'none',
        showMatches: false,
        planSide: null,
      });
    }
    expect(browseTool.panes.source.heading).toBe('Source');
    expect(browseTool.panes.comparison.heading).toBe('Comparison');
  });

  it('lets the filter bar narrow both panes', () => {
    expect(browseTool.filter).toEqual({ applies: 'both' });
  });

  it('adds nothing to the framework', () => {
    expect(browseTool.plan).toBeUndefined();
    expect(browseTool.actions).toBeUndefined();
    expect(browseTool.controls).toBeUndefined();
    expect(browseTool.setup).toBeUndefined();
    expect(browseTool.recursive).toBeUndefined();
  });
});
