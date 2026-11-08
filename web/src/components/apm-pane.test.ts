import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Filter } from '@/api/generated/Filter';
import type { PlanRef } from '@/api/generated/PlanRef';
import { events } from '@/api/sse';
import type { ApmFileGrid } from '@/components/apm-file-grid';
import type { ApmFolderList } from '@/components/apm-folder-list';
import type {
  ApmPane,
  BaseChangeDetail,
  PaneCountsDetail,
  PlanStaleDetail,
} from '@/components/apm-pane';
import '@/components/apm-pane';
import type { ApmSelect } from '@/components/apm-select';
import type { ItemOpenDetail } from '@/controllers/selection-controller';
import {
  locations,
  type PaneId,
  type PaneLocation,
  ROOT_LOCATION,
  setLocation,
  syncFromLocation,
} from '@/stores/nav';
import { INVALIDATE_DEBOUNCE_MS } from '@/stores/paged-items';
import { GRID_SIZE, setGridSize, setSort, sort } from '@/stores/prefs';
import { activePane, resetSelectionForTests, selection } from '@/stores/selection';
import { clearToasts, toasts } from '@/stores/toasts';
import { loadVolumes } from '@/stores/volumes';
import {
  type BrowseServer,
  foldersAnswer,
  type ServerFailure,
  stubBrowseServer,
  VOLUME_BACKUP,
  VOLUME_PHOTOS,
} from '@/test/browse-server';
import { folderEntry } from '@/test/folder-entry';
import { mount } from '@/test/helpers';
import { EMPTY_FILTER } from '@/util/filter';

const TRIP: PaneLocation = { volume: 'photos', dir: 'trip' };
const NOT_FOUND: ServerFailure = { status: 404, code: 'folder_not_found', message: 'gone' };
const PLAN_GONE: ServerFailure = { status: 404, code: 'plan_not_found', message: 'gone' };

let server: BrowseServer;
const cleanups: (() => void)[] = [];

beforeEach(async () => {
  window.history.replaceState(null, '', '/');
  syncFromLocation();
  server = stubBrowseServer();
  await loadVolumes();
});

afterEach(() => {
  // The panes go first: a pane that is still mounted would fetch again for the preferences reset.
  for (const pane of document.querySelectorAll('apm-pane')) pane.remove();
  for (const cleanup of cleanups.splice(0)) cleanup();
  events.stop();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  clearToasts();
  resetSelectionForTests();
  for (const pane of ['source', 'comparison'] as const) {
    setSort(pane, { key: 'name', order: 'asc' });
    setGridSize(pane, GRID_SIZE.default);
  }
});

type LitElement = Element & { updateComplete: Promise<unknown> };

const litElementsIn = (root: Element): LitElement[] =>
  [root, ...root.querySelectorAll('*')].filter(
    (element): element is LitElement => 'updateComplete' in element,
  );

/** Lets the stubbed server answer and the pane and everything in it render what came back. */
async function settle(pane: ApmPane, rounds = 6): Promise<void> {
  for (let round = 0; round < rounds; round += 1) {
    if (vi.isFakeTimers()) await vi.advanceTimersByTimeAsync(0);
    else await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await Promise.all(litElementsIn(pane).map((element) => element.updateComplete));
  }
}

async function mountPane(props: Partial<ApmPane> = {}): Promise<ApmPane> {
  const pane = await mount('apm-pane', { pane: 'source', ...props });
  await settle(pane);
  return pane;
}

/** Collects the `detail` of every `type` event that reaches the page, from now on. */
function record<T>(type: string): T[] {
  const seen: T[] = [];
  const listener = (event: Event) => seen.push((event as CustomEvent<T>).detail);
  document.body.addEventListener(type, listener);
  cleanups.push(() => document.body.removeEventListener(type, listener));
  return seen;
}

function part<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`No element matches "${selector}"`);
  return found;
}

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

const body = (pane: ApmPane) => part(pane, '[data-pane-body]');
const folderList = (pane: ApmPane) => part<ApmFolderList>(pane, 'apm-folder-list');
const grid = (pane: ApmPane) => part<ApmFileGrid>(pane, 'apm-file-grid');
const rowElements = (pane: ApmPane) => [
  ...pane.querySelectorAll<HTMLAnchorElement>('apm-folder-list a[data-name]'),
];
const rowNames = (pane: ApmPane) => rowElements(pane).map((row) => row.dataset.name);
const row = (pane: ApmPane, name: string) =>
  part<HTMLAnchorElement>(pane, `a[data-name="${name}"]`);
const crumbElements = (pane: ApmPane) => [
  ...pane.querySelectorAll<HTMLAnchorElement>('[data-crumbs] a'),
];
const crumbLabels = (pane: ApmPane) => crumbElements(pane).map(text);
const alerts = (pane: ApmPane) => [...pane.querySelectorAll('[role="alert"]')].map(text);
const upButton = (pane: ApmPane) =>
  part<HTMLButtonElement>(pane, 'button[aria-label="Up one folder"]');
const baseRow = (pane: ApmPane) => pane.querySelector('[role="group"][aria-label="Base folder"]');
const buttonNamed = (root: ParentNode, name: string) =>
  [...root.querySelectorAll('button')].find((button) => text(button) === name);

/**
 * Clicks a link the way a user does and reports whether the page took the click over. The
 * browser's own navigation is cancelled after the page had its say, so a click it left alone
 * does not make happy-dom load the URL.
 */
function clickLink(target: Element, init: MouseEventInit = {}): { claimed: boolean } {
  const result = { claimed: false };
  const observe = (event: Event) => {
    result.claimed = event.defaultPrevented;
    event.preventDefault();
  };
  document.addEventListener('click', observe, { once: true });
  target.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, ...init }));
  document.removeEventListener('click', observe);
  return result;
}

function chooseSort(pane: ApmPane, value: string): void {
  const select = part<HTMLSelectElement>(pane, 'apm-select select');
  select.value = value;
  select.dispatchEvent(new Event('change', { bubbles: true }));
}

function goTo(pane: PaneId, location: PaneLocation): void {
  setLocation(pane, location);
}

/** The server knows these subfolders of every directory it is asked about. */
function serveFolders(...entries: Parameters<typeof folderEntry>[0][]): void {
  server.state.folders = () => foldersAnswer(entries.map((entry) => folderEntry(entry)));
}

describe('the listing it shows', () => {
  it('asks for the folders and the files of the pane that it is, with its filter and plan', async () => {
    goTo('source', TRIP);
    const filter: Filter = { ...EMPTY_FILTER, cameras: ['Canon EOS R5'] };
    const plan: PlanRef = { plan_id: 3, side: 'source' };
    server.state.total = 3;

    const pane = await mountPane({ filter, plan });

    const request = { volume_id: 1, dir: 'trip', filter, plan };
    expect(pane.browseRequest).toEqual(request);
    expect(server.calls.folders).toEqual([request]);
    expect(server.calls.items).toHaveLength(1);
    expect(server.calls.items[0]).toMatchObject({ ...request, sort: 'name', order: 'asc' });
    expect(grid(pane).request).toEqual(request);
  });

  it('uses the location of its own pane and no other', async () => {
    goTo('source', TRIP);
    goTo('comparison', { volume: 'backup', dir: 'old' });

    await mountPane({ pane: 'comparison' });

    expect(server.calls.folders).toEqual([
      { volume_id: 2, dir: 'old', filter: EMPTY_FILTER, plan: null },
    ]);
  });

  it('starts over when the filter changes: it is another listing', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();

    pane.filter = { ...EMPTY_FILTER, ratings: [4, 5] };
    await settle(pane);

    expect(server.calls.folders.map((request) => request.filter.ratings)).toEqual([[], [4, 5]]);
  });

  it('lists the volumes at the volumes root and has no files to show there', async () => {
    serveFolders(
      { name: 'photos', path: '', count: 40, total: 40 },
      { name: 'backup', path: '', volume_id: 2, count: 12, total: 12 },
    );

    const pane = await mountPane();

    expect(server.calls.folders).toEqual([
      { volume_id: null, dir: '', filter: EMPTY_FILTER, plan: null },
    ]);
    expect(rowNames(pane)).toEqual(['photos', 'backup']);
    expect(pane.querySelector('apm-file-grid')).toBeNull();
    expect(server.calls.items).toEqual([]);
    expect(crumbLabels(pane)).toEqual(['Volumes']);
    expect(upButton(pane).disabled).toBe(true);
  });

  it('says so when there is no volume at all', async () => {
    const pane = await mountPane();

    expect(text(part(pane, 'p[role="status"]'))).toBe('No volumes are available');
    expect(rowNames(pane)).toEqual([]);
  });

  it('reports a volume that is not mounted, with a way out', async () => {
    goTo('source', { volume: 'nas', dir: 'trip' });

    const pane = await mountPane();

    expect(alerts(pane)).toEqual(['Volume nas was not found Show all volumes']);
    expect(server.calls.folders).toEqual([]);
    expect(pane.browseRequest).toBeNull();
    expect(pane.querySelector('apm-folder-list, apm-file-grid')).toBeNull();
    expect(crumbLabels(pane)).toEqual(['Volumes', 'nas', 'trip']);

    const result = clickLink(part(pane, '[role="alert"] a'));
    await settle(pane);

    expect(result.claimed).toBe(true);
    expect(locations.source.value).toEqual(ROOT_LOCATION);
    expect(alerts(pane)).toEqual([]);
    expect(server.calls.folders).toHaveLength(1);
  });

  it('warns about an offline volume and still shows what was indexed', async () => {
    server.state.volumes = [{ ...VOLUME_PHOTOS, online: false }, VOLUME_BACKUP];
    await loadVolumes();
    goTo('source', { volume: 'photos', dir: '' });
    serveFolders({ name: 'trip' });

    const pane = await mountPane();

    expect(alerts(pane)).toEqual(['Volume photos is offline: showing what was last indexed']);
    expect(rowNames(pane)).toEqual(['trip']);
  });

  it('follows the volumes when they change', async () => {
    goTo('source', { volume: 'nas', dir: '' });
    const pane = await mountPane();
    expect(pane.browseRequest).toBeNull();

    server.state.volumes = [VOLUME_PHOTOS, { id: 3, name: 'nas', writable: true, online: true }];
    await loadVolumes();
    await settle(pane);

    expect(pane.browseRequest).toMatchObject({ volume_id: 3, dir: '' });
    expect(server.calls.folders).toHaveLength(1);
  });

  describe('while it is disabled', () => {
    it('stays on screen with its reason and asks the server for nothing', async () => {
      goTo('source', TRIP);
      const counts = record<PaneCountsDetail>('pane-counts');

      const pane = await mountPane({
        disabled: true,
        disabledReason: 'Choose a plan first',
        heading: 'Comparison',
      });

      expect(pane.browseRequest).toBeNull();
      expect(alerts(pane)).toEqual(['Choose a plan first']);
      expect(text(pane.querySelector('header'))).toBe('Comparison');
      expect(pane.querySelector('apm-folder-list, apm-file-grid, [data-crumbs]')).toBeNull();
      expect(server.calls.folders).toEqual([]);
      expect(server.calls.items).toEqual([]);
      expect(counts).toEqual([{ pane: 'source', counts: null }]);
    });

    it('comes to life when it is enabled', async () => {
      goTo('source', TRIP);
      const pane = await mountPane({ disabled: true, disabledReason: 'Choose a plan first' });

      pane.disabled = false;
      await settle(pane);

      expect(alerts(pane)).toEqual([]);
      expect(server.calls.folders).toHaveLength(1);
      expect(pane.browseRequest).toMatchObject({ volume_id: 1, dir: 'trip' });
    });

    it('drops what it showed and stops asking for it', async () => {
      goTo('source', TRIP);
      serveFolders({ name: 'raw', path: 'trip/raw' });
      const pane = await mountPane();
      expect(rowNames(pane)).toEqual(['raw']);

      pane.disabled = true;
      await settle(pane);

      expect(rowNames(pane)).toEqual([]);
      server.calls.folders.length = 0;
      events.emit({ type: 'fs_change', volume_id: 1, dirs: ['trip'] });
      await settle(pane);
      expect(server.calls.folders).toEqual([]);
    });
  });
});

describe('the subfolders', () => {
  it('show skeleton rows until the first answer', async () => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    server.hold('folders');

    const pane = await mountPane();

    expect(folderList(pane).loading).toBe(true);
    expect(text(folderList(pane))).toBe('Loading folders');

    server.release('folders');
    await settle(pane);

    expect(folderList(pane).loading).toBe(false);
    expect(rowNames(pane)).toEqual(['raw']);
  });

  it('give each folder a real link that keeps the other pane where it is', async () => {
    goTo('source', TRIP);
    goTo('comparison', { volume: 'backup', dir: 'old' });
    serveFolders({ name: 'raw', path: 'trip/raw' });

    const pane = await mountPane();

    expect(row(pane, 'raw').getAttribute('href')).toBe('/?src=photos/trip/raw&cmp=backup/old');
  });

  it('keep their links current when the other pane moves', async () => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    const pane = await mountPane();

    goTo('comparison', { volume: 'backup', dir: 'new' });
    await settle(pane);

    expect(row(pane, 'raw').getAttribute('href')).toBe('/?src=photos/trip/raw&cmp=backup/new');
  });

  it('open in this pane only, as one history entry', async () => {
    goTo('source', TRIP);
    goTo('comparison', { volume: 'backup', dir: 'old' });
    serveFolders({ name: 'raw', path: 'trip/raw' });
    const pane = await mountPane();
    const push = vi.spyOn(window.history, 'pushState');

    const result = clickLink(row(pane, 'raw'));
    await settle(pane);

    expect(result.claimed).toBe(true);
    expect(push).toHaveBeenCalledTimes(1);
    expect(locations.source.value).toEqual({ volume: 'photos', dir: 'trip/raw' });
    expect(locations.comparison.value).toEqual({ volume: 'backup', dir: 'old' });
    expect(window.location.search).toBe('?src=photos/trip/raw&cmp=backup/old');
    expect(server.calls.folders.at(-1)).toMatchObject({ volume_id: 1, dir: 'trip/raw' });
  });

  it('open a volume from the volumes root', async () => {
    serveFolders({ name: 'backup', path: '', volume_id: 2, count: 12, total: 12 });
    const pane = await mountPane();
    expect(row(pane, 'backup').getAttribute('href')).toBe('/?src=backup');

    clickLink(row(pane, 'backup'));
    await settle(pane);

    expect(locations.source.value).toEqual({ volume: 'backup', dir: '' });
    expect(server.calls.folders.at(-1)).toMatchObject({ volume_id: 2, dir: '' });
  });

  it.each([
    ['a middle click', { button: 1 }],
    ['a ctrl-click', { ctrlKey: true }],
    ['a cmd-click', { metaKey: true }],
    ['a shift-click', { shiftKey: true }],
  ])('leave %s to the browser', async (_name, init) => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    const pane = await mountPane();

    const result = clickLink(row(pane, 'raw'), init);

    expect(result.claimed).toBe(false);
    expect(locations.source.value).toEqual(TRIP);
  });

  it('grey a folder that the filters empty, and hide it when asked to', async () => {
    goTo('source', TRIP);
    serveFolders(
      { name: 'raw', path: 'trip/raw' },
      { name: 'jpg', path: 'trip/jpg', count: 0, total: 5 },
    );
    const pane = await mountPane();

    expect(rowNames(pane)).toEqual(['raw', 'jpg']);
    expect(row(pane, 'jpg').className).toContain('opacity-50');
    expect(row(pane, 'raw').className).not.toContain('opacity-50');

    pane.hideUnmatched = true;
    await settle(pane);

    expect(rowNames(pane)).toEqual(['raw']);
    expect(folderList(pane).hideUnmatched).toBe(true);
  });

  it('show how many files match the plan only when asked to', async () => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw', matches: 2 });
    const pane = await mountPane({ plan: { plan_id: 1, side: 'source' } });
    expect(text(row(pane, 'raw'))).not.toContain('2 matches');

    pane.showMatches = true;
    await settle(pane);

    expect(folderList(pane).showMatches).toBe(true);
    expect(text(row(pane, 'raw'))).toContain('2 matches');
  });

  it('report a failure with a retry that works', async () => {
    goTo('source', TRIP);
    server.state.folders = () => ({ status: 500, code: 'internal', message: 'disk on fire' });
    const pane = await mountPane();

    expect(alerts(pane)).toEqual(['Could not load the folders: disk on fire Retry']);

    serveFolders({ name: 'raw', path: 'trip/raw' });
    part<HTMLButtonElement>(pane, '[role="alert"] button').click();
    await settle(pane);

    expect(alerts(pane)).toEqual([]);
    expect(rowNames(pane)).toEqual(['raw']);
  });

  it('keep their rows while they are fetched again, and say that they are busy', async () => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    const pane = await mountPane();
    expect(body(pane).hasAttribute('aria-busy')).toBe(false);

    vi.useFakeTimers();
    server.hold('folders');
    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['trip/raw'] });
    await vi.advanceTimersByTimeAsync(INVALIDATE_DEBOUNCE_MS);
    await settle(pane);

    expect(rowNames(pane)).toEqual(['raw']);
    expect(body(pane).getAttribute('aria-busy')).toBe('true');

    serveFolders({ name: 'raw', path: 'trip/raw' }, { name: 'new', path: 'trip/new' });
    server.release('folders');
    await settle(pane);

    expect(rowNames(pane)).toEqual(['raw', 'new']);
    expect(body(pane).hasAttribute('aria-busy')).toBe(false);
  });

  it('are separated from the files by a divider, when there are both', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();
    expect(pane.querySelector('[role="separator"]')).toBeNull();

    serveFolders({ name: 'raw', path: 'trip/raw' });
    pane.disabled = true;
    await settle(pane);
    pane.disabled = false;
    await settle(pane);

    expect(pane.querySelector('[role="separator"]')).not.toBeNull();
  });
});

describe('the breadcrumbs and the up button', () => {
  it('show the path from the volumes root and mark the current folder', async () => {
    goTo('source', { volume: 'photos', dir: 'trip/2024' });
    goTo('comparison', { volume: 'backup', dir: 'old' });

    const pane = await mountPane();

    expect(crumbLabels(pane)).toEqual(['Volumes', 'photos', 'trip', '2024']);
    expect(crumbElements(pane).map((crumb) => crumb.getAttribute('href'))).toEqual([
      '/?cmp=backup/old',
      '/?src=photos&cmp=backup/old',
      '/?src=photos/trip&cmp=backup/old',
      '/?src=photos/trip/2024&cmp=backup/old',
    ]);
    expect(crumbElements(pane).map((crumb) => crumb.getAttribute('aria-current'))).toEqual([
      null,
      null,
      null,
      'page',
    ]);
    expect(part(pane, '[data-crumbs]').getAttribute('aria-label')).toBe('Source folder path');
    expect(part(pane, '[data-crumbs]').getAttribute('title')).toBe('photos/trip/2024');
  });

  it('go to the crumb that is clicked, in this pane only', async () => {
    goTo('source', { volume: 'photos', dir: 'trip/2024' });
    goTo('comparison', { volume: 'backup', dir: 'old' });
    const pane = await mountPane();

    const result = clickLink(part(pane, '[data-crumbs] a[title="trip"]'));
    await settle(pane);

    expect(result.claimed).toBe(true);
    expect(locations.source.value).toEqual({ volume: 'photos', dir: 'trip' });
    expect(locations.comparison.value).toEqual({ volume: 'backup', dir: 'old' });
    expect(crumbLabels(pane)).toEqual(['Volumes', 'photos', 'trip']);
  });

  it('leave a ctrl-click on a crumb to the browser', async () => {
    goTo('source', { volume: 'photos', dir: 'trip/2024' });
    const pane = await mountPane();

    const result = clickLink(part(pane, '[data-crumbs] a[title="trip"]'), { ctrlKey: true });

    expect(result.claimed).toBe(false);
    expect(locations.source.value).toEqual({ volume: 'photos', dir: 'trip/2024' });
  });

  it('name the pane in the label of the path', async () => {
    const pane = await mountPane({ pane: 'comparison' });

    expect(part(pane, '[data-crumbs]').getAttribute('aria-label')).toBe('Comparison folder path');
  });

  it('go up one folder, then to the volume, then to the volumes root', async () => {
    goTo('source', { volume: 'photos', dir: 'trip/2024' });
    const pane = await mountPane();
    const up = async () => {
      upButton(pane).click();
      await settle(pane);
      return locations.source.value;
    };

    expect(await up()).toEqual({ volume: 'photos', dir: 'trip' });
    expect(await up()).toEqual({ volume: 'photos', dir: '' });
    expect(await up()).toEqual(ROOT_LOCATION);
    expect(upButton(pane).disabled).toBe(true);
  });

  it('keep the other pane where it is when going up', async () => {
    goTo('source', TRIP);
    goTo('comparison', { volume: 'backup', dir: 'old' });
    const pane = await mountPane();

    upButton(pane).click();

    expect(locations.comparison.value).toEqual({ volume: 'backup', dir: 'old' });
  });

  it('show the path of an unknown volume too, so that the user can leave it', async () => {
    goTo('source', { volume: 'nas', dir: 'trip' });
    const pane = await mountPane();

    upButton(pane).click();
    await settle(pane);

    expect(locations.source.value).toEqual({ volume: 'nas', dir: '' });
  });
});

describe('the sort controls', () => {
  it('offer the sort keys and show the ones of this pane', async () => {
    setSort('comparison', { key: 'size', order: 'desc' });

    const pane = await mountPane({ pane: 'comparison', heading: 'Backup' });

    const select = part<ApmSelect>(pane, 'apm-select');
    expect(select.options.map((option) => option.label)).toEqual(['Name', 'Date', 'Size']);
    expect(select.value).toBe('size');
    expect(select.label).toBe('Sort by (Backup)');
    const toggle = part(pane, 'button[aria-label^="Sort order"]');
    expect(toggle.getAttribute('aria-label')).toBe(
      'Sort order (Backup): descending. Switch to ascending',
    );
    expect(toggle.getAttribute('title')).toBe(toggle.getAttribute('aria-label'));
  });

  it('are named after the pane when it has no heading', async () => {
    const pane = await mountPane();

    expect(part<ApmSelect>(pane, 'apm-select').label).toBe('Sort by (Source)');
    expect(part(pane, 'button[aria-label^="Sort order"]').getAttribute('aria-label')).toBe(
      'Sort order (Source): ascending. Switch to descending',
    );
  });

  it('write the key to the preferences of this pane and sort the files with it', async () => {
    goTo('source', TRIP);
    server.state.total = 3;
    const pane = await mountPane();

    chooseSort(pane, 'date');
    await settle(pane);

    expect(sort.value).toEqual({
      source: { key: 'date', order: 'asc' },
      comparison: { key: 'name', order: 'asc' },
    });
    expect(grid(pane).sort).toBe('date');
    expect(server.calls.items.at(-1)).toMatchObject({ sort: 'date', order: 'asc' });
  });

  it('flip the order with the toggle', async () => {
    goTo('source', TRIP);
    server.state.total = 3;
    const pane = await mountPane();
    const toggle = () => part<HTMLButtonElement>(pane, 'button[aria-label^="Sort order"]');
    expect(toggle().querySelector('apm-icon')?.getAttribute('name')).toBe('arrow-up');

    toggle().click();
    await settle(pane);

    expect(sort.value.source).toEqual({ key: 'name', order: 'desc' });
    expect(grid(pane).order).toBe('desc');
    expect(server.calls.items.at(-1)).toMatchObject({ sort: 'name', order: 'desc' });
    expect(toggle().querySelector('apm-icon')?.getAttribute('name')).toBe('arrow-down');
    expect(toggle().getAttribute('aria-label')).toBe(
      'Sort order (Source): descending. Switch to ascending',
    );
  });

  it('keep the choice inside the pane: nobody above hears select-change', async () => {
    const heard = vi.fn();
    document.body.addEventListener('select-change', heard);
    cleanups.push(() => document.body.removeEventListener('select-change', heard));
    const pane = await mountPane();

    chooseSort(pane, 'size');

    expect(heard).not.toHaveBeenCalled();
    expect(sort.value.source.key).toBe('size');
  });

  it('ignore a key that it does not know', async () => {
    const pane = await mountPane();
    const select = part<ApmSelect>(pane, 'apm-select');

    select.dispatchEvent(
      new CustomEvent('select-change', { detail: { value: 'colour' }, bubbles: true }),
    );

    expect(sort.value.source).toEqual({ key: 'name', order: 'asc' });
  });

  it('start the list at the top again when the sort changes', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();
    body(pane).scrollTop = 300;

    chooseSort(pane, 'size');
    await settle(pane);

    expect(body(pane).scrollTop).toBe(0);
  });
});

describe('the file grid', () => {
  it('gets the request, the sort, the size and the selection of this pane', async () => {
    goTo('comparison', TRIP);
    setSort('comparison', { key: 'date', order: 'desc' });
    setGridSize('comparison', 224);
    selection.comparison.set([2, 3]);

    const pane = await mountPane({ pane: 'comparison' });

    const shown = grid(pane);
    expect(shown.request).toEqual(pane.browseRequest);
    expect(shown.sort).toBe('date');
    expect(shown.order).toBe('desc');
    expect(shown.size).toBe(224);
    expect(shown.label).toBe('Comparison files');
    expect([...shown.selectedIds]).toEqual([2, 3]);
    expect(shown.selectionActive).toBe(true);
  });

  it('follows the size and the selection when they change', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();
    expect(grid(pane).selectionActive).toBe(false);

    setGridSize('source', 192);
    selection.source.set([5]);
    await settle(pane);

    expect(grid(pane).size).toBe(192);
    expect([...grid(pane).selectedIds]).toEqual([5]);
    expect(grid(pane).selectionActive).toBe(true);
  });

  it('shows a card activation as an item to open, from the pane', async () => {
    goTo('source', TRIP);
    server.state.total = 3;
    const pane = await mountPane();
    const opened = record<ItemOpenDetail>('item-open');

    grid(pane).dispatchEvent(
      new CustomEvent('card-activate', {
        detail: { id: 12, index: 1, via: 'card', shift: false, toggle: false },
        bubbles: true,
      }),
    );

    expect(opened).toEqual([{ id: 12, index: 1 }]);
  });

  it('selects on a click once something is selected', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();

    grid(pane).dispatchEvent(
      new CustomEvent('card-activate', {
        detail: { id: 12, index: 1, via: 'checkbox', shift: false, toggle: true },
        bubbles: true,
      }),
    );
    await settle(pane);

    expect(selection.source.toArray()).toEqual([12]);
    expect(grid(pane).selectionActive).toBe(true);
  });

  it('gives the loaded item at an index, for the viewer to start from', async () => {
    goTo('source', TRIP);
    server.state.total = 3;
    const pane = await mountPane();
    await settle(pane);

    expect(pane.itemAt(1)?.name).toBe('IMG_1.jpg');
    expect(pane.itemAt(1)?.id).toBe(2);
    expect(pane.itemAt(3)).toBeUndefined();
  });

  it('has no item to give where it shows no file grid', async () => {
    goTo('source', ROOT_LOCATION);
    const pane = await mountPane();
    await settle(pane);

    expect(pane.itemAt(0)).toBeUndefined();
    expect(document.createElement('apm-pane').itemAt(0)).toBeUndefined();
  });

  describe('when it has no files', () => {
    const emptyText = async (pane: ApmPane) => {
      await settle(pane);
      return text(part(pane, 'apm-file-grid [role="status"]'));
    };

    it('calls a folder with nothing at all empty', async () => {
      goTo('source', TRIP);

      expect(await emptyText(await mountPane())).toBe('This folder is empty');
    });

    it('does not call a folder empty when it holds folders', async () => {
      goTo('source', TRIP);
      serveFolders({ name: 'raw', path: 'trip/raw' });

      expect(await emptyText(await mountPane())).toBe('No files in this folder');
    });

    it('blames the filters when they are on', async () => {
      goTo('source', TRIP);
      serveFolders({ name: 'raw', path: 'trip/raw', count: 0 });
      const filter: Filter = { ...EMPTY_FILTER, flags: ['selected'] };

      expect(await emptyText(await mountPane({ filter }))).toBe('No files match the filters');
    });

    it('blames the filters for a folder that holds nothing either', async () => {
      goTo('source', TRIP);
      const filter: Filter = { ...EMPTY_FILTER, tags: ['family'] };

      expect(await emptyText(await mountPane({ filter }))).toBe('No items match the filters');
    });

    it('does not blame a filter that holds nothing but blank entries', async () => {
      goTo('source', TRIP);
      const filter: Filter = { ...EMPTY_FILTER, cameras: ['  '], tags: [''] };

      expect(await emptyText(await mountPane({ filter }))).toBe('This folder is empty');
    });

    it('tells that the folders without matches are hidden', async () => {
      goTo('source', TRIP);
      serveFolders({ name: 'raw', path: 'trip/raw', count: 0, total: 8 });

      const pane = await mountPane({ hideUnmatched: true });

      expect(await emptyText(pane)).toBe('Nothing to show: the folders without matches are hidden');
    });

    it('does not claim that the folder is empty before it knows its folders', async () => {
      goTo('source', TRIP);
      serveFolders({ name: 'raw', path: 'trip/raw' });
      server.hold('folders');

      const pane = await mountPane();

      expect(await emptyText(pane)).toBe('No files in this folder');
      server.release('folders');
      await settle(pane);
      expect(await emptyText(pane)).toBe('No files in this folder');
    });

    it('does not claim that the folder is empty when its folders failed', async () => {
      goTo('source', TRIP);
      server.state.folders = () => ({ status: 500, code: 'internal', message: 'oops' });

      expect(await emptyText(await mountPane())).toBe('No files in this folder');
    });
  });
});

describe('a folder that disappears', () => {
  const exists = (...dirs: string[]) => {
    server.state.folders = (request) =>
      dirs.includes(request.dir) ? foldersAnswer([]) : NOT_FOUND;
  };

  it('is left for its nearest ancestor, with one toast and no history entry', async () => {
    goTo('source', { volume: 'photos', dir: 'trip/2024/jan' });
    exists('', 'trip');
    const push = vi.spyOn(window.history, 'pushState');
    const replace = vi.spyOn(window.history, 'replaceState');

    const pane = await mountPane();
    await settle(pane, 20);

    expect(locations.source.value).toEqual(TRIP);
    expect(server.calls.folders.map((request) => request.dir)).toEqual([
      'trip/2024/jan',
      'trip/2024',
      'trip',
    ]);
    expect(toasts.value.map((toast) => toast.message)).toEqual(['The folder jan no longer exists']);
    expect(push).not.toHaveBeenCalled();
    expect(replace).toHaveBeenCalledTimes(2);
    expect(window.location.search).toBe('?src=photos/trip');
    expect(alerts(pane)).toEqual([]);
  });

  it('tells again for a folder that disappears later', async () => {
    goTo('source', { volume: 'photos', dir: 'trip/2024' });
    exists('', 'trip', 'trip/2023');
    const pane = await mountPane();
    await settle(pane, 20);
    expect(toasts.value).toHaveLength(1);

    goTo('source', { volume: 'photos', dir: 'trip/2022' });
    await settle(pane, 20);

    expect(locations.source.value).toEqual(TRIP);
    expect(toasts.value.map((toast) => toast.message)).toEqual([
      'The folder 2024 no longer exists',
      'The folder 2022 no longer exists',
    ]);
  });

  it('is not a reason to show anything while it is left', async () => {
    goTo('source', { volume: 'photos', dir: 'trip/2024' });
    exists('', 'trip');
    server.hold('folders');
    const pane = await mountPane();
    server.release('folders');
    await settle(pane, 1);

    expect(alerts(pane)).toEqual([]);
  });

  it('stops at a volume that does not exist any more, with a way out', async () => {
    goTo('source', { volume: 'photos', dir: '' });
    server.state.folders = () => NOT_FOUND;

    const pane = await mountPane();
    await settle(pane, 10);

    expect(alerts(pane)).toEqual(['The folder photos no longer exists Show all volumes']);
    expect(server.calls.folders).toHaveLength(1);
    expect(toasts.value).toEqual([]);

    clickLink(part(pane, '[role="alert"] a'));
    await settle(pane);

    expect(locations.source.value).toEqual(ROOT_LOCATION);
  });

  it('drops the counts and the selection of the folder that is gone', async () => {
    goTo('source', TRIP);
    server.state.folders = () => foldersAnswer([], { in_folder: 5, in_subfolders: 1 });
    const counts = record<PaneCountsDetail>('pane-counts');
    const pane = await mountPane();
    selection.source.set([1, 2]);
    expect(counts.at(-1)).toEqual({ pane: 'source', counts: { in_folder: 5, in_subfolders: 1 } });

    server.state.folders = () => NOT_FOUND;
    goTo('source', { volume: 'photos', dir: 'trip/gone' });
    await settle(pane, 10);

    expect(counts.at(-1)).toEqual({ pane: 'source', counts: null });
    expect(selection.source.toArray()).toEqual([]);
  });
});

describe('a plan that the server forgot', () => {
  const plan: PlanRef = { plan_id: 4, side: 'comparison' };

  it('is reported once, and the folders it was showing stay', async () => {
    goTo('comparison', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    const stale = record<PlanStaleDetail>('plan-stale');
    const pane = await mountPane({ pane: 'comparison' });
    expect(stale).toEqual([]);

    server.state.folders = () => PLAN_GONE;
    pane.plan = plan;
    await settle(pane);

    expect(stale).toEqual([{ pane: 'comparison', plan }]);
    expect(rowNames(pane)).toEqual(['raw']);
    expect(alerts(pane)).toEqual([]);
    expect(pane.querySelector('[role="status"][aria-label="Waiting for the plan"]')).not.toBeNull();

    // Another render does not ask again.
    pane.hideUnmatched = true;
    await settle(pane);
    expect(stale).toHaveLength(1);
  });

  it('leaves the grid with placeholders until there is a plan again', async () => {
    goTo('comparison', TRIP);
    server.state.total = 3;
    serveFolders({ name: 'raw', path: 'trip/raw' });
    const pane = await mountPane({ pane: 'comparison' });
    expect(grid(pane).request).toEqual(pane.browseRequest);

    server.state.folders = () => PLAN_GONE;
    pane.plan = plan;
    await settle(pane);
    expect(grid(pane).request).toBeUndefined();

    serveFolders({ name: 'raw', path: 'trip/raw' });
    pane.plan = { plan_id: 5, side: 'comparison' };
    await settle(pane);

    expect(grid(pane).request).toMatchObject({ plan: { plan_id: 5, side: 'comparison' } });
    expect(pane.querySelector('[aria-label="Waiting for the plan"]')).toBeNull();
  });

  it('is reported again for the next plan that goes missing', async () => {
    goTo('comparison', TRIP);
    const stale = record<PlanStaleDetail>('plan-stale');
    server.state.folders = () => PLAN_GONE;
    const pane = await mountPane({ pane: 'comparison', plan });

    const next: PlanRef = { plan_id: 5, side: 'comparison' };
    pane.plan = next;
    await settle(pane);

    expect(stale).toEqual([
      { pane: 'comparison', plan },
      { pane: 'comparison', plan: next },
    ]);
  });
});

describe('pane-counts', () => {
  it('reports no counts while they load, then the counts, once each', async () => {
    goTo('source', TRIP);
    server.state.folders = () => foldersAnswer([], { in_folder: 12, in_subfolders: 30 });
    server.hold('folders');
    const counts = record<PaneCountsDetail>('pane-counts');

    const pane = await mountPane();
    expect(counts).toEqual([{ pane: 'source', counts: null }]);

    server.release('folders');
    await settle(pane);
    expect(counts).toEqual([
      { pane: 'source', counts: null },
      { pane: 'source', counts: { in_folder: 12, in_subfolders: 30 } },
    ]);

    pane.hideUnmatched = true;
    await settle(pane);
    expect(counts).toHaveLength(2);
  });

  it('reports the counts of the pane it is, and no counts for another folder until they come', async () => {
    goTo('comparison', TRIP);
    server.state.folders = () => foldersAnswer([], { in_folder: 1, in_subfolders: 2 });
    const counts = record<PaneCountsDetail>('pane-counts');
    const pane = await mountPane({ pane: 'comparison' });

    server.hold('folders');
    server.state.folders = () => foldersAnswer([], { in_folder: 7, in_subfolders: 0 });
    goTo('comparison', { volume: 'photos', dir: 'other' });
    await settle(pane);
    server.release('folders');
    await settle(pane);

    expect(counts).toEqual([
      { pane: 'comparison', counts: null },
      { pane: 'comparison', counts: { in_folder: 1, in_subfolders: 2 } },
      { pane: 'comparison', counts: null },
      { pane: 'comparison', counts: { in_folder: 7, in_subfolders: 0 } },
    ]);
  });

  it('reports no counts once it is disabled', async () => {
    goTo('source', TRIP);
    server.state.folders = () => foldersAnswer([], { in_folder: 3, in_subfolders: 0 });
    const counts = record<PaneCountsDetail>('pane-counts');
    const pane = await mountPane();

    pane.disabled = true;
    await settle(pane);

    expect(counts.at(-1)).toEqual({ pane: 'source', counts: null });
  });

  it('reports the counts again after an event on disk changed them', async () => {
    goTo('source', TRIP);
    server.state.folders = () => foldersAnswer([], { in_folder: 3, in_subfolders: 0 });
    const counts = record<PaneCountsDetail>('pane-counts');
    const pane = await mountPane();

    vi.useFakeTimers();
    server.state.folders = () => foldersAnswer([], { in_folder: 4, in_subfolders: 0 });
    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['trip'] });
    await vi.advanceTimersByTimeAsync(INVALIDATE_DEBOUNCE_MS);
    await settle(pane);

    expect(counts.at(-1)).toEqual({ pane: 'source', counts: { in_folder: 4, in_subfolders: 0 } });
  });
});

describe('the selection', () => {
  async function selecting(props: Partial<ApmPane> = {}): Promise<ApmPane> {
    goTo('source', TRIP);
    const pane = await mountPane(props);
    selection.source.set([1, 2, 3]);
    return pane;
  }

  it('is cleared by another folder', async () => {
    const pane = await selecting();

    goTo('source', { volume: 'photos', dir: 'other' });
    await settle(pane);

    expect(selection.source.toArray()).toEqual([]);
  });

  it('is cleared by a filter, because it is another listing', async () => {
    const pane = await selecting();

    pane.filter = { ...EMPTY_FILTER, cameras: ['Nikon Z9'] };
    await settle(pane);

    expect(selection.source.toArray()).toEqual([]);
  });

  it('survives a new sort, a plan and the other pane moving', async () => {
    const pane = await selecting();

    setSort('source', { key: 'size', order: 'desc' });
    pane.plan = { plan_id: 2, side: 'source' };
    goTo('comparison', { volume: 'backup', dir: 'old' });
    await settle(pane);

    expect(selection.source.toArray()).toEqual([1, 2, 3]);
  });

  it('is the one of the pane, whatever the other pane has', async () => {
    const pane = await selecting();
    selection.comparison.set([9]);

    expect([...grid(pane).selectedIds]).toEqual([1, 2, 3]);
  });

  it('asks the server for the ids of the folder in the sort that is on screen', async () => {
    const pane = await selecting();
    server.state.ids = () => [30, 20, 10];
    setSort('source', { key: 'size', order: 'desc' });
    await settle(pane);

    await selection.source.selectAll();

    expect(server.calls.ids).toHaveLength(1);
    expect(server.calls.ids[0]).toMatchObject({
      volume_id: 1,
      dir: 'trip',
      sort: 'size',
      order: 'desc',
      filter: EMPTY_FILTER,
      plan: null,
    });
    expect(server.calls.idSignals[0]).toBeInstanceOf(AbortSignal);
    expect(selection.source.toArray()).toEqual([30, 20, 10]);
  });

  it('asks for the files of the folder it is in now', async () => {
    const pane = await selecting();
    goTo('source', { volume: 'photos', dir: 'other' });
    await settle(pane);
    server.state.ids = () => [7];

    await selection.source.selectAll();

    expect(server.calls.ids[0]).toMatchObject({ volume_id: 1, dir: 'other' });
  });

  it('has no files to select at the volumes root, and does not ask', async () => {
    goTo('source', ROOT_LOCATION);
    await mountPane();

    await selection.source.selectAll();

    expect(server.calls.ids).toEqual([]);
    expect(selection.source.toArray()).toEqual([]);
  });

  it('is served by the pane of its own id', async () => {
    goTo('comparison', { volume: 'backup', dir: 'old' });
    await mountPane({ pane: 'comparison' });
    server.state.ids = () => [4, 5];

    await selection.comparison.selectAll();
    await selection.source.selectAll();

    expect(server.calls.ids).toHaveLength(1);
    expect(server.calls.ids[0]).toMatchObject({ volume_id: 2, dir: 'old' });
    expect(selection.comparison.toArray()).toEqual([4, 5]);
  });

  it('is no longer served once the pane is removed', async () => {
    const pane = await selecting();

    pane.remove();
    await selection.source.selectAll();

    expect(server.calls.ids).toEqual([]);
  });

  it('is brought up to date by a change on disk in the folder', async () => {
    const pane = await selecting();
    server.state.ids = () => [1, 3];

    vi.useFakeTimers();
    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['trip'] });
    await settle(pane);

    expect(selection.source.toArray()).toEqual([1, 3]);
  });

  it.each([
    ['another folder', { volume_id: 1, dirs: ['other'] }],
    ['a subfolder', { volume_id: 1, dirs: ['trip/raw'] }],
    ['another volume', { volume_id: 2, dirs: ['trip'] }],
  ])('is left alone by a change in %s', async (_name, change) => {
    const pane = await selecting();
    const reconcile = vi.spyOn(selection.source, 'reconcile');

    events.emit({ type: 'fs_change', ...change });
    await settle(pane);

    expect(reconcile).not.toHaveBeenCalled();
  });

  it('is brought up to date by a change that lists the folder among others', async () => {
    const pane = await selecting();
    const reconcile = vi.spyOn(selection.source, 'reconcile');

    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['other', 'trip'] });
    await settle(pane);

    expect(reconcile).toHaveBeenCalledTimes(1);
  });

  it('is not brought up to date once the pane is removed', async () => {
    const pane = await selecting();
    const reconcile = vi.spyOn(selection.source, 'reconcile');

    pane.remove();
    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['trip'] });

    expect(reconcile).not.toHaveBeenCalled();
  });
});

describe('the base folder row', () => {
  const base: PaneLocation = { volume: 'photos', dir: 'trip/raw' };

  it('is not there by default', async () => {
    const pane = await mountPane();

    expect(baseRow(pane)).toBeNull();
  });

  it('says that the folder is the base', async () => {
    goTo('source', TRIP);

    const pane = await mountPane({ baseMode: 'current' });

    expect(text(baseRow(pane))).toBe('Base folder: this folder');
    expect(baseRow(pane)?.querySelector('button')).toBeNull();
  });

  describe('pinned', () => {
    it('shows the base, and lets the user use this folder or go to the base', async () => {
      goTo('source', TRIP);
      const changes = record<BaseChangeDetail>('base-change');
      const pane = await mountPane({ baseMode: 'pinned', base });
      const row = baseRow(pane) as Element;

      expect(text(row)).toContain('photos/trip/raw');
      expect(row.querySelector('[title="photos/trip/raw"]')).not.toBeNull();
      expect(buttonNamed(row, 'Reset')).toBeUndefined();

      buttonNamed(row, 'Use this folder')?.click();
      expect(changes).toEqual([{ pane: 'source', base: TRIP }]);

      buttonNamed(row, 'Go to base')?.click();
      await settle(pane);
      expect(locations.source.value).toEqual(base);
      expect(changes).toHaveLength(1);
    });

    it('cannot pick the base again, or go to it, at the base', async () => {
      goTo('source', base);

      const pane = await mountPane({ baseMode: 'pinned', base });

      const row = baseRow(pane) as Element;
      expect(buttonNamed(row, 'Use this folder')?.hasAttribute('disabled')).toBe(true);
      expect(buttonNamed(row, 'Go to base')?.hasAttribute('disabled')).toBe(true);
    });

    it('cannot pick the volumes root', async () => {
      const pane = await mountPane({ baseMode: 'pinned', base });

      const row = baseRow(pane) as Element;
      expect(buttonNamed(row, 'Use this folder')?.hasAttribute('disabled')).toBe(true);
      expect(buttonNamed(row, 'Go to base')?.hasAttribute('disabled')).toBe(false);
    });

    it('says that no base is selected, and has none to go to', async () => {
      goTo('source', TRIP);

      const pane = await mountPane({ baseMode: 'pinned', base: null });

      const row = baseRow(pane) as Element;
      expect(text(row)).toContain('Not selected');
      expect(buttonNamed(row, 'Go to base')?.hasAttribute('disabled')).toBe(true);
      expect(buttonNamed(row, 'Use this folder')?.hasAttribute('disabled')).toBe(false);
    });

    it('names the default base of the tool when that is the base', async () => {
      goTo('source', TRIP);
      const changes = record<BaseChangeDetail>('base-change');

      const pane = await mountPane({
        baseMode: 'pinned',
        base,
        baseDefault: { label: 'Source folder', active: true },
      });

      const row = baseRow(pane) as Element;
      expect(text(row)).toContain('Source folder');
      expect(row.querySelector('[title="Source folder"]')).not.toBeNull();
      expect(buttonNamed(row, 'Reset')).toBeUndefined();
      // Picking the folder that is the default pins it for good.
      goTo('source', base);
      await settle(pane);
      expect(buttonNamed(row, 'Use this folder')?.hasAttribute('disabled')).toBe(false);
      buttonNamed(row, 'Use this folder')?.click();
      expect(changes).toEqual([{ pane: 'source', base }]);
    });

    it('offers to use the default again once the user pinned another folder', async () => {
      goTo('source', TRIP);
      const changes = record<BaseChangeDetail>('base-change');

      const pane = await mountPane({
        baseMode: 'pinned',
        base,
        baseDefault: { label: 'Source folder', active: false },
      });

      const reset = buttonNamed(baseRow(pane) as Element, 'Reset');
      expect(reset?.getAttribute('title')).toBe('Use again: Source folder');
      reset?.click();
      expect(changes).toEqual([{ pane: 'source', base: null }]);
    });

    it('is not there while the pane is disabled', async () => {
      const pane = await mountPane({
        baseMode: 'pinned',
        base,
        disabled: true,
        disabledReason: 'Nothing to compare',
      });

      expect(baseRow(pane)).toBeNull();
    });
  });
});

describe('the layout', () => {
  it('fills its host and lets only the body scroll', async () => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    server.state.total = 3;

    const pane = await mountPane({ baseMode: 'current' });

    expect(
      ['block', 'h-full', 'min-h-0', 'min-w-0'].every((name) => pane.classList.contains(name)),
    ).toBe(true);
    const region = part(pane, 'section');
    expect(region.className).toContain('flex h-full min-h-0 min-w-0 flex-col');
    expect(body(pane).className).toContain('min-h-0 flex-1 overflow-y-auto');
    expect(part(pane, 'header').className).toContain('flex-none');
    expect(part(pane, '[role="group"]').className).toContain('flex-none');
    // Nothing else in the pane scrolls: the lists live in the body.
    const scrollers = [
      ...pane.querySelectorAll('[class*="overflow-y-"], .overflow-auto, .overflow-scroll'),
    ];
    expect(scrollers).toEqual([body(pane)]);
    expect(body(pane).contains(folderList(pane))).toBe(true);
    expect(body(pane).contains(grid(pane))).toBe(true);
  });

  it('is a region named after its heading, or its pane', async () => {
    const named = await mountPane({ heading: 'Photos 2024' });
    const plain = await mountPane({ pane: 'comparison' });

    expect(part(named, 'section').getAttribute('aria-label')).toBe('Photos 2024');
    expect(part(plain, 'section').getAttribute('aria-label')).toBe('Comparison pane');
    expect(text(part(named, 'header .badge'))).toBe('Photos 2024');
    expect(plain.querySelector('header .badge')).toBeNull();
  });

  it('shows that it is the pane the user touched last, in a ring and in words', async () => {
    const source = await mountPane({ pane: 'source' });
    const comparison = await mountPane({ pane: 'comparison' });

    expect(part(source, 'section').getAttribute('aria-current')).toBe('true');
    expect(part(source, 'section').className).toContain('ring-1 ring-inset ring-primary/40');
    expect(text(part(source, '.sr-only'))).toBe('active pane');
    expect(part(comparison, 'section').hasAttribute('aria-current')).toBe(false);
    expect(part(comparison, 'section').className).not.toContain('ring-1');
    expect(comparison.textContent).not.toContain('active pane');

    activePane.value = 'comparison';
    await settle(source);
    await settle(comparison);

    expect(part(source, 'section').hasAttribute('aria-current')).toBe(false);
    expect(part(comparison, 'section').getAttribute('aria-current')).toBe('true');
  });

  it('becomes the active pane when the user focuses something in it', async () => {
    const pane = await mountPane({ pane: 'comparison' });
    expect(activePane.value).toBe('source');

    part(pane, 'button[aria-label="Up one folder"]').dispatchEvent(
      new FocusEvent('focusin', { bubbles: true }),
    );

    expect(activePane.value).toBe('comparison');
  });

  it('starts its list at the top in another folder', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();
    body(pane).scrollTop = 250;

    goTo('source', { volume: 'photos', dir: 'other' });
    await settle(pane);

    expect(body(pane).scrollTop).toBe(0);
  });

  it('keeps its scroll position when only the plan changes', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();
    body(pane).scrollTop = 250;

    pane.plan = { plan_id: 8, side: 'source' };
    await settle(pane);

    expect(body(pane).scrollTop).toBe(250);
  });
});

describe('the keyboard focus after a folder is opened', () => {
  it('moves to the first folder of the new listing', async () => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    const pane = await mountPane();
    row(pane, 'raw').focus();
    expect(document.activeElement).toBe(row(pane, 'raw'));

    server.state.folders = () =>
      foldersAnswer([folderEntry({ name: 'deep', path: 'trip/raw/deep' })]);
    clickLink(row(pane, 'raw'));
    await settle(pane);

    expect(rowNames(pane)).toEqual(['deep']);
    expect(document.activeElement).toBe(row(pane, 'deep'));
  });

  it('moves to the files when the new folder has no subfolders', async () => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    server.state.total = 3;
    const pane = await mountPane();
    row(pane, 'raw').focus();
    const focus = vi.spyOn(grid(pane), 'focus');

    serveFolders();
    clickLink(row(pane, 'raw'));
    await settle(pane);

    expect(focus).toHaveBeenCalledTimes(1);
  });

  it('is left alone when the user did not have the focus in the pane', async () => {
    goTo('source', TRIP);
    serveFolders({ name: 'raw', path: 'trip/raw' });
    const pane = await mountPane();
    const outside = document.createElement('button');
    document.body.append(outside);
    cleanups.push(() => outside.remove());
    outside.focus();

    server.state.folders = () =>
      foldersAnswer([folderEntry({ name: 'deep', path: 'trip/raw/deep' })]);
    clickLink(row(pane, 'raw'));
    await settle(pane);

    expect(document.activeElement).toBe(outside);
  });
});

describe('a pane that left the page', () => {
  it('does not touch the shared selection when an update was still queued', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();
    selection.source.set([1, 2]);

    goTo('source', { volume: 'photos', dir: 'other' });
    pane.remove();
    await settle(pane);

    expect(selection.source.toArray()).toEqual([1, 2]);
  });

  it('reports nothing when an update was still queued', async () => {
    goTo('source', TRIP);
    const pane = await mountPane();
    const counts: PaneCountsDetail[] = [];
    pane.addEventListener('pane-counts', (event) =>
      counts.push((event as CustomEvent<PaneCountsDetail>).detail),
    );

    pane.disabled = true;
    pane.remove();
    await settle(pane);

    expect(counts).toEqual([]);
  });
});
