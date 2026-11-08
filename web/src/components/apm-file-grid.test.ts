import { afterEach, describe, expect, it, vi } from 'vitest';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FileItem } from '@/api/generated/FileItem';
import type { ItemsRequest } from '@/api/generated/ItemsRequest';
import { events } from '@/api/sse';
import type { ApmFileCard, CardActivateDetail } from '@/components/apm-file-card';
import {
  type ApmFileGrid,
  columnsFor,
  type GridState,
  gridFocusTarget,
  normalizeGridSize,
  revealDelta,
} from '@/components/apm-file-grid';
import { INVALIDATE_DEBOUNCE_MS } from '@/stores/paged-items';
import { GRID_SIZE } from '@/stores/prefs';
import { fileItem } from '@/test/file-item';
import { mount } from '@/test/helpers';

afterEach(() => {
  events.stop();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const request: BrowseRequest = {
  volume_id: 1,
  dir: 'trip',
  filter: { cameras: [], ratings: [], flags: [], tags: [], selection_id: null },
  plan: null,
};

interface ServerState {
  total: number;
  item(index: number): FileItem;
  failure: string | null;
  gate: Promise<void> | null;
}

/** Answers `POST /api/browse/items` from `state`; the test changes `state` between requests. */
function stubServer(initial: Partial<ServerState> = {}) {
  const state: ServerState = {
    total: 5,
    item: (index) => fileItem({ id: index + 1, name: `IMG_${index}.jpg` }),
    failure: null,
    gate: null,
    ...initial,
  };
  const bodies: ItemsRequest[] = [];
  let open: (() => void) | undefined;

  const stub = vi.fn(async (_url: string, init: RequestInit): Promise<Response> => {
    const body = JSON.parse(String(init.body)) as ItemsRequest;
    bodies.push(body);
    await state.gate;
    if (state.failure !== null) {
      const error = { code: 'internal', message: state.failure };
      return new Response(JSON.stringify({ error }), { status: 500 });
    }
    const items: FileItem[] = [];
    const end = Math.min(state.total, body.offset + body.limit);
    for (let index = body.offset; index < end; index += 1) items.push(state.item(index));
    return new Response(JSON.stringify({ items, total: state.total, offset: body.offset }));
  });
  vi.stubGlobal('fetch', stub);

  return {
    state,
    bodies,
    fetch: stub,
    /** Keeps the answers back until `release()`. */
    hold() {
      state.gate = new Promise<void>((resolve) => {
        open = resolve;
      });
    },
    release() {
      state.gate = null;
      open?.();
    },
  };
}

function part<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`No element matches "${selector}"`);
  return found;
}

const cardsOf = (grid: ApmFileGrid): ApmFileCard[] => [...grid.querySelectorAll('apm-file-card')];
const optionOf = (card: ApmFileCard) => part(card, '[role="option"]');
const listboxOf = (grid: ApmFileGrid) => part(grid, '[role="listbox"]');
const namesOf = (grid: ApmFileGrid) => cardsOf(grid).map((card) => card.item?.name);

/** Indexes of the cards that are a tab stop. */
const tabStops = (grid: ApmFileGrid): number[] =>
  cardsOf(grid).flatMap((card, index) =>
    optionOf(card).getAttribute('tabindex') === '0' ? [index] : [],
  );

/** Index of the card that has the keyboard focus, -1 when it is none. */
const activeIndex = (grid: ApmFileGrid): number =>
  cardsOf(grid).findIndex((card) => optionOf(card) === document.activeElement);

/** Lets the stubbed server answer and the grid and its cards render what came back. */
async function settle(grid: ApmFileGrid): Promise<void> {
  for (let round = 0; round < 4; round += 1) {
    if (vi.isFakeTimers()) await vi.advanceTimersByTimeAsync(0);
    else await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await grid.updateComplete;
    await Promise.all(cardsOf(grid).map((card) => card.updateComplete));
  }
}

/** A grid showing `total` items, the first page in; happy-dom has no layout, so `columns` is set. */
async function listing(total: number, columns = 4, props: Partial<ApmFileGrid> = {}) {
  const server = stubServer({ total });
  const grid = await mount('apm-file-grid', { request, ...props });
  Object.defineProperty(grid, 'columns', { value: columns });
  await settle(grid);
  return { grid, server };
}

function click(target: Element, init: MouseEventInit = {}): MouseEvent {
  const event = new MouseEvent('click', { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

function press(target: Element, key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

/** The server announces that `dir` changed and the debounce runs out. */
async function changeOnDisk(dir: string): Promise<void> {
  vi.useFakeTimers();
  events.emit({ type: 'fs_change', volume_id: 1, dirs: [dir] });
  await vi.advanceTimersByTimeAsync(INVALIDATE_DEBOUNCE_MS);
}

describe('normalizeGridSize', () => {
  it.each([
    [GRID_SIZE.default, GRID_SIZE.default],
    [GRID_SIZE.min, GRID_SIZE.min],
    [GRID_SIZE.max, GRID_SIZE.max],
    [GRID_SIZE.min - 1, GRID_SIZE.min],
    [GRID_SIZE.max + 1, GRID_SIZE.max],
    [-5, GRID_SIZE.min],
    [128.4, 128],
    [Number.NaN, GRID_SIZE.default],
    [Number.POSITIVE_INFINITY, GRID_SIZE.default],
  ])('turns %s into %s', (size, expected) => {
    expect(normalizeGridSize(size)).toBe(expected);
  });
});

describe('columnsFor', () => {
  it.each([
    [-50, 160, 1],
    [0, 160, 1],
    [160, 160, 1],
    [327, 160, 1],
    [328, 160, 2],
    [1000, 96, 9],
    [1000, 320, 3],
  ])('fits %s px of cards of %s px in %s columns', (width, size, expected) => {
    expect(columnsFor(width, size)).toBe(expected);
  });
});

describe('gridFocusTarget', () => {
  // Ten items in rows of four: 0-3, 4-7, 8-9. A page is three visible rows, so two moved.
  const total = 10;
  const target = (key: string, current: number) => gridFocusTarget(key, current, total, 4, 3);

  it.each([
    ['ArrowRight', 5, 6],
    ['ArrowRight', 9, 9],
    ['ArrowLeft', 5, 4],
    ['ArrowLeft', 0, 0],
    ['ArrowDown', 1, 5],
    ['ArrowDown', 5, 9],
    ['ArrowDown', 6, 9],
    ['ArrowDown', 9, 9],
    ['ArrowDown', 8, 8],
    ['ArrowUp', 5, 1],
    ['ArrowUp', 9, 5],
    ['ArrowUp', 1, 1],
    ['ArrowUp', 0, 0],
    ['PageDown', 0, 8],
    ['PageDown', 2, 9],
    ['PageDown', 9, 9],
    ['PageUp', 9, 1],
    ['PageUp', 5, 1],
    ['PageUp', 0, 0],
    ['Home', 7, 0],
    ['End', 3, 9],
  ])('%s from %s goes to %s', (key, current, expected) => {
    expect(target(key, current)).toBe(expected);
  });

  it.each(['a', 'Enter', ' ', 'Tab', 'Escape', 'Shift'])('does not handle %j', (key) => {
    expect(target(key, 3)).toBeNull();
  });

  it('has nowhere to go in an empty listing', () => {
    expect(gridFocusTarget('ArrowDown', 0, 0, 4, 3)).toBeNull();
    expect(gridFocusTarget('Home', 0, 0, 4, 3)).toBeNull();
  });

  it('moves a page by at least one row', () => {
    expect(gridFocusTarget('PageDown', 0, 20, 4, 1)).toBe(4);
    expect(gridFocusTarget('PageDown', 0, 20, 4, 0)).toBe(4);
  });

  it('copes with one column and one item', () => {
    expect(gridFocusTarget('ArrowDown', 2, 6, 1, 3)).toBe(3);
    expect(gridFocusTarget('ArrowRight', 2, 6, 1, 3)).toBe(3);
    expect(gridFocusTarget('ArrowDown', 0, 1, 4, 3)).toBe(0);
    expect(gridFocusTarget('End', 0, 1, 4, 3)).toBe(0);
  });

  it('treats a missing column count as one column', () => {
    expect(gridFocusTarget('ArrowDown', 0, 5, 0, 3)).toBe(1);
    expect(gridFocusTarget('ArrowDown', 0, 5, Number.NaN, 3)).toBe(1);
  });

  it('starts from the nearest card when the current one is out of range', () => {
    expect(gridFocusTarget('ArrowLeft', 99, total, 4, 3)).toBe(8);
    expect(gridFocusTarget('ArrowRight', -3, total, 4, 3)).toBe(1);
  });
});

describe('revealDelta', () => {
  it.each([
    ['already in view', 10, 100, 500, 0],
    ['touching the bottom', 400, 100, 500, 0],
    ['above the view', -30, 100, 500, -30],
    ['below the view', 450, 100, 500, 50],
    ['taller than the view: aligns its top', 10, 600, 500, 10],
    ['taller than the view, above it', -10, 600, 500, -10],
  ])('%s', (_name, top, height, view, expected) => {
    expect(revealDelta(top, height, view)).toBe(expected);
  });
});

describe('listing', () => {
  it('shows placeholders while it has no request', async () => {
    const server = stubServer();
    const grid = await mount('apm-file-grid');

    const cards = cardsOf(grid);
    expect(cards).toHaveLength(24);
    expect(cards.every((card) => card.item === undefined)).toBe(true);
    expect(cards.every((card) => !card.focused)).toBe(true);
    const list = listboxOf(grid);
    expect(list.getAttribute('aria-busy')).toBe('true');
    expect(list.hasAttribute('tabindex')).toBe(false);
    expect(server.fetch).not.toHaveBeenCalled();
  });

  it('asks the server for the first page of the listing', async () => {
    const server = stubServer();
    const grid = await mount('apm-file-grid', { request, sort: 'date', order: 'desc' });
    await settle(grid);

    expect(server.bodies).toEqual([
      {
        volume_id: 1,
        dir: 'trip',
        filter: request.filter,
        plan: null,
        sort: 'date',
        order: 'desc',
        offset: 0,
        limit: 200,
      },
    ]);
  });

  it('shows placeholders until the first page is in, then the items', async () => {
    const server = stubServer({ total: 3 });
    server.hold();
    const grid = await mount('apm-file-grid', { request });

    expect(cardsOf(grid)).toHaveLength(24);
    expect(cardsOf(grid).every((card) => card.item === undefined)).toBe(true);
    expect(listboxOf(grid).getAttribute('aria-busy')).toBe('true');

    server.release();
    await settle(grid);

    expect(namesOf(grid)).toEqual(['IMG_0.jpg', 'IMG_1.jpg', 'IMG_2.jpg']);
    expect(listboxOf(grid).hasAttribute('aria-busy')).toBe(false);
    expect(cardsOf(grid)[1]?.textContent).toContain('IMG_1.jpg');
  });

  it('is a labelled multi-select list whose cards know their place', async () => {
    const { grid } = await listing(5);

    const list = listboxOf(grid);
    expect(list.getAttribute('aria-label')).toBe('Files');
    expect(list.getAttribute('aria-multiselectable')).toBe('true');
    const options = cardsOf(grid).map(optionOf);
    expect(options.map((option) => option.getAttribute('aria-posinset'))).toEqual([
      '1',
      '2',
      '3',
      '4',
      '5',
    ]);
    expect(options.every((option) => option.getAttribute('aria-setsize') === '5')).toBe(true);

    grid.label = 'Holiday photos';
    await grid.updateComplete;
    expect(listboxOf(grid).getAttribute('aria-label')).toBe('Holiday photos');
  });

  it('says so when the listing is empty', async () => {
    stubServer({ total: 0 });
    const grid = await mount('apm-file-grid', { request });
    await settle(grid);

    expect(part(grid, '[role="status"]').textContent?.trim()).toBe('No files');
    expect(grid.querySelector('[role="listbox"]')).toBeNull();
    expect(cardsOf(grid)).toHaveLength(0);

    grid.emptyText = 'Nothing here';
    await grid.updateComplete;
    expect(part(grid, '[role="status"]').textContent?.trim()).toBe('Nothing here');
  });

  it('reads empty-text from its attribute', async () => {
    stubServer({ total: 0 });
    const grid = document.createElement('apm-file-grid');
    grid.setAttribute('empty-text', 'No photos in this folder');
    await mount(grid, { request });
    await settle(grid);

    expect(part(grid, '[role="status"]').textContent?.trim()).toBe('No photos in this folder');
  });

  it('shows the error when the first page fails, and fetches again on Retry', async () => {
    const server = stubServer({ failure: 'The server is on fire.' });
    const grid = await mount('apm-file-grid', { request });
    await settle(grid);

    const alert = part(grid, '[role="alert"]');
    expect(alert.textContent).toContain('The server is on fire.');
    expect(cardsOf(grid)).toHaveLength(0);

    server.state.failure = null;
    part(alert, 'button').click();
    await settle(grid);

    expect(grid.querySelector('[role="alert"]')).toBeNull();
    expect(namesOf(grid)).toHaveLength(5);
    expect(server.bodies).toHaveLength(2);
  });

  it('keeps the items and shows the error above them when a refresh fails', async () => {
    const { grid, server } = await listing(5);
    server.state.failure = 'Temporarily broken.';

    await changeOnDisk('trip');
    await settle(grid);

    expect(part(grid, '[role="alert"]').textContent).toContain('Temporarily broken.');
    expect(part(grid, '[role="alert"]').parentElement?.classList.contains('sticky')).toBe(true);
    expect(namesOf(grid)).toHaveLength(5);

    server.state.failure = null;
    part(part(grid, '[role="alert"]'), 'button').click();
    await settle(grid);

    expect(grid.querySelector('[role="alert"]')).toBeNull();
    expect(namesOf(grid)).toHaveLength(5);
  });

  it('shows the fresh items when the folder changed on disk', async () => {
    const { grid, server } = await listing(3);
    server.state.item = (index) => fileItem({ id: index + 1, name: `NEW_${index}.jpg` });

    await changeOnDisk('trip');
    await settle(grid);

    expect(namesOf(grid)).toEqual(['NEW_0.jpg', 'NEW_1.jpg', 'NEW_2.jpg']);
    expect(server.bodies).toHaveLength(2);
  });

  it('does not refetch for a change in another folder', async () => {
    const { grid, server } = await listing(3);

    await changeOnDisk('elsewhere');
    await settle(grid);

    expect(server.bodies).toHaveLength(1);
  });

  it('gives every cell the size, rounded and kept inside the allowed range', async () => {
    const { grid } = await listing(3, 4, { size: 200 });

    expect(cardsOf(grid).every((card) => card.size === 200)).toBe(true);
    const style = part(grid, '[data-grid]').getAttribute('style') ?? '';
    expect(style).toContain('repeat(auto-fill, 200px)');
    expect(style).toContain('240px');

    grid.size = 10_000;
    await settle(grid);
    expect(cardsOf(grid).every((card) => card.size === GRID_SIZE.max)).toBe(true);

    grid.setAttribute('size', '128');
    await settle(grid);
    expect(cardsOf(grid).every((card) => card.size === 128)).toBe(true);
  });
});

describe('selection', () => {
  it('draws the ids it is given as selected', async () => {
    const { grid } = await listing(5);
    expect(cardsOf(grid).map((card) => card.selected)).toEqual([false, false, false, false, false]);

    grid.selectedIds = new Set([2, 4]);
    await settle(grid);
    expect(cardsOf(grid).map((card) => card.selected)).toEqual([false, true, false, true, false]);

    grid.selectedIds = new Set([5]);
    await settle(grid);
    expect(cardsOf(grid).map((card) => card.selected)).toEqual([false, false, false, false, true]);
  });

  it('passes the selection mode to every card', async () => {
    const { grid } = await listing(3);
    expect(cardsOf(grid).some((card) => card.selectionActive)).toBe(false);

    grid.selectionActive = true;
    await settle(grid);
    expect(cardsOf(grid).every((card) => card.selectionActive)).toBe(true);
  });

  it('reads selection-active from its attribute', async () => {
    stubServer({ total: 2 });
    const grid = document.createElement('apm-file-grid');
    grid.setAttribute('selection-active', '');
    await mount(grid, { request });
    await settle(grid);

    expect(grid.selectionActive).toBe(true);
    expect(cardsOf(grid).every((card) => card.selectionActive)).toBe(true);
  });

  it('keeps no selection of its own: a click only reports', async () => {
    const { grid } = await listing(3);
    const [first] = cardsOf(grid);
    if (!first) throw new Error('no card');

    click(optionOf(first));
    await settle(grid);

    expect(grid.selectedIds.size).toBe(0);
    expect(cardsOf(grid).some((card) => card.selected)).toBe(false);
  });

  it('lets the card events through, with the index of the card', async () => {
    const { grid } = await listing(5);
    const seen: CardActivateDetail[] = [];
    grid.addEventListener('card-activate', (event) => seen.push(event.detail));

    click(optionOf(cardsOf(grid)[3] as ApmFileCard));
    click(part(cardsOf(grid)[1] as ApmFileCard, '[data-checkbox]'), { shiftKey: true });

    expect(seen).toEqual([
      { id: 4, index: 3, via: 'card', shift: false, toggle: false },
      { id: 2, index: 1, via: 'checkbox', shift: true, toggle: false },
    ]);
  });

  it('draws a ghost that cannot be activated', async () => {
    stubServer({
      total: 5,
      item: (index) => fileItem({ id: index + 1, overlay: { planned: index === 4 } }),
    });
    const grid = await mount('apm-file-grid', { request });
    await settle(grid);
    const seen: CardActivateDetail[] = [];
    grid.addEventListener('card-activate', (event) => seen.push(event.detail));

    click(optionOf(cardsOf(grid)[4] as ApmFileCard));
    expect(seen).toEqual([]);
    click(optionOf(cardsOf(grid)[0] as ApmFileCard));
    expect(seen).toHaveLength(1);
  });
});

describe('keyboard', () => {
  it('has one tab stop, the first card, and the list itself is not one', async () => {
    const { grid } = await listing(10);

    expect(tabStops(grid)).toEqual([0]);
    expect(listboxOf(grid).hasAttribute('tabindex')).toBe(false);
  });

  // Ten items, four to a row; a 624 px tall view shows three rows, so a page moves two.
  it.each([
    ['ArrowRight', 0, 1],
    ['ArrowLeft', 5, 4],
    ['ArrowDown', 1, 5],
    ['ArrowUp', 5, 1],
    ['ArrowDown', 6, 9],
    ['PageDown', 0, 8],
    ['PageUp', 9, 1],
    ['Home', 5, 0],
    ['End', 5, 9],
  ])('%s from card %s moves the focus to card %s', async (key, start, expected) => {
    vi.stubGlobal('innerHeight', 624);
    const { grid } = await listing(10);
    grid.focusItem(start);
    await settle(grid);

    const event = press(optionOf(cardsOf(grid)[start] as ApmFileCard), key);
    await settle(grid);

    expect(event.defaultPrevented).toBe(true);
    expect(activeIndex(grid)).toBe(expected);
    expect(tabStops(grid)).toEqual([expected]);
  });

  it('scrolls the card to which it moves into view, and focuses it without scrolling again', async () => {
    const scroll = vi.spyOn(Element.prototype, 'scrollIntoView').mockImplementation(() => {});
    const { grid } = await listing(10);
    const target = cardsOf(grid)[1] as ApmFileCard;
    const focus = vi.spyOn(target, 'focus');
    scroll.mockClear();

    press(optionOf(cardsOf(grid)[0] as ApmFileCard), 'ArrowRight');

    expect(scroll).toHaveBeenCalledTimes(1);
    expect(scroll).toHaveBeenCalledWith({ block: 'nearest', inline: 'nearest' });
    expect(scroll.mock.contexts[0]).toBe(target);
    expect(focus).toHaveBeenCalledWith({ preventScroll: true });
  });

  it('keeps a Shift press for moving: selecting is up to the owner', async () => {
    const { grid } = await listing(10);
    const seen: CardActivateDetail[] = [];
    grid.addEventListener('card-activate', (event) => seen.push(event.detail));

    const event = press(optionOf(cardsOf(grid)[0] as ApmFileCard), 'ArrowRight', {
      shiftKey: true,
    });
    await settle(grid);

    expect(event.defaultPrevented).toBe(true);
    expect(activeIndex(grid)).toBe(1);
    expect(seen).toEqual([]);
  });

  it.each([
    ['Ctrl', { ctrlKey: true }],
    ['Cmd', { metaKey: true }],
    ['Alt', { altKey: true }],
  ])('leaves %s plus an arrow to the browser', async (_name, modifiers) => {
    const { grid } = await listing(10);

    const event = press(optionOf(cardsOf(grid)[0] as ApmFileCard), 'ArrowRight', modifiers);
    await settle(grid);

    expect(event.defaultPrevented).toBe(false);
    expect(tabStops(grid)).toEqual([0]);
  });

  it('leaves the keys it does not handle, and the keys something else handled', async () => {
    const { grid } = await listing(10);
    const first = optionOf(cardsOf(grid)[0] as ApmFileCard);

    expect(press(first, 'a').defaultPrevented).toBe(false);
    expect(press(first, 'Tab').defaultPrevented).toBe(false);

    first.addEventListener('keydown', (event) => event.preventDefault());
    press(first, 'ArrowRight');
    await settle(grid);
    expect(tabStops(grid)).toEqual([0]);
  });

  it('does nothing until there are items', async () => {
    stubServer();
    const grid = await mount('apm-file-grid');
    const first = optionOf(cardsOf(grid)[0] as ApmFileCard);

    expect(press(first, 'ArrowRight').defaultPrevented).toBe(false);
  });

  it('follows the card that took the focus another way, such as a click', async () => {
    const { grid } = await listing(10);

    optionOf(cardsOf(grid)[3] as ApmFileCard).focus();
    await settle(grid);

    expect(tabStops(grid)).toEqual([3]);
    press(optionOf(cardsOf(grid)[3] as ApmFileCard), 'ArrowRight');
    await settle(grid);
    expect(activeIndex(grid)).toBe(4);
  });

  it('gives a mouse press with Shift to the card, so that no text gets selected', async () => {
    const { grid } = await listing(10);
    const target = optionOf(cardsOf(grid)[2] as ApmFileCard);

    const press = new MouseEvent('mousedown', {
      bubbles: true,
      cancelable: true,
      shiftKey: true,
      button: 0,
    });
    target.dispatchEvent(press);

    expect(press.defaultPrevented).toBe(true);
    expect(activeIndex(grid)).toBe(2);
  });

  it('leaves other mouse presses alone', async () => {
    const { grid } = await listing(10);
    const target = optionOf(cardsOf(grid)[2] as ApmFileCard);

    const plain = new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0 });
    const secondary = new MouseEvent('mousedown', {
      bubbles: true,
      cancelable: true,
      shiftKey: true,
      button: 2,
    });
    target.dispatchEvent(plain);
    target.dispatchEvent(secondary);

    expect(plain.defaultPrevented).toBe(false);
    expect(secondary.defaultPrevented).toBe(false);
  });
});

describe('itemAt', () => {
  it('gives the loaded item at an index, and nothing for a page that is not in', async () => {
    const { grid } = await listing(5);

    expect(grid.itemAt(0)?.name).toBe('IMG_0.jpg');
    expect(grid.itemAt(4)?.id).toBe(5);
    expect(grid.itemAt(5)).toBeUndefined();
    expect(grid.itemAt(-1)).toBeUndefined();
  });

  it('has nothing before the first page, and nothing once the grid left the page', async () => {
    const server = stubServer({ total: 3 });
    server.hold();
    const grid = await mount('apm-file-grid', { request });
    expect(grid.itemAt(0)).toBeUndefined();

    server.release();
    await settle(grid);
    expect(grid.itemAt(0)?.name).toBe('IMG_0.jpg');

    grid.remove();
    expect(grid.itemAt(0)).toBeUndefined();
    expect(document.createElement('apm-file-grid').itemAt(0)).toBeUndefined();
  });
});

describe('focusItem', () => {
  it('clamps the index to the listing', async () => {
    const { grid } = await listing(5);

    grid.focusItem(99);
    await settle(grid);
    expect(activeIndex(grid)).toBe(4);

    grid.focusItem(-3);
    await settle(grid);
    expect(activeIndex(grid)).toBe(0);

    grid.focusItem(2.7);
    await settle(grid);
    expect(activeIndex(grid)).toBe(2);
    expect(tabStops(grid)).toEqual([2]);
  });

  it('ignores an index that is not a number', async () => {
    const { grid } = await listing(5);
    grid.focusItem(3);
    await settle(grid);

    grid.focusItem(Number.NaN);
    await settle(grid);

    expect(tabStops(grid)).toEqual([3]);
  });

  it('does nothing while there is nothing to focus', async () => {
    stubServer();
    const grid = await mount('apm-file-grid');

    expect(() => grid.focusItem(3)).not.toThrow();
    expect(() => grid.focus()).not.toThrow();
    expect(document.activeElement).not.toBe(optionOf(cardsOf(grid)[0] as ApmFileCard));

    const lonely = document.createElement('apm-file-grid');
    expect(() => lonely.focusItem(0)).not.toThrow();
  });

  it('is what focus() does, on the card that is the tab stop', async () => {
    const { grid } = await listing(5);
    grid.focusItem(3);
    await settle(grid);
    (document.activeElement as HTMLElement).blur();

    grid.focus();

    expect(activeIndex(grid)).toBe(3);
  });

  it('hands the focus the list receives to the current card', async () => {
    const { grid } = await listing(5);
    grid.focusItem(2);
    await settle(grid);
    (document.activeElement as HTMLElement).blur();

    listboxOf(grid).dispatchEvent(new FocusEvent('focus'));

    expect(activeIndex(grid)).toBe(2);
  });

  it('does not react to the focus of what is inside the list', async () => {
    const { grid } = await listing(5);
    grid.focusItem(2);
    await settle(grid);
    (document.activeElement as HTMLElement).blur();

    const inside = part(cardsOf(grid)[0] as ApmFileCard, '[data-checkbox]');
    inside.dispatchEvent(new FocusEvent('focus', { bubbles: true }));

    expect(activeIndex(grid)).toBe(-1);
  });
});

describe('changing the listing', () => {
  it('starts over, with the first card focusable, when the sort changes', async () => {
    const { grid, server } = await listing(5);
    grid.focusItem(3);
    await settle(grid);

    grid.sort = 'date';
    await settle(grid);

    expect(server.bodies.map((body) => body.sort)).toEqual(['name', 'date']);
    expect(tabStops(grid)).toEqual([0]);
  });

  it('starts over when the order or the folder changes', async () => {
    const { grid, server } = await listing(5);

    grid.order = 'desc';
    await settle(grid);
    grid.request = { ...request, dir: 'other' };
    await settle(grid);

    expect(server.bodies.map((body) => [body.dir, body.order])).toEqual([
      ['trip', 'asc'],
      ['trip', 'desc'],
      ['other', 'desc'],
    ]);
  });

  it('shows placeholders while the next listing loads', async () => {
    const { grid, server } = await listing(5);
    server.hold();

    grid.request = { ...request, dir: 'other' };
    await grid.updateComplete;

    expect(cardsOf(grid)).toHaveLength(24);
    expect(cardsOf(grid).every((card) => card.item === undefined)).toBe(true);
    server.release();
    await settle(grid);
    expect(namesOf(grid)).toHaveLength(5);
  });

  it('keeps the listing and the focus for the same request in a new object', async () => {
    const { grid, server } = await listing(5);
    grid.focusItem(3);
    await settle(grid);

    grid.request = { ...request, filter: { ...request.filter } };
    await settle(grid);

    expect(server.bodies).toHaveLength(1);
    expect(tabStops(grid)).toEqual([3]);
  });

  it('goes back to placeholders when the request is taken away', async () => {
    const { grid, server } = await listing(5);

    grid.request = undefined;
    await settle(grid);

    expect(cardsOf(grid)).toHaveLength(24);
    expect(cardsOf(grid).every((card) => card.item === undefined)).toBe(true);
    expect(server.bodies).toHaveLength(1);

    grid.request = request;
    await settle(grid);
    expect(namesOf(grid)).toHaveLength(5);
    expect(server.bodies).toHaveLength(2);
  });

  it('moves the focus to the last card when the listing shrinks below it', async () => {
    const { grid, server } = await listing(5);
    grid.focusItem(4);
    await settle(grid);
    server.state.total = 2;

    await changeOnDisk('trip');
    await settle(grid);

    expect(cardsOf(grid)).toHaveLength(2);
    expect(tabStops(grid)).toEqual([1]);
  });
});

describe('grid-state', () => {
  it('reports what the listing is doing, once per change', async () => {
    const server = stubServer({ total: 4 });
    server.hold();
    const grid = document.createElement('apm-file-grid');
    const states: GridState[] = [];
    grid.addEventListener('grid-state', (event) => states.push(event.detail));

    await mount(grid, { request });
    expect(states).toEqual([{ total: null, loading: true, error: null }]);

    server.release();
    await settle(grid);
    expect(states).toEqual([
      { total: null, loading: true, error: null },
      { total: 4, loading: false, error: null },
    ]);

    grid.selectionActive = true;
    grid.size = 200;
    await settle(grid);
    expect(states).toHaveLength(2);
  });

  it('bubbles, and tells about a failure', async () => {
    stubServer({ failure: 'No luck.' });
    const states: GridState[] = [];
    document.body.addEventListener('grid-state', (event) => states.push(event.detail));
    const grid = await mount('apm-file-grid', { request });
    await settle(grid);

    expect(states.at(-1)).toEqual({ total: null, loading: false, error: 'No luck.' });
  });

  it('starts with the empty state when there is no request', async () => {
    stubServer();
    const grid = document.createElement('apm-file-grid');
    const states: GridState[] = [];
    grid.addEventListener('grid-state', (event) => states.push(event.detail));

    await mount(grid);

    expect(states).toEqual([{ total: null, loading: false, error: null }]);
  });
});

describe('when it leaves the page', () => {
  it('stops listening to the server, and starts over when it comes back', async () => {
    const { grid, server } = await listing(3);

    grid.remove();
    await changeOnDisk('trip');
    expect(server.bodies).toHaveLength(1);

    vi.useRealTimers();
    document.body.append(grid);
    await settle(grid);

    expect(server.bodies).toHaveLength(2);
    expect(namesOf(grid)).toHaveLength(3);
    expect(tabStops(grid)).toEqual([0]);
  });

  it('drops the listing it had, so the placeholders show first', async () => {
    const { grid, server } = await listing(3);
    grid.remove();
    server.hold();

    document.body.append(grid);
    await grid.updateComplete;

    expect(cardsOf(grid)).toHaveLength(24);
    expect(cardsOf(grid).every((card) => card.item === undefined)).toBe(true);
    server.release();
    await settle(grid);
  });
});
