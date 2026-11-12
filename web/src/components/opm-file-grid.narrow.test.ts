import { afterEach, describe, expect, it, vi } from 'vitest';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FileItem } from '@/api/generated/FileItem';
import type { ItemsRequest } from '@/api/generated/ItemsRequest';
import { events } from '@/api/sse';
import { cardHeight, type OpmFileCard } from '@/components/opm-file-card';
import { type OpmFileGrid, VIRTUALIZE_ABOVE } from '@/components/opm-file-grid';
import { GRID_SIZE } from '@/stores/prefs';
import { fileItem } from '@/test/file-item';
import { mount } from '@/test/helpers';

vi.mock('@lit-labs/virtualizer/virtualize.js', () => ({ virtualize: vi.fn(() => '') }));
const { virtualize } = await import('@lit-labs/virtualizer/virtualize.js');

afterEach(() => {
  events.stop();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.mocked(virtualize).mockClear();
});

const request: BrowseRequest = {
  volume_id: 1,
  dir: 'trip',
  filter: { cameras: [], ratings: [], flags: [], tags: [], selection_id: null },
  plan: null,
};

/** Answers `POST /api/browse/items` with the first page of a listing of `total` files. */
function stubServer(total: number): void {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (_url: string, init: RequestInit): Promise<Response> => {
      const body = JSON.parse(String(init.body)) as ItemsRequest;
      const items: FileItem[] = [];
      const end = Math.min(total, body.offset + body.limit);
      for (let index = body.offset; index < end; index += 1) {
        items.push(fileItem({ id: index + 1, name: `IMG_${index}.jpg` }));
      }
      return new Response(JSON.stringify({ items, total, offset: body.offset }));
    }),
  );
}

/**
 * happy-dom has no layout: the grid element of the page is given `width`, and `resize` runs the
 * grid's resize observer the way a browser does after a layout.
 */
function layout(initial: number) {
  const state = { width: initial };
  const callbacks: ResizeObserverCallback[] = [];
  vi.stubGlobal(
    'ResizeObserver',
    class {
      constructor(callback: ResizeObserverCallback) {
        callbacks.push(callback);
      }
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  );
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.hasAttribute('data-grid') ? state.width : 0;
  });
  return {
    resize(width: number): void {
      state.width = width;
      for (const callback of callbacks) callback([], {} as ResizeObserver);
    },
  };
}

const cardsOf = (grid: OpmFileGrid): OpmFileCard[] => [...grid.querySelectorAll('opm-file-card')];

/** Lets the first page arrive and the grid render again after a change of its card width. */
async function settle(grid: OpmFileGrid): Promise<void> {
  for (let round = 0; round < 4; round += 1) {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    await grid.updateComplete;
    await Promise.all(cardsOf(grid).map((card) => card.updateComplete));
  }
}

async function showing(total: number, width: number, size: number) {
  stubServer(total);
  const sizes = layout(width);
  const grid = await mount('opm-file-grid', { request, size });
  await settle(grid);
  return { grid, resize: sizes.resize };
}

const cardWidths = (grid: OpmFileGrid): number[] => cardsOf(grid).map((card) => card.size);

/** The card width the virtualiser was last given, as the `px` string of its grid layout. */
function virtualisedItemSize(): { width: string; height: string } | undefined {
  const call = vi.mocked(virtualize).mock.calls.at(-1);
  const layoutConfig = call?.[0]?.layout as { itemSize?: { width: string; height: string } };
  return layoutConfig?.itemSize;
}

describe('a grid narrower than the chosen card size', () => {
  it('keeps the chosen size when the grid is wide enough', async () => {
    const { grid } = await showing(5, GRID_SIZE.max, GRID_SIZE.max);
    expect(new Set(cardWidths(grid))).toEqual(new Set([GRID_SIZE.max]));
    expect(grid.columns).toBe(1);
  });

  it('keeps the chosen size in a wide grid, however wide', async () => {
    const { grid } = await showing(5, 1200, 200);
    expect(new Set(cardWidths(grid))).toEqual(new Set([200]));
    expect(grid.columns).toBe(5);
  });

  it('shrinks the cards to the grid so that one column fits', async () => {
    const { grid } = await showing(5, 304, GRID_SIZE.max);
    expect(cardsOf(grid)).toHaveLength(5);
    expect(new Set(cardWidths(grid))).toEqual(new Set([304]));
    expect(grid.columns).toBe(1);
    const style = grid.querySelector<HTMLElement>('[data-grid]')?.getAttribute('style') ?? '';
    expect(style).toContain('repeat(auto-fill, 304px)');
    expect(style).toContain(`${cardHeight(304)}px`);
  });

  it('keeps a size that already fits a narrow grid', async () => {
    const { grid } = await showing(5, 300, GRID_SIZE.min);
    expect(new Set(cardWidths(grid))).toEqual(new Set([GRID_SIZE.min]));
    expect(grid.columns).toBe(2);
  });

  it('follows the width of the grid as it narrows and widens again', async () => {
    const { grid, resize } = await showing(5, 600, GRID_SIZE.max);
    expect(new Set(cardWidths(grid))).toEqual(new Set([GRID_SIZE.max]));

    resize(250);
    await settle(grid);
    expect(new Set(cardWidths(grid))).toEqual(new Set([250]));
    expect(grid.columns).toBe(1);

    resize(500);
    await settle(grid);
    expect(new Set(cardWidths(grid))).toEqual(new Set([GRID_SIZE.max]));
  });

  it('does not render again when a wide grid is resized', async () => {
    const { grid, resize } = await showing(5, 600, GRID_SIZE.max);
    const before = cardsOf(grid);
    resize(700);
    await settle(grid);
    expect(cardsOf(grid)).toEqual(before);
    expect(new Set(cardWidths(grid))).toEqual(new Set([GRID_SIZE.max]));
  });

  it('gives a virtualised listing the narrow width, since it draws no card in a grid narrower than one', async () => {
    const { grid } = await showing(VIRTUALIZE_ABOVE + 50, 304, GRID_SIZE.max);
    expect(grid.columns).toBe(1);
    expect(virtualisedItemSize()).toEqual({ width: '304px', height: `${cardHeight(304)}px` });
  });

  it('gives a virtualised listing the chosen size in a wide grid', async () => {
    await showing(VIRTUALIZE_ABOVE + 50, 800, GRID_SIZE.max);
    expect(virtualisedItemSize()).toEqual({
      width: `${GRID_SIZE.max}px`,
      height: `${cardHeight(GRID_SIZE.max)}px`,
    });
  });
});
