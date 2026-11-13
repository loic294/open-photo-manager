import { signal } from '@preact/signals-core';
import { html } from 'lit';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Facets } from '@/api/generated/Facets';
import type { Filter } from '@/api/generated/Filter';
import { events } from '@/api/sse';
import type { OpmBottomBar } from '@/components/opm-bottom-bar';
import type { OpmFilterBar } from '@/components/opm-filter-bar';
import type { OpmPane } from '@/components/opm-pane';
import type { ViewerSource } from '@/components/opm-viewer';
import {
  filterFor,
  filterScopes,
  resetFilters,
  scopeFor,
  setFilter,
  setScope,
} from '@/stores/filters';
import { locations, PANES, type PaneId, syncFromLocation } from '@/stores/nav';
import { GRID_SIZE, gridSize, hideUnmatchedFolders, setGridSize, setSort } from '@/stores/prefs';
import { activePane, resetSelectionForTests, selection } from '@/stores/selection';
import { clearToasts } from '@/stores/toasts';
import { filtersOpen, toolHelpOpen } from '@/stores/tool-chrome';
import {
  handoffFor,
  pinnedBase,
  recursiveFor,
  resetToolStateForTests,
  setHandoff,
  setNavigationSynced,
} from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import {
  type BrowseServer,
  type BrowseServerState,
  foldersAnswer,
  stubBrowseServer,
} from '@/test/browse-server';
import { mount } from '@/test/helpers';
import { FakePlan, plainPane, toolConfig } from '@/test/tool-config';
import type { OpmToolView } from '@/tools/framework/opm-tool-view';
import '@/tools/framework/opm-tool-view';
import type { PlanBinding } from '@/tools/framework/plan-binding';
import type { ToolConfig } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import { DEFAULT_SCOPE_LABELS, EMPTY_FILTER } from '@/util/filter';

const seam = vi.hoisted(() => ({ make: null as null | ((ctx: unknown) => unknown) }));
const viewer = vi.hoisted(() => ({ open: vi.fn<(source: ViewerSource) => Promise<void>>() }));

// The plan client is not part of these tests: a test hands the context its own binding.
vi.mock('@/tools/framework/plan-binding', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/tools/framework/plan-binding')>();
  return {
    ...actual,
    createPlanBinding: (ctx: ToolContext) =>
      (seam.make?.(ctx) as PlanBinding | undefined) ?? actual.createPlanBinding(ctx),
  };
});

// The viewer is its own component with its own tests: here only what the view hands it matters.
vi.mock('@/components/opm-viewer', () => ({ openViewer: viewer.open }));

const CAMERA: Filter = { ...EMPTY_FILTER, cameras: ['Canon EOS R5'] };

const FACETS: Facets = {
  cameras: [{ value: 'Canon EOS R5', count: 4 }],
  tags: [{ value: 'family', count: 2 }],
  ratings: [{ value: '0', count: 6 }],
};

let server: BrowseServer;

/** The browse server of the pane tests, plus the facets the filter bar asks for. */
function stubServer(initial: Partial<BrowseServerState> = {}): BrowseServer {
  const browse = stubBrowseServer(initial);
  const inner = globalThis.fetch;
  vi.stubGlobal(
    'fetch',
    vi.fn((url: string, init: RequestInit) =>
      url === '/api/facets'
        ? Promise.resolve(new Response(JSON.stringify(FACETS)))
        : inner(url, init),
    ),
  );
  return browse;
}

beforeEach(async () => {
  window.history.replaceState(null, '', '/');
  syncFromLocation();
  server = stubServer();
  viewer.open.mockResolvedValue(undefined);
  await loadVolumes();
});

afterEach(() => {
  // The views go first: one that is still mounted would fetch again for the resets below.
  for (const view of document.querySelectorAll('opm-tool-view')) view.remove();
  events.stop();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  viewer.open.mockReset();
  seam.make = null;
  clearToasts();
  resetSelectionForTests();
  resetToolStateForTests();
  resetFilters();
  filterScopes.value = {};
  hideUnmatchedFolders.value = false;
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

/** Lets the stubbed server answer and the view and everything in it render what came back. */
async function settle(view: Element, rounds = 6): Promise<void> {
  for (let round = 0; round < rounds; round += 1) {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await Promise.all(litElementsIn(view).map((element) => element.updateComplete));
  }
}

async function mountView(config: ToolConfig = toolConfig()): Promise<OpmToolView> {
  const view = await mount('opm-tool-view', { config });
  await settle(view);
  return view;
}

/** Both panes in a real folder: the counts, the ids and the viewer need a location. */
function browseAt(source: string, comparison: string): void {
  window.history.replaceState(null, '', `/browse?src=${source}&cmp=${comparison}`);
  syncFromLocation();
}

function part<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`No element matches "${selector}"`);
  return found;
}

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

function paneOf(view: Element, id: PaneId): OpmPane {
  const found = [...view.querySelectorAll<OpmPane>('opm-pane')].find((pane) => pane.pane === id);
  if (!found) throw new Error(`No ${id} pane`);
  return found;
}

const filterBar = (view: Element) => part<OpmFilterBar>(view, 'opm-filter-bar');
const bottomBar = (view: Element) => part<OpmBottomBar>(view, 'opm-bottom-bar');
const toolBar = (view: Element) => part(view, 'header');
const paneGrid = (view: Element) => part(view, 'opm-pane').parentElement?.parentElement as Element;

const SEGMENT_LABELS: Record<PaneId, string> = {
  source: 'Source pane',
  comparison: 'Comparison pane',
};

const segment = (view: Element, pane: PaneId) =>
  part(bottomBar(view), `[role="group"][aria-label="${SEGMENT_LABELS[pane]}"]`);

function press(key: string, init: KeyboardEventInit = {}): void {
  document.body.dispatchEvent(
    new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init }),
  );
}

function emit<T>(target: Element, type: string, detail: T): void {
  target.dispatchEvent(new CustomEvent<T>(type, { detail, bubbles: true }));
}

/** A tool with a plan on both sides. */
function planConfig(patch: Partial<ToolConfig> = {}): ToolConfig {
  return toolConfig({
    id: 'duplicates',
    plan: { kind: 'duplicates', params: () => ({ ready: true, params: {} }) },
    panes: {
      source: { ...plainPane('Source'), planSide: 'source' },
      comparison: { ...plainPane('Comparison'), planSide: 'comparison' },
    },
    ...patch,
  });
}

/** Makes every context use this binding instead of the plan client. */
function withPlan(): FakePlan {
  const plan = new FakePlan();
  seam.make = () => plan;
  return plan;
}

describe('layout', () => {
  it('keeps restored and revisited locations linked while enabled, and stops when disconnected', async () => {
    setNavigationSynced('browse', true);
    browseAt('photos/one', 'backup/two');
    const view = await mountView();
    expect(locations.comparison.value).toEqual(locations.source.value);

    activePane.value = 'comparison';
    browseAt('photos/one', 'backup/old');
    await settle(view);
    expect(locations.source.value).toEqual({ volume: 'backup', dir: 'old' });
    expect(locations.comparison.value).toEqual(locations.source.value);
    expect(new URLSearchParams(window.location.search).get('src')).toBe('backup/old');

    view.remove();
    browseAt('photos/one', 'backup/two');
    expect(locations.source.value).not.toEqual(locations.comparison.value);
  });

  it('syncs exact folder navigation and scrolling in either direction only while enabled', async () => {
    browseAt('photos/one', 'backup/two');
    const view = await mountView(toolConfig({ recursive: 'toggle' }));
    const toggle = [...view.querySelectorAll<HTMLInputElement>('input[role="switch"]')].find(
      (input) => input.closest('label')?.textContent?.includes('Sync navigation'),
    );
    expect(toggle).toBeDefined();
    if (!toggle) return;
    toggle.checked = true;
    toggle.dispatchEvent(new Event('change'));
    await settle(view);
    expect(locations.comparison.value).toEqual(locations.source.value);
    const comparison = paneOf(view, 'comparison');
    part<HTMLButtonElement>(comparison, 'button[aria-label="Up one folder"]').click();
    await settle(view);
    expect(locations.source.value).toEqual({ volume: 'photos', dir: '' });
    expect(locations.comparison.value).toEqual(locations.source.value);
    const left = part(paneOf(view, 'source'), '[data-pane-body]');
    const right = part(comparison, '[data-pane-body]');
    left.scrollTop = 120;
    left.dispatchEvent(new Event('scroll'));
    expect(right.scrollTop).toBe(120);
    right.dispatchEvent(new Event('scroll'));
    right.scrollTop = 240;
    right.dispatchEvent(new Event('scroll'));
    expect(left.scrollTop).toBe(240);
    toggle.checked = false;
    toggle.dispatchEvent(new Event('change'));
    await settle(view);
    right.scrollTop = 360;
    right.dispatchEvent(new Event('scroll'));
    expect(left.scrollTop).toBe(240);
    part<HTMLButtonElement>(comparison, 'button[aria-label="Up one folder"]').click();
    await settle(view);
    expect(locations.source.value.volume).toBe('photos');
    expect(locations.comparison.value.volume).toBeNull();
  });

  it('fills its parent and keeps the tool bar, the filter bar and the bottom bar fixed', async () => {
    const view = await mountView();
    for (const name of ['flex', 'h-full', 'min-h-0', 'min-w-0', 'flex-col']) {
      expect(view.classList.contains(name), name).toBe(true);
    }
    expect([...view.children].map((child) => child.tagName.toLowerCase())).toEqual([
      'header',
      'div',
      'div',
      'opm-bottom-bar',
      'opm-modal',
    ]);
    const [header, filters, panes, bottom] = [...view.children];
    for (const fixed of [header, filters, bottom]) {
      expect(fixed?.classList.contains('flex-none')).toBe(true);
    }
    for (const name of ['min-h-0', 'flex-1']) {
      expect(panes?.classList.contains(name), name).toBe(true);
    }
  });

  it('puts the panes side by side from the md breakpoint and stacks them below it', async () => {
    const view = await mountView();
    const grid = paneGrid(view);
    for (const name of ['grid', 'grid-cols-1', 'grid-rows-2', 'md:grid-cols-2', 'md:grid-rows-1']) {
      expect(grid.classList.contains(name), name).toBe(true);
    }
    expect(grid.children).toHaveLength(2);
    for (const cell of grid.children) {
      expect(cell.classList.contains('min-h-0')).toBe(true);
      expect(cell.classList.contains('min-w-0')).toBe(true);
    }
  });

  it('lets every ancestor of a pane shrink, so that only the body of a pane scrolls', async () => {
    const view = await mountView();
    for (const id of PANES) {
      const pane = paneOf(view, id);
      expect(part(pane, '[data-pane-body]').classList.contains('overflow-y-auto')).toBe(true);
      for (let node: Element | null = pane; node && node !== view; node = node.parentElement) {
        expect(node.classList.contains('min-h-0'), `${id}: ${node.tagName}`).toBe(true);
      }
    }
  });

  it('keeps the repeated title and description out of the toolbar and shows help on demand', async () => {
    const view = await mountView(
      toolConfig({
        title: 'Look around',
        summary: 'Two panes.',
        help: ['Open a folder.', 'Review the files.'],
      }),
    );
    expect(toolBar(view).querySelector('h2')).toBeNull();
    expect(text(toolBar(view))).not.toContain('Two panes.');
    const modal = part<HTMLElementTagNameMap['opm-modal']>(view, 'opm-modal');
    expect(modal.open).toBe(false);
    toolHelpOpen.value = true;
    await settle(view);
    expect(modal.querySelector('dialog')?.open).toBe(true);
    expect(text(modal)).toContain('Look around');
    expect(text(modal)).toContain('Two panes.');
    expect([...modal.querySelectorAll('li')].map(text)).toEqual([
      'Open a folder.',
      'Review the files.',
    ]);
    await modal.close();
    expect(toolHelpOpen.value).toBe(false);
  });

  it('starts filters collapsed, preserves their values when toggled, and resets on tool entry', async () => {
    setFilter('browse', CAMERA);
    const view = await mountView();
    expect(part<HTMLElement>(view, '#tool-filters').hidden).toBe(true);
    filtersOpen.value = true;
    await settle(view);
    expect(part<HTMLElement>(view, '#tool-filters').hidden).toBe(false);
    filtersOpen.value = false;
    await settle(view);
    expect(filterBar(view).filter).toEqual(CAMERA);
    expect(paneOf(view, 'source').filter).toEqual(CAMERA);
    view.config = toolConfig({ id: 'move' });
    await settle(view);
    expect(part<HTMLElement>(view, '#tool-filters').hidden).toBe(true);
  });

  it('opens controls in a dialog without replacing the pane listings', async () => {
    const view = await mountView(
      toolConfig({
        controls: () => html`<code>{YYYY}/{mm}</code>`,
        controlsDialog: {
          label: 'Edit formatting',
          render: () => html`<input aria-label="Pattern" />`,
        },
      }),
    );
    const pane = paneOf(view, 'source');
    const modal = [...view.querySelectorAll('opm-modal')].find(
      (item) => item.heading === 'Edit formatting',
    );
    expect(modal?.open).toBe(false);
    [...toolBar(view).querySelectorAll('button')]
      .find((item) => text(item) === 'Edit formatting')
      ?.click();
    await settle(view);
    expect(modal?.querySelector('dialog')?.open).toBe(true);
    expect(modal?.querySelector('input[aria-label="Pattern"]')).not.toBeNull();
    await modal?.close();
    expect(paneOf(view, 'source')).toBe(pane);
  });

  it('renders one pane per side with what its spec says', async () => {
    const view = await mountView(
      planConfig({
        panes: {
          source: { heading: 'Keep', baseMode: 'current', showMatches: true, planSide: 'source' },
          comparison: {
            heading: 'Remove',
            baseMode: 'none',
            showMatches: false,
            planSide: 'comparison',
          },
        },
      }),
    );
    expect([...view.querySelectorAll<OpmPane>('opm-pane')].map((pane) => pane.pane)).toEqual([
      'source',
      'comparison',
    ]);
    const [source, comparison] = PANES.map((id) => paneOf(view, id));
    expect([source?.heading, source?.baseMode, source?.showMatches]).toEqual([
      'Keep',
      'current',
      true,
    ]);
    expect([comparison?.heading, comparison?.baseMode, comparison?.showMatches]).toEqual([
      'Remove',
      'none',
      false,
    ]);
  });
});

describe('the filter', () => {
  beforeEach(() => {
    browseAt('photos/trip', 'backup/old');
  });

  it('applies to both panes', async () => {
    setFilter('browse', CAMERA);
    const view = await mountView();
    expect(paneOf(view, 'source').filter).toEqual(CAMERA);
    expect(paneOf(view, 'comparison').filter).toEqual(CAMERA);
    const bar = filterBar(view);
    expect(bar.filter).toEqual(CAMERA);
    expect(bar.scope).toBeNull();
    expect(bar.disabled).toBe(false);
    expect(bar.scopeLabels).toEqual(DEFAULT_SCOPE_LABELS);
    expect(bar.contexts.map((context) => [context.volume_id, context.dir])).toEqual([
      [1, 'trip'],
      [2, 'old'],
    ]);
  });

  it('applies to the source pane only, and the facets follow it', async () => {
    setFilter('browse', CAMERA);
    const view = await mountView(toolConfig({ filter: { applies: 'source' } }));
    expect(paneOf(view, 'source').filter).toEqual(CAMERA);
    expect(paneOf(view, 'comparison').filter).toBe(EMPTY_FILTER);
    const bar = filterBar(view);
    expect(bar.filter).toEqual(CAMERA);
    expect(bar.scope).toBeNull();
    expect(bar.contexts.map((context) => context.volume_id)).toEqual([1]);
  });

  it('lets the user choose where it applies, starting from `initial`', async () => {
    setFilter('duplicates', CAMERA);
    const labels = { both: 'Both panes', source: 'Left', comparison: 'Right' };
    const view = await mountView(
      toolConfig({ id: 'duplicates', filter: { applies: 'choice', labels, initial: 'source' } }),
    );
    const bar = filterBar(view);
    expect(bar.scope).toBe('source');
    expect(bar.scopeLabels).toEqual(labels);
    expect(paneOf(view, 'source').filter).toEqual(CAMERA);
    expect(paneOf(view, 'comparison').filter).toBe(EMPTY_FILTER);
    expect(bar.contexts.map((context) => context.volume_id)).toEqual([1]);

    setScope('duplicates', 'comparison');
    await view.updateComplete;
    expect(bar.scope).toBe('comparison');
    expect(paneOf(view, 'source').filter).toBe(EMPTY_FILTER);
    expect(paneOf(view, 'comparison').filter).toEqual(CAMERA);
    expect(bar.contexts.map((context) => context.volume_id)).toEqual([2]);
  });

  it('chooses `both` for a choice without `initial`', async () => {
    const view = await mountView(toolConfig({ id: 'duplicates', filter: { applies: 'choice' } }));
    expect(filterBar(view).scope).toBe('both');
    expect(filterBar(view).scopeLabels).toEqual(DEFAULT_SCOPE_LABELS);
  });

  it('locks the bar and filters nothing when it applies to none, even with a stored filter', async () => {
    setFilter('cleanup', CAMERA);
    const view = await mountView(
      toolConfig({
        id: 'cleanup',
        filter: { applies: 'none', reason: 'Empty folders have no files to filter.' },
      }),
    );
    const bar = filterBar(view);
    expect(bar.disabled).toBe(true);
    expect(bar.disabledReason).toBe('Empty folders have no files to filter.');
    expect(bar.scope).toBeNull();
    expect(bar.contexts).toEqual([]);
    expect(text(bar)).toContain('Empty folders have no files to filter.');
    expect(paneOf(view, 'source').filter).toBe(EMPTY_FILTER);
    expect(paneOf(view, 'comparison').filter).toBe(EMPTY_FILTER);
  });

  it('writes the filter of the bar to the store of ITS tool, and the panes follow', async () => {
    const view = await mountView(toolConfig({ id: 'duplicates', filter: { applies: 'both' } }));
    emit(filterBar(view), 'filter-change', CAMERA);
    expect(filterFor('duplicates')).toEqual(CAMERA);
    expect(filterFor('browse')).toEqual(EMPTY_FILTER);
    await view.updateComplete;
    expect(paneOf(view, 'source').filter).toEqual(CAMERA);
    expect(paneOf(view, 'comparison').filter).toEqual(CAMERA);
    expect(filterBar(view).filter).toEqual(CAMERA);
  });

  it('writes the scope of the bar to the store of its tool', async () => {
    const view = await mountView(toolConfig({ id: 'duplicates', filter: { applies: 'choice' } }));
    emit(filterBar(view), 'scope-change', 'comparison');
    expect(scopeFor('duplicates')).toBe('comparison');
    await view.updateComplete;
    expect(filterBar(view).scope).toBe('comparison');
  });

  it('writes the hide or grey choice to the preference, and the panes follow', async () => {
    const view = await mountView();
    expect(filterBar(view).hideUnmatched).toBe(false);
    emit(filterBar(view), 'hide-unmatched-change', true);
    expect(hideUnmatchedFolders.value).toBe(true);
    await view.updateComplete;
    expect(filterBar(view).hideUnmatched).toBe(true);
    expect(paneOf(view, 'source').hideUnmatched).toBe(true);
    expect(paneOf(view, 'comparison').hideUnmatched).toBe(true);
  });

  describe('the hand-off', () => {
    it('shows the label of the selection the tool was opened with', async () => {
      setFilter('browse', { ...CAMERA, selection_id: 12 });
      setHandoff('browse', { label: '3 selected duplicates' });
      const view = await mountView();
      expect(filterBar(view).selectionLabel).toBe('3 selected duplicates');
      expect(text(filterBar(view))).toContain('3 selected duplicates');
    });

    it('has no label without a hand-off', async () => {
      const view = await mountView();
      expect(filterBar(view).selectionLabel).toBeNull();
    });

    it('is dropped with the chip: the selection, the label, and nothing else', async () => {
      setFilter('browse', { ...CAMERA, selection_id: 12 });
      setHandoff('browse', { label: '3 selected duplicates' });
      const view = await mountView();
      part<HTMLButtonElement>(
        filterBar(view),
        'button[aria-label="Remove selection filter"]',
      ).click();
      expect(filterFor('browse').selection_id).toBeNull();
      expect(filterFor('browse').cameras).toEqual(['Canon EOS R5']);
      expect(handoffFor('browse')).toBeNull();
      await settle(view);
      expect(filterBar(view).selectionLabel).toBeNull();
      expect(
        filterBar(view).querySelector('button[aria-label="Remove selection filter"]'),
      ).toBeNull();
      expect(paneOf(view, 'source').filter.selection_id).toBeNull();
    });
  });
});

describe('the bottom bar', () => {
  it('shows the counts the panes report', async () => {
    browseAt('photos/trip', 'backup/old');
    server.state.folders = (request) =>
      foldersAnswer(
        [],
        request.volume_id === 1
          ? { in_folder: 5, in_subfolders: 7 }
          : { in_folder: 1, in_subfolders: 2 },
      );
    const view = await mountView();
    expect(bottomBar(view).source.counts).toEqual({ in_folder: 5, in_subfolders: 7 });
    expect(bottomBar(view).comparison?.counts).toEqual({ in_folder: 1, in_subfolders: 2 });
    expect(text(segment(view, 'source'))).toContain('5 in folder');
    expect(text(segment(view, 'source'))).toContain('7 in subfolders');
    expect(text(segment(view, 'comparison'))).toContain('1 in folder');
  });

  it('follows a pane whose counts change, and shows the counting state for none', async () => {
    const view = await mountView();
    const source = paneOf(view, 'source');
    emit(source, 'pane-counts', { pane: 'comparison', counts: { in_folder: 9, in_subfolders: 0 } });
    await view.updateComplete;
    expect(bottomBar(view).comparison?.counts).toEqual({ in_folder: 9, in_subfolders: 0 });
    emit(source, 'pane-counts', { pane: 'comparison', counts: null });
    await view.updateComplete;
    expect(bottomBar(view).comparison?.counts).toBeNull();
  });

  it('sets both thumbnail scales from the source slider and has no right-pane slider', async () => {
    const view = await mountView();
    expect(segment(view, 'comparison').querySelector('input[type="range"]')).toBeNull();
    const slider = part<HTMLInputElement>(segment(view, 'source'), 'input[type="range"]');
    slider.value = '224';
    slider.dispatchEvent(new Event('input', { bubbles: true }));
    expect(gridSize.value.comparison).toBe(224);
    expect(gridSize.value.source).toBe(224);
    await view.updateComplete;
    expect(bottomBar(view).comparison?.size).toBe(224);
    expect(bottomBar(view).source.size).toBe(224);
  });

  it('gives the slider the bounds of the preference', async () => {
    const view = await mountView();
    expect(bottomBar(view).source).toMatchObject({
      sizeMin: GRID_SIZE.min,
      sizeMax: GRID_SIZE.max,
      sizeStep: GRID_SIZE.step,
    });
  });

  describe('selecting', () => {
    beforeEach(() => {
      browseAt('photos/trip', 'backup/old');
      server.state.folders = () => foldersAnswer([], { in_folder: 3 });
      server.state.total = 3;
      server.state.ids = (request) => (request.volume_id === 1 ? [1, 2, 3] : [11, 12]);
    });

    it('selects the whole listing of the pane and makes it the active pane', async () => {
      const view = await mountView();
      expect(activePane.value).toBe('source');
      part<HTMLButtonElement>(
        segment(view, 'comparison'),
        'button[aria-label="Select all"]',
      ).click();
      expect(activePane.value).toBe('comparison');
      await settle(view);
      expect(selection.comparison.toArray()).toEqual([11, 12]);
      expect(selection.source.count.value).toBe(0);
      expect(bottomBar(view).comparison?.selectedCount).toBe(2);
      expect(bottomBar(view).comparison?.active).toBe(true);
      expect(bottomBar(view).source.active).toBe(false);
    });

    it('clears the selection of the pane and makes it the active pane', async () => {
      const view = await mountView();
      selection.source.set([1]);
      selection.comparison.set([11, 12]);
      await settle(view);
      expect(bottomBar(view).comparison?.selectedCount).toBe(2);
      part<HTMLButtonElement>(
        segment(view, 'comparison'),
        'button[aria-label="Clear selection"]',
      ).click();
      expect(selection.comparison.count.value).toBe(0);
      expect(selection.source.toArray()).toEqual([1]);
      expect(activePane.value).toBe('comparison');
    });
  });
});

describe('the selection', () => {
  beforeEach(() => {
    browseAt('photos/trip', 'backup/old');
    server.state.ids = () => [1, 2, 3];
  });

  it('starts empty in both panes', async () => {
    selection.source.set([1]);
    selection.comparison.set([2]);
    await mountView();
    expect(selection.source.count.value).toBe(0);
    expect(selection.comparison.count.value).toBe(0);
  });

  it('answers Escape while the view is connected, and no more once it is gone', async () => {
    const view = await mountView();
    selection.source.set([1, 2]);
    press('Escape');
    expect(selection.source.count.value).toBe(0);
    view.remove();
    selection.source.set([1, 2]);
    press('Escape');
    expect(selection.source.count.value).toBe(2);
  });

  it('answers Cmd+A once for the active pane, however often the view is connected', async () => {
    const selectAll = {
      source: vi.spyOn(selection.source, 'selectAll'),
      comparison: vi.spyOn(selection.comparison, 'selectAll'),
    };
    const view = await mountView();
    press('a', { metaKey: true });
    expect(selectAll.source).toHaveBeenCalledTimes(1);
    expect(selectAll.comparison).not.toHaveBeenCalled();

    view.remove();
    press('a', { metaKey: true });
    expect(selectAll.source).toHaveBeenCalledTimes(1);

    document.body.append(view);
    await view.updateComplete;
    activePane.value = 'comparison';
    press('a', { metaKey: true });
    expect(selectAll.source).toHaveBeenCalledTimes(1);
    expect(selectAll.comparison).toHaveBeenCalledTimes(1);
  });
});

describe('the viewer', () => {
  const lastSource = (): ViewerSource => {
    const source = viewer.open.mock.calls.at(-1)?.[0];
    if (!source) throw new Error('The viewer was not opened');
    return source;
  };

  beforeEach(() => {
    browseAt('photos/trip', 'backup/old');
    server.state.total = 4;
    server.state.ids = () => [1, 2, 3, 4];
  });

  it('pages through the listing of the pane that holds the item, in its order', async () => {
    setSort('comparison', { key: 'size', order: 'desc' });
    const view = await mountView();
    const comparison = paneOf(view, 'comparison');
    emit(part(comparison, 'opm-file-grid'), 'item-open', { id: 2, index: 1 });

    expect(viewer.open).toHaveBeenCalledTimes(1);
    const source = lastSource();
    expect(source.startId).toBe(2);
    expect(source.startItem).toBe(comparison.itemAt(1));
    expect(source.startItem?.id).toBe(2);
    await expect(source.loadIds(new AbortController().signal)).resolves.toEqual([1, 2, 3, 4]);
    expect(server.calls.ids.at(-1)).toMatchObject({
      volume_id: 2,
      dir: 'old',
      sort: 'size',
      order: 'desc',
    });
  });

  it('uses the request and the sort of the source pane for an item of the source pane', async () => {
    setSort('comparison', { key: 'size', order: 'desc' });
    const view = await mountView();
    emit(part(paneOf(view, 'source'), 'opm-file-grid'), 'item-open', { id: 3, index: 2 });
    await lastSource().loadIds(new AbortController().signal);
    expect(server.calls.ids.at(-1)).toMatchObject({
      volume_id: 1,
      dir: 'trip',
      sort: 'name',
      order: 'asc',
    });
  });

  it('asks with the filter and the plan the pane shows', async () => {
    const plan = withPlan();
    plan.planIdSignal.value = 5;
    setFilter('duplicates', CAMERA);
    const view = await mountView(planConfig());
    emit(part(paneOf(view, 'comparison'), 'opm-file-grid'), 'item-open', { id: 1, index: 0 });
    await lastSource().loadIds(new AbortController().signal);
    expect(server.calls.ids.at(-1)).toMatchObject({
      filter: CAMERA,
      plan: { plan_id: 5, side: 'comparison' },
    });
  });

  it('ignores an item-open that does not come from a pane', async () => {
    const view = await mountView();
    emit(paneGrid(view), 'item-open', { id: 1, index: 0 });
    expect(viewer.open).not.toHaveBeenCalled();
  });
});

describe('the tool bar', () => {
  it('renders the controls of the tool again when a signal they read changes', async () => {
    const label = signal('one');
    const contexts: ToolContext[] = [];
    const view = await mountView(
      toolConfig({
        controls: (ctx) => {
          contexts.push(ctx);
          return html`<span data-test="mine">${label.value}</span>`;
        },
      }),
    );
    expect(text(part(toolBar(view), '[data-test="mine"]'))).toBe('one');
    label.value = 'two';
    await view.updateComplete;
    expect(text(part(toolBar(view), '[data-test="mine"]'))).toBe('two');
    expect(new Set(contexts).size).toBe(1);
    expect(contexts[0]?.host).toBe(view);
  });

  it('has no controls area for a tool without controls', async () => {
    const view = await mountView();
    expect(toolBar(view).querySelector('[data-test]')).toBeNull();
    expect(toolBar(view).querySelector('[role="status"]')).toBeNull();
  });

  describe('Include subfolders', () => {
    const switchIn = (view: Element) =>
      part<HTMLInputElement>(toolBar(view), 'input[role="switch"]');

    it('is only there for `recursive: toggle`', async () => {
      for (const recursive of [undefined, 'always'] as const) {
        const view = await mountView(toolConfig(recursive ? { recursive } : {}));
        expect(text(toolBar(view)), String(recursive)).not.toContain('Include subfolders');
        view.remove();
      }
      const view = await mountView(toolConfig({ recursive: 'toggle' }));
      expect(text(toolBar(view))).toContain('Include subfolders');
      expect(switchIn(view).checked).toBe(true);
    });

    it('is on until the user turns it off, and the context follows', async () => {
      let ctx: ToolContext | undefined;
      const view = await mountView(
        toolConfig({
          id: 'move',
          recursive: 'toggle',
          setup: (made) => {
            ctx = made;
            return undefined;
          },
        }),
      );
      expect(ctx?.recursive).toBe(true);
      switchIn(view).click();
      expect(recursiveFor('move')).toBe(false);
      expect(ctx?.recursive).toBe(false);
      await view.updateComplete;
      expect(switchIn(view).checked).toBe(false);
      switchIn(view).click();
      expect(recursiveFor('move')).toBe(true);
    });
  });
});

describe('the bases', () => {
  const config = () =>
    toolConfig({
      id: 'move',
      panes: {
        source: { ...plainPane('Source'), baseMode: 'current' },
        comparison: {
          ...plainPane('Destination'),
          baseMode: 'pinned',
          defaultBase: { from: 'source', label: 'Same as the source folder' },
        },
      },
    });

  beforeEach(() => {
    browseAt('photos/trip', 'backup/old');
  });

  it('gives each pane its base and the state of its default', async () => {
    const view = await mountView(config());
    expect(paneOf(view, 'source').base).toEqual({ volume: 'photos', dir: 'trip' });
    expect(paneOf(view, 'source').baseDefault).toBeNull();
    expect(paneOf(view, 'comparison').base).toEqual({ volume: 'photos', dir: 'trip' });
    expect(paneOf(view, 'comparison').baseDefault).toEqual({
      label: 'Same as the source folder',
      active: true,
    });
  });

  it('pins the base a pane asks for, and goes back to the default when it asks for none', async () => {
    const view = await mountView(config());
    const comparison = paneOf(view, 'comparison');
    emit(comparison, 'base-change', {
      pane: 'comparison',
      base: { volume: 'backup', dir: 'old' },
    });
    expect(pinnedBase('move', 'comparison')).toEqual({ volume: 'backup', dir: 'old' });
    await view.updateComplete;
    expect(comparison.base).toEqual({ volume: 'backup', dir: 'old' });
    expect(comparison.baseDefault).toEqual({ label: 'Same as the source folder', active: false });

    emit(comparison, 'base-change', { pane: 'comparison', base: null });
    expect(pinnedBase('move', 'comparison')).toBeNull();
    await view.updateComplete;
    expect(comparison.base).toEqual({ volume: 'photos', dir: 'trip' });
    expect(comparison.baseDefault?.active).toBe(true);
  });

  it('ignores a base for a pane that does not pin one', async () => {
    const view = await mountView(config());
    emit(paneOf(view, 'source'), 'base-change', {
      pane: 'source',
      base: { volume: 'backup', dir: 'old' },
    });
    expect(pinnedBase('move', 'source')).toBeNull();
  });

  it('keeps the object it gave a pane while nothing about it changed', async () => {
    const view = await mountView(config());
    const before = paneOf(view, 'comparison').baseDefault;
    setFilter('move', CAMERA);
    await view.updateComplete;
    expect(paneOf(view, 'comparison').baseDefault).toBe(before);
  });
});

describe('a tool with a plan', () => {
  it('shows the status of the plan in the tool bar and nothing for a tool without one', async () => {
    const plain = await mountView();
    expect(plain.querySelector('header [role="status"]')).toBeNull();
    plain.remove();

    const plan = withPlan();
    const view = await mountView(planConfig());
    expect(text(part(toolBar(view), '[role="status"]'))).toBe('');
    plan.statusSignal.value = 'building';
    await view.updateComplete;
    expect(text(part(toolBar(view), '[role="status"]'))).toBe('Matching...');
    plan.statusSignal.value = 'ready';
    await view.updateComplete;
    expect(text(part(toolBar(view), '[role="status"]'))).toBe('');
  });

  it('keeps the plan status in the controls row through status changes', async () => {
    const plan = withPlan();
    const view = await mountView(planConfig({ summary: 'Match things.' }));
    const row = toolBar(view);
    expect(row.classList.contains('min-h-10')).toBe(true);
    expect(row.querySelector('h2')).toBeNull();

    for (const status of ['building', 'executing', 'error', 'ready', 'idle'] as const) {
      plan.statusSignal.value = status;
      await view.updateComplete;
      expect(toolBar(view), status).toBe(row);
      expect(row.contains(part(toolBar(view), '[role="status"]')), status).toBe(true);
    }
  });

  it('shows a failed plan with a Retry that asks the plan to try again', async () => {
    const plan = withPlan();
    const view = await mountView(planConfig());
    plan.errorSignal.value = 'The server is busy.';
    plan.statusSignal.value = 'error';
    await view.updateComplete;
    const alert = part(toolBar(view), '[role="alert"]');
    expect(text(alert)).toContain('The server is busy.');
    const retry = [...alert.querySelectorAll('button')].find((button) => text(button) === 'Retry');
    retry?.click();
    expect(plan.retry).toHaveBeenCalledTimes(1);
  });

  it('gives each pane the reference of its own side, and none before a plan exists', async () => {
    const plan = withPlan();
    const view = await mountView(planConfig());
    expect(paneOf(view, 'source').plan).toBeNull();
    expect(paneOf(view, 'comparison').plan).toBeNull();

    plan.planIdSignal.value = 5;
    await view.updateComplete;
    expect(paneOf(view, 'source').plan).toEqual({ plan_id: 5, side: 'source' });
    expect(paneOf(view, 'comparison').plan).toEqual({ plan_id: 5, side: 'comparison' });

    plan.planIdSignal.value = null;
    await view.updateComplete;
    expect(paneOf(view, 'source').plan).toBeNull();
  });

  it('keeps the object it gave a pane until the plan changes, so the pane does not refetch', async () => {
    const plan = withPlan();
    plan.planIdSignal.value = 5;
    const view = await mountView(planConfig());
    const source = paneOf(view, 'source');
    const before = source.plan;
    expect(before).toEqual({ plan_id: 5, side: 'source' });

    plan.statusSignal.value = 'ready';
    plan.statsSignal.value = { duplicates: 3 };
    await view.updateComplete;
    expect(source.plan).toBe(before);

    plan.planIdSignal.value = 6;
    await view.updateComplete;
    expect(source.plan).not.toBe(before);
    expect(source.plan).toEqual({ plan_id: 6, side: 'source' });
  });

  it('gives a pane without a plan side no plan', async () => {
    const plan = withPlan();
    plan.planIdSignal.value = 5;
    const view = await mountView(
      planConfig({
        panes: {
          source: { ...plainPane('Source'), planSide: 'source' },
          comparison: plainPane('Comparison'),
        },
      }),
    );
    expect(paneOf(view, 'source').plan).toEqual({ plan_id: 5, side: 'source' });
    expect(paneOf(view, 'comparison').plan).toBeNull();
  });

  it('hands a plan the server forgot to the plan binding', async () => {
    const plan = withPlan();
    const view = await mountView(planConfig());
    emit(paneOf(view, 'source'), 'plan-stale', {
      pane: 'source',
      plan: { plan_id: 5, side: 'source' },
    });
    expect(plan.stale).toHaveBeenCalledTimes(1);
    expect(plan.stale).toHaveBeenCalledWith({ plan_id: 5, side: 'source' });
  });
});

describe('the actions', () => {
  it('puts them in the bottom bar and runs one with the context of the view', async () => {
    const run = vi.fn<(ctx: ToolContext) => Promise<void>>(async () => {});
    const view = await mountView(
      toolConfig({
        actions: () => ({ options: [{ id: 'go', label: 'Go', run }], defaultId: 'go' }),
      }),
    );
    const go = [...bottomBar(view).querySelectorAll('button')].find((button) =>
      text(button).startsWith('Go'),
    );
    go?.click();
    expect(run).toHaveBeenCalledTimes(1);
    expect(run.mock.calls[0]?.[0].host).toBe(view);
  });

  it('shows what they read from signals, and renders again when it changes', async () => {
    const count = signal(3);
    const view = await mountView(
      toolConfig({
        actions: () => ({
          options: [{ id: 'go', label: 'Go', run: async () => {} }],
          defaultId: 'go',
          count: () => count.value,
        }),
      }),
    );
    expect(text(bottomBar(view))).toContain('Go 3');
    count.value = 5;
    await settle(view);
    expect(text(bottomBar(view))).toContain('Go 5');
  });

  it('shows none for a tool without actions', async () => {
    const view = await mountView(toolConfig({ actions: () => null }));
    expect(bottomBar(view).actions).toBeUndefined();
    expect(bottomBar(view).querySelector('.btn-primary')).toBeNull();
  });
});

describe('its life', () => {
  it('renders nothing without a config, and starts when it gets one', async () => {
    const view = await mount('opm-tool-view');
    expect(view.children).toHaveLength(0);
    view.config = toolConfig();
    await view.updateComplete;
    expect(view.querySelectorAll('opm-pane')).toHaveLength(2);
    view.config = undefined;
    await view.updateComplete;
    expect(view.children).toHaveLength(0);
  });

  it('runs `setup` once for a connection, with the context of this view, and cleans up after it', async () => {
    const cleanup = vi.fn();
    const setup = vi.fn<(ctx: ToolContext) => (() => void) | undefined>(() => cleanup);
    const config = toolConfig({ setup });
    const view = await mountView(config);
    expect(setup).toHaveBeenCalledTimes(1);
    expect(setup.mock.calls[0]?.[0].config).toBe(config);
    expect(setup.mock.calls[0]?.[0].host).toBe(view);

    setFilter('browse', CAMERA);
    await settle(view);
    expect(setup).toHaveBeenCalledTimes(1);
    expect(cleanup).not.toHaveBeenCalled();

    view.remove();
    expect(cleanup).toHaveBeenCalledTimes(1);
  });

  it('accepts a `setup` without a cleanup', async () => {
    const view = await mountView(toolConfig({ setup: () => undefined }));
    expect(view.querySelectorAll('opm-pane')).toHaveLength(2);
    view.remove();
  });

  it('disposes the plan binding when it disconnects', async () => {
    const plan = withPlan();
    const view = await mountView(planConfig());
    expect(plan.dispose).not.toHaveBeenCalled();
    view.remove();
    expect(plan.dispose).toHaveBeenCalledTimes(1);
  });

  it('starts again when it is connected again, and starts with empty selections', async () => {
    const cleanup = vi.fn();
    const setup = vi.fn<(ctx: ToolContext) => (() => void) | undefined>(() => cleanup);
    browseAt('photos/trip', 'backup/old');
    server.state.folders = () => foldersAnswer([], { in_folder: 3 });
    const view = await mountView(toolConfig({ setup }));
    view.remove();
    selection.source.set([1]);
    document.body.append(view);
    await settle(view);
    expect(setup).toHaveBeenCalledTimes(2);
    expect(cleanup).toHaveBeenCalledTimes(1);
    expect(selection.source.count.value).toBe(0);
    expect(view.querySelectorAll('opm-pane')).toHaveLength(2);
    expect(bottomBar(view).source.counts).toEqual({ in_folder: 3, in_subfolders: 0 });
  });

  it('starts the config it has when it is connected again after a change', async () => {
    const view = await mountView(toolConfig({ title: 'First' }));
    view.remove();
    view.config = toolConfig({ title: 'Second' });
    document.body.append(view);
    await view.updateComplete;
    expect(view.querySelector('opm-modal')?.heading).toBe('Second');
  });

  describe('given another config while connected', () => {
    it('stops the old tool and starts the new one', async () => {
      const plans: FakePlan[] = [];
      seam.make = () => {
        const plan = new FakePlan();
        plans.push(plan);
        return plan;
      };
      const cleanupOld = vi.fn();
      const setupNew = vi.fn<(ctx: ToolContext) => (() => void) | undefined>();
      const view = await mountView(
        planConfig({ title: 'Old', setup: () => cleanupOld, summary: 'The old one.' }),
      );
      selection.source.set([1]);

      view.config = toolConfig({
        id: 'move',
        title: 'New',
        panes: { source: plainPane('Left'), comparison: plainPane('Right') },
        setup: setupNew,
      });
      await view.updateComplete;

      expect(cleanupOld).toHaveBeenCalledTimes(1);
      expect(plans).toHaveLength(2);
      expect(plans[0]?.dispose).toHaveBeenCalledTimes(1);
      expect(plans[1]?.dispose).not.toHaveBeenCalled();
      expect(setupNew).toHaveBeenCalledTimes(1);
      expect(setupNew.mock.calls[0]?.[0].tool).toBe('move');
      expect(selection.source.count.value).toBe(0);
      expect(view.querySelector('opm-modal')?.heading).toBe('New');
      expect(PANES.map((id) => paneOf(view, id).heading)).toEqual(['Left', 'Right']);
    });

    it('keeps the counts of the panes for the new tool', async () => {
      browseAt('photos/trip', 'backup/old');
      server.state.folders = () => foldersAnswer([], { in_folder: 4, in_subfolders: 1 });
      const view = await mountView();
      view.config = toolConfig({ id: 'move', title: 'New' });
      await settle(view);
      expect(bottomBar(view).source.counts).toEqual({ in_folder: 4, in_subfolders: 1 });
      expect(bottomBar(view).comparison?.counts).toEqual({ in_folder: 4, in_subfolders: 1 });
    });

    it('does nothing for the very same config', async () => {
      const setup = vi.fn<(ctx: ToolContext) => (() => void) | undefined>();
      const config = toolConfig({ setup });
      const view = await mountView(config);
      view.config = config;
      await view.updateComplete;
      expect(setup).toHaveBeenCalledTimes(1);
    });
  });
});
