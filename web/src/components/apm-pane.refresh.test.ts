import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import { events } from '@/api/sse';
import type { ApmFileCard } from '@/components/apm-file-card';
import '@/components/apm-file-grid';
import type { ApmPane, PaneCountsDetail } from '@/components/apm-pane';
import '@/components/apm-pane';
import { type PaneLocation, setLocation, syncFromLocation } from '@/stores/nav';
import { GRID_SIZE, setGridSize, setSort } from '@/stores/prefs';
import { resetSelectionForTests } from '@/stores/selection';
import { clearToasts } from '@/stores/toasts';
import { loadVolumes } from '@/stores/volumes';
import { type BrowseServer, foldersAnswer, stubBrowseServer } from '@/test/browse-server';
import { folderEntry } from '@/test/folder-entry';
import { mount } from '@/test/helpers';
import { EMPTY_FILTER } from '@/util/filter';

const TRIP: PaneLocation = { volume: 'photos', dir: 'trip' };

let server: BrowseServer;
const cleanups: (() => void)[] = [];

beforeEach(async () => {
  window.history.replaceState(null, '', '/');
  syncFromLocation();
  server = stubBrowseServer();
  await loadVolumes();
});

afterEach(() => {
  for (const element of document.querySelectorAll('apm-pane, apm-file-grid')) element.remove();
  for (const cleanup of cleanups.splice(0)) cleanup();
  events.stop();
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

/** Lets the stubbed server answer and everything under `root` render what came back. */
async function settle(root: Element, rounds = 6): Promise<void> {
  for (let round = 0; round < rounds; round += 1) {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await Promise.all(litElementsIn(root).map((element) => element.updateComplete));
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

const rowNames = (pane: ApmPane) =>
  [...pane.querySelectorAll<HTMLAnchorElement>('apm-folder-list a[data-name]')].map(
    (row) => row.dataset.name,
  );

const cardNames = (root: Element) =>
  [...root.querySelectorAll<ApmFileCard>('apm-file-card')].map((card) => card.item?.name);

/** The server has these subfolders and this many files in every directory it is asked about. */
function serve(subfolders: readonly string[], files: number): void {
  server.state.folders = () =>
    foldersAnswer(
      subfolders.map((name) => folderEntry({ name })),
      { in_folder: files, in_subfolders: 0 },
    );
  server.state.total = files;
}

describe('ApmPane.refresh', () => {
  it('asks the server for the folders and for the files again, at once', async () => {
    setLocation('source', TRIP);
    serve(['raw'], 3);
    const pane = await mountPane();
    expect(server.calls.folders).toHaveLength(1);
    expect(server.calls.items).toHaveLength(1);

    pane.refresh();
    await settle(pane);

    expect(server.calls.folders).toHaveLength(2);
    expect(server.calls.folders[1]).toEqual(server.calls.folders[0]);
    expect(server.calls.items).toHaveLength(2);
    expect(server.calls.items[1]).toEqual(server.calls.items[0]);
  });

  it('shows what the server answers now: the subfolders, the files and the counts', async () => {
    setLocation('source', TRIP);
    serve(['raw'], 3);
    const counts = record<PaneCountsDetail>('pane-counts');
    const pane = await mountPane();
    expect(rowNames(pane)).toEqual(['raw']);
    expect(cardNames(pane)).toHaveLength(3);

    serve(['raw', 'edits'], 5);
    pane.refresh();
    await settle(pane);

    expect(rowNames(pane)).toEqual(['raw', 'edits']);
    expect(cardNames(pane)).toHaveLength(5);
    expect(counts.at(-1)).toEqual({ pane: 'source', counts: { in_folder: 5, in_subfolders: 0 } });
  });

  it('keeps what is shown until the answer arrives', async () => {
    setLocation('source', TRIP);
    serve(['raw'], 3);
    const pane = await mountPane();

    server.hold('folders');
    server.hold('items');
    serve(['edits'], 1);
    pane.refresh();
    await settle(pane);
    expect(rowNames(pane)).toEqual(['raw']);
    expect(cardNames(pane)).toHaveLength(3);

    server.release('folders');
    server.release('items');
    await settle(pane);
    expect(rowNames(pane)).toEqual(['edits']);
    expect(cardNames(pane)).toHaveLength(1);
  });

  it('refreshes the pane it is called on and no other', async () => {
    setLocation('source', TRIP);
    setLocation('comparison', { volume: 'backup', dir: 'old' });
    serve([], 2);
    await mountPane({ pane: 'source' });
    const comparison = await mountPane({ pane: 'comparison' });
    server.calls.folders.length = 0;
    server.calls.items.length = 0;

    comparison.refresh();
    await settle(comparison);

    expect(server.calls.folders.map((request) => request.dir)).toEqual(['old']);
    expect(server.calls.items.map((request) => request.dir)).toEqual(['old']);
  });

  it('keeps the plan and the filter of the pane in the request', async () => {
    setLocation('source', TRIP);
    serve([], 2);
    const filter = { ...EMPTY_FILTER, ratings: [4, 5] };
    const plan = { plan_id: 3, side: 'source' as const };
    const pane = await mountPane({ filter, plan });

    pane.refresh();
    await settle(pane);

    const expected = { volume_id: 1, dir: 'trip', filter, plan };
    expect(server.calls.folders).toEqual([expected, expected]);
    expect(server.calls.items[1]).toMatchObject(expected);
  });

  it('asks again after another refresh, as often as it is called', async () => {
    setLocation('source', TRIP);
    serve([], 2);
    const pane = await mountPane();

    pane.refresh();
    await settle(pane);
    pane.refresh();
    await settle(pane);

    expect(server.calls.folders).toHaveLength(3);
    expect(server.calls.items).toHaveLength(3);
  });

  it('does nothing while the pane has no listing to ask about', async () => {
    setLocation('source', TRIP);
    const pane = await mountPane({ disabled: true });
    expect(server.calls.folders).toEqual([]);

    expect(() => pane.refresh()).not.toThrow();
    await settle(pane);

    expect(server.calls.folders).toEqual([]);
    expect(server.calls.items).toEqual([]);
  });

  it('does nothing once the pane left the page', async () => {
    setLocation('source', TRIP);
    serve([], 2);
    const pane = await mountPane();
    pane.remove();
    server.calls.folders.length = 0;
    server.calls.items.length = 0;

    expect(() => pane.refresh()).not.toThrow();
    await settle(pane);

    expect(server.calls.folders).toEqual([]);
    expect(server.calls.items).toEqual([]);
  });
});

describe('ApmFileGrid.refresh', () => {
  const request: BrowseRequest = { volume_id: 1, dir: 'trip', filter: EMPTY_FILTER, plan: null };

  it('fetches the files on screen again and shows the new answer', async () => {
    server.state.total = 3;
    const grid = await mount('apm-file-grid', { request });
    await settle(grid);
    expect(cardNames(grid)).toHaveLength(3);

    server.state.total = 4;
    grid.refresh();
    await settle(grid);

    expect(server.calls.items).toHaveLength(2);
    expect(cardNames(grid)).toHaveLength(4);
  });

  it('does nothing without a listing', async () => {
    const grid = await mount('apm-file-grid');
    await settle(grid);

    expect(() => grid.refresh()).not.toThrow();
    await settle(grid);

    expect(server.calls.items).toEqual([]);
  });
});
