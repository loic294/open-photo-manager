import type { RangeChangedEvent } from '@lit-labs/virtualizer/events.js';
import { grid } from '@lit-labs/virtualizer/layouts/grid.js';
import { virtualize } from '@lit-labs/virtualizer/virtualize.js';
import { html, nothing, type PropertyValues, type TemplateResult } from 'lit';
import { repeat } from 'lit/directives/repeat.js';
import { type StyleInfo, styleMap } from 'lit/directives/style-map.js';
import { ITEMS_PAGE_SIZE } from '@/api/files';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FileItem } from '@/api/generated/FileItem';
import type { SortKey } from '@/api/generated/SortKey';
import type { SortOrder } from '@/api/generated/SortOrder';
import { ApmElement } from '@/base/apm-element';
import { type ApmFileCard, cardHeight } from '@/components/apm-file-card';
import '@/components/apm-icon';
import { PagedItems } from '@/stores/paged-items';
import { GRID_SIZE } from '@/stores/prefs';

/** Space between the cards in px: the same in the plain grid and in the virtualiser's layout. */
export const GRID_GAP_PX = 8;
/** A listing of at most this many items is a plain CSS grid; a longer one is virtualised. */
export const VIRTUALIZE_ABOVE = ITEMS_PAGE_SIZE;
/** Placeholder cards shown until the first page of a listing arrives. */
const SKELETON_COUNT = 24;
/** Animation frames to wait for a card to render after scrolling to it. */
const REVEAL_FRAMES = 30;

const NO_IDS: ReadonlySet<number> = new Set<number>();
const SKELETON_INDEXES = Array.from({ length: SKELETON_COUNT }, (_, index) => index);

export interface GridState {
  /** `null` until the first page of the listing arrived. */
  total: number | null;
  /** A page that is not cached yet is being fetched. */
  loading: boolean;
  /** Message of the last failed fetch. */
  error: string | null;
}

export type GridStateEvent = CustomEvent<GridState>;

declare global {
  interface GlobalEventHandlersEventMap {
    'grid-state': GridStateEvent;
  }
}

/** The card size the grid uses: a whole number inside `GRID_SIZE`, the default if not a number. */
export function normalizeGridSize(size: number): number {
  if (!Number.isFinite(size)) return GRID_SIZE.default;
  return Math.round(Math.min(GRID_SIZE.max, Math.max(GRID_SIZE.min, size)));
}

/** How many cards of `size` fit a row `width` px wide (the same rule as the virtualiser's grid). */
export function columnsFor(width: number, size: number): number {
  return Math.max(1, Math.floor((width + GRID_GAP_PX) / (size + GRID_GAP_PX)));
}

/** `value` rounded down, but at least `min`; `min` for anything that is not a number. */
function wholeAtLeast(value: number, min: number): number {
  return Number.isFinite(value) ? Math.max(min, Math.floor(value)) : min;
}

/** The index `rows` rows away in the same column: it stays in the column unless the row is short. */
function moveRows(from: number, rows: number, columns: number, last: number): number {
  const target = from + rows * columns;
  if (target < 0) return from % columns;
  if (target <= last) return target;
  const sameColumn = Math.floor(last / columns) * columns + (from % columns);
  return Math.min(sameColumn, last);
}

/**
 * Where an arrow, Home, End, PageUp or PageDown key moves the focus, or `null` for any other key.
 * Left and Right step one card, Up and Down one row, a page is `pageRows - 1` rows (at least one),
 * Home and End are the first and the last card of the whole listing. Up from the first row and
 * Down from the last stay put; Down into a short last row goes to its last card.
 */
export function gridFocusTarget(
  key: string,
  current: number,
  total: number,
  columns: number,
  pageRows: number,
): number | null {
  if (total <= 0) return null;
  const last = total - 1;
  const cols = wholeAtLeast(columns, 1);
  const from = Math.min(Math.max(0, current), last);
  const page = wholeAtLeast(pageRows - 1, 1);
  switch (key) {
    case 'ArrowLeft':
      return Math.max(0, from - 1);
    case 'ArrowRight':
      return Math.min(last, from + 1);
    case 'ArrowUp':
      return moveRows(from, -1, cols, last);
    case 'ArrowDown':
      return moveRows(from, 1, cols, last);
    case 'PageUp':
      return moveRows(from, -page, cols, last);
    case 'PageDown':
      return moveRows(from, page, cols, last);
    case 'Home':
      return 0;
    case 'End':
      return last;
    default:
      return null;
  }
}

/**
 * The distance to scroll to bring a box into a view with the least movement: `top` is the box's
 * offset from the top of the view, negative or positive. 0 when it is already in.
 */
export function revealDelta(top: number, height: number, viewHeight: number): number {
  if (top < 0) return top;
  if (top + height > viewHeight) return Math.min(top, top + height - viewHeight);
  return 0;
}

interface View {
  scroller: Element;
  /** Top edge of the visible area in viewport coordinates. */
  top: number;
  height: number;
}

/** The nearest ancestor that scrolls, else the page: the area `element` is seen through. */
function viewOf(element: Element): View {
  for (let parent = element.parentElement; parent; parent = parent.parentElement) {
    if (/auto|scroll|overlay/.test(getComputedStyle(parent).overflowY)) {
      return {
        scroller: parent,
        top: parent.getBoundingClientRect().top + parent.clientTop,
        height: parent.clientHeight,
      };
    }
  }
  return {
    scroller: document.scrollingElement ?? document.documentElement,
    top: 0,
    height: window.innerHeight,
  };
}

/**
 * The file grid of a pane: a listing of any length as cards (`<apm-file-card>`), read page by page
 * from the server (`PagedItems`) and rendered by a virtualiser once it is longer than a page.
 *
 *   <apm-file-grid .request=${request} sort="name" order="asc" size="160"
 *     .selectedIds=${selecting.selectedIds} ?selection-active=${selecting.active}></apm-file-grid>
 *
 * Properties: `request` (`undefined` shows placeholders), `sort`, `order`, `size` (card width in
 * px, clamped to `GRID_SIZE` and to the width of the grid, so a pane narrower than a card still
 * shows one column), `selectedIds` (a NEW set on every change), `selectionActive`
 * (checkboxes on every card), `emptyText` and `label` (the accessible name of the list). A change
 * of `request`, `sort` or `order` starts a new listing; the same values in a new object do not.
 * The grid keeps no selection: it only draws `selectedIds` and the cards report clicks.
 *
 * Events, both bubbling: `card-activate` (from the cards, with the item's `id` and `index`) and
 * `grid-state` (`GridState`: `total`, `loading`, `error`, whenever one of them changes). Read-only:
 * `columns` and `itemAt(index)` (the loaded item, for the viewer's `startItem`).
 *
 * The grid is not a scroller: it grows to the height of the whole listing and the page scrolls
 * it, so put it inside the pane body (`overflow-y-auto`), below anything else that scrolls with it.
 *
 * Keyboard: the list is one tab stop (roving tabindex). Arrows move the focus by card and row,
 * Home and End to the first and last card, PageUp and PageDown by a page; Shift only moves the
 * focus (selecting is the owner's business). Space and Enter belong to the card.
 */
export class ApmFileGrid extends ApmElement {
  static override properties = {
    request: { attribute: false },
    sort: { type: String },
    order: { type: String },
    size: { type: Number },
    selectedIds: { attribute: false },
    selectionActive: { type: Boolean, attribute: 'selection-active' },
    emptyText: { type: String, attribute: 'empty-text' },
    label: { type: String },
    focusIndex: { state: true },
  };

  declare request: BrowseRequest | undefined;
  declare sort: SortKey;
  declare order: SortOrder;
  declare size: number;
  declare selectedIds: ReadonlySet<number>;
  declare selectionActive: boolean;
  declare emptyText: string;
  declare label: string;
  /** The card that is the tab stop and takes the arrow keys. */
  declare focusIndex: number;

  private store: PagedItems | undefined;
  /** The store is new: it has to be given the listing. */
  private fresh = true;
  /** The store was given a listing. */
  private listed = false;
  private columnCount = 1;
  /** The widest a card may be: the grid's own width when that is below `GRID_SIZE.max`. */
  private fitWidth = Number.POSITIVE_INFINITY;
  /** One slot per item, only there to give the virtualiser a length. */
  private slots: unknown[] = [];
  private resizeObserver: ResizeObserver | undefined;
  private observed: HTMLElement | undefined;
  private lastState: GridState | undefined;
  /** The range the virtualiser renders. */
  private rendered = { first: -1, last: -1 };
  /** Whether the last render left the tab stop on a card (else on the list itself). */
  private cardHasTabStop = true;
  /** The last render was a virtualised listing. */
  private virtualList = false;
  /** The grid element the virtualiser may start in: one that is already part of the page. */
  private armed: HTMLElement | undefined;
  private revealToken = 0;

  constructor() {
    super();
    this.request = undefined;
    this.sort = 'name';
    this.order = 'asc';
    this.size = GRID_SIZE.default;
    this.selectedIds = NO_IDS;
    this.selectionActive = false;
    this.emptyText = 'No files';
    this.label = 'Files';
    this.focusIndex = 0;
  }

  /** Cards per row, as laid out now (1 until the grid has a width). Read-only. */
  get columns(): number {
    return this.columnCount;
  }

  /**
   * The item at `index` of the listing, `undefined` while its page has not arrived: what the
   * viewer takes as its `startItem`, so it shows the file at once.
   */
  itemAt(index: number): FileItem | undefined {
    return this.store?.get(index);
  }

  /** Fetches what is on screen again, now. The shown items stay until the answer arrives. */
  refresh(): void {
    this.store?.invalidate();
  }

  /** The listing's store: made when first needed, dropped when the grid leaves the page. */
  private get paged(): PagedItems {
    this.store ??= new PagedItems();
    return this.store;
  }

  override connectedCallback(): void {
    this.fresh = true;
    this.listed = false;
    this.lastState = undefined;
    if (typeof ResizeObserver !== 'undefined') {
      this.resizeObserver = new ResizeObserver(() => this.measure());
    }
    super.connectedCallback();
    this.classList.add('block', 'w-full');
    this.requestUpdate();
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.store?.dispose();
    this.store = undefined;
    this.resizeObserver?.disconnect();
    this.resizeObserver = undefined;
    this.observed = undefined;
    this.armed = undefined;
    this.revealToken += 1;
  }

  /** Focuses the card that is the tab stop, scrolling to it. */
  override focus(): void {
    this.focusItem(this.focusIndex);
  }

  /**
   * Moves the focus to the card at `index` (clamped to the listing) and scrolls it into view. A
   * card that is not rendered yet is scrolled to first and focused as soon as it exists.
   */
  focusItem(index: number): void {
    const total = this.store?.total.peek() ?? null;
    if (total === null || total === 0 || Number.isNaN(index)) return;
    const target = Math.min(Math.max(0, Math.trunc(index)), total - 1);
    this.focusIndex = target;
    const token = ++this.revealToken;
    if (!this.reveal(target)) {
      this.scrollToRow(target);
      this.focusWhenRendered(target, token, REVEAL_FRAMES);
    }
  }

  protected override willUpdate(changed: PropertyValues): void {
    if (this.fresh || changed.has('request') || changed.has('sort') || changed.has('order')) {
      this.fresh = false;
      this.syncListing();
    }
    const total = this.paged.total.peek();
    if (total !== null && this.focusIndex >= total) this.focusIndex = Math.max(0, total - 1);
  }

  protected override updated(): void {
    this.watchGrid();
    this.measure();
    this.armVirtualiser();
    this.announce();
  }

  /**
   * `virtualize()` looks for the ancestors that scroll when it starts, so its host has to be in
   * the page by then, but a template renders into a fragment that is not. The first render of a
   * virtualised listing therefore only makes the host; this starts the virtualiser in it.
   */
  private armVirtualiser(): void {
    const element = this.virtualList ? this.gridElement() : null;
    if (!element) {
      this.armed = undefined;
      return;
    }
    if (element === this.armed) return;
    this.armed = element;
    queueMicrotask(() => {
      if (this.isConnected && this.armed === element) this.requestUpdate();
    });
  }

  private syncListing(): void {
    const { request } = this;
    if (request === undefined) {
      if (this.listed) {
        this.store?.dispose();
        this.store = undefined;
        this.listed = false;
        this.focusIndex = 0;
      }
      return;
    }
    this.listed = true;
    if (this.paged.reset(request, this.sort, this.order)) this.focusIndex = 0;
  }

  /** The card width in use: `size`, but never wider than the grid, so one column always fits. */
  private get cell(): number {
    return Math.min(normalizeGridSize(this.size), this.fitWidth);
  }

  private gridElement(): HTMLElement | null {
    return this.querySelector<HTMLElement>('[data-grid]');
  }

  private cardAt(index: number): ApmFileCard | null {
    return this.querySelector<ApmFileCard>(`apm-file-card[data-index="${index}"]`);
  }

  private cardOf(target: EventTarget | null): ApmFileCard | null {
    return target instanceof Element ? target.closest<ApmFileCard>('apm-file-card') : null;
  }

  /** Follows the element that is the grid, which changes with the state of the listing. */
  private watchGrid(): void {
    const element = this.gridElement() ?? undefined;
    if (element === this.observed) return;
    if (this.observed) this.resizeObserver?.unobserve(this.observed);
    this.observed = element;
    if (element) this.resizeObserver?.observe(element);
  }

  private measure(): void {
    const width = this.observed?.clientWidth ?? 0;
    if (width <= 0) return;
    // The virtualiser renders no card in a grid narrower than a card. Only a grid below the widest
    // card needs the limit, so resizing a wide grid never renders again.
    const fit = width < GRID_SIZE.max ? width : Number.POSITIVE_INFINITY;
    if (fit !== this.fitWidth) {
      this.fitWidth = fit;
      queueMicrotask(() => {
        if (this.isConnected) this.requestUpdate();
      });
    }
    this.columnCount = columnsFor(width, this.cell);
  }

  private announce(): void {
    const { paged } = this;
    const state: GridState = {
      total: paged.total.peek(),
      loading: paged.loading.peek(),
      error: paged.error.peek(),
    };
    const last = this.lastState;
    if (
      last &&
      last.total === state.total &&
      last.loading === state.loading &&
      last.error === state.error
    ) {
      return;
    }
    this.lastState = state;
    this.dispatchEvent(new CustomEvent<GridState>('grid-state', { bubbles: true, detail: state }));
  }

  /** Scrolls a rendered card into view and focuses it; false when the card is not rendered. */
  private reveal(index: number): boolean {
    const card = this.cardAt(index);
    if (!card) return false;
    // `focus()` would centre a card that is out of view; "nearest" moves the page the least.
    card.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    card.focus({ preventScroll: true });
    return true;
  }

  /** The cards of a virtualised listing only exist near the view: scroll the row of `index` in. */
  private scrollToRow(index: number): void {
    const element = this.gridElement();
    if (!element) return;
    const view = viewOf(element);
    const height = cardHeight(this.cell);
    const row = Math.floor(index / this.columns);
    const top = element.getBoundingClientRect().top - view.top + row * (height + GRID_GAP_PX);
    const delta = revealDelta(top, height, view.height);
    if (delta !== 0) view.scroller.scrollBy({ top: delta, behavior: 'instant' });
  }

  private focusWhenRendered(index: number, token: number, framesLeft: number): void {
    window.requestAnimationFrame(() => {
      if (token !== this.revealToken || !this.isConnected) return;
      if (this.reveal(index)) return;
      if (framesLeft > 0) this.focusWhenRendered(index, token, framesLeft - 1);
    });
  }

  private pageRows(): number {
    const element = this.gridElement();
    const height = element ? viewOf(element).height : window.innerHeight;
    return Math.max(1, Math.floor(height / (cardHeight(this.cell) + GRID_GAP_PX)));
  }

  private focusIsRendered(): boolean {
    return this.focusIndex >= this.rendered.first && this.focusIndex <= this.rendered.last;
  }

  private onKeydown(event: KeyboardEvent): void {
    if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) return;
    const total = this.paged.total.peek();
    if (total === null) return;
    const target = gridFocusTarget(
      event.key,
      this.focusIndex,
      total,
      this.columns,
      this.pageRows(),
    );
    if (target === null) return;
    event.preventDefault();
    this.focusItem(target);
  }

  private onFocusin(event: FocusEvent): void {
    const card = this.cardOf(event.target);
    if (card && card.index >= 0 && card.index !== this.focusIndex) this.focusIndex = card.index;
  }

  private onListFocus(event: FocusEvent): void {
    if (event.target === event.currentTarget) this.focusItem(this.focusIndex);
  }

  /** Shift-click must not select text; stopping that also stops the focus, so give it here. */
  private onMousedown(event: MouseEvent): void {
    if (!event.shiftKey || event.button !== 0) return;
    event.preventDefault();
    this.cardOf(event.target)?.focus({ preventScroll: true });
  }

  private onRangeChanged(event: RangeChangedEvent): void {
    this.rendered = { first: event.first, last: event.last };
    if (this.focusIsRendered() !== this.cardHasTabStop) this.requestUpdate();
  }

  private onRetry(): void {
    this.paged.retry();
  }

  private renderCell(index: number, total: number): TemplateResult {
    const item = this.paged.get(index);
    return html`<apm-file-card
      data-index=${index}
      .item=${item}
      .index=${index}
      .size=${this.cell}
      .setSize=${total}
      .selected=${item !== undefined && this.selectedIds.has(item.id)}
      .selectionActive=${this.selectionActive}
      .focused=${index === this.focusIndex}
    ></apm-file-card>`;
  }

  /** The plain grid: as many columns of the card width as fit, centred, rows of one card. */
  private plainStyle(): StyleInfo {
    const size = this.cell;
    return {
      gap: `${GRID_GAP_PX}px`,
      'grid-template-columns': `repeat(auto-fill, ${size}px)`,
      'grid-auto-rows': `${cardHeight(size)}px`,
    };
  }

  private renderPlain(total: number): TemplateResult {
    return html`<div data-grid class="grid justify-center" style=${styleMap(this.plainStyle())}>${repeat(
      this.slots,
      (_slot, index) => index,
      (_slot, index) => this.renderCell(index, total),
    )}</div>`;
  }

  /** The host in the page is the one the virtualiser may start in. */
  private virtualiserStarted(): boolean {
    return this.armed !== undefined && this.armed === this.gridElement();
  }

  private renderVirtualised(total: number): TemplateResult {
    const size = this.cell;
    return html`<div data-grid @rangeChanged=${this.onRangeChanged}>${
      this.virtualiserStarted()
        ? virtualize({
            items: this.slots,
            renderItem: (_slot, index) => this.renderCell(index, total),
            keyFunction: (_slot, index) => index,
            layout: grid({
              itemSize: { width: `${size}px`, height: `${cardHeight(size)}px` },
              gap: `${GRID_GAP_PX}px`,
              padding: '0px',
              justify: 'center',
            }),
            scroller: false,
          })
        : nothing
    }</div>`;
  }

  private renderSkeletons(): TemplateResult {
    const size = this.cell;
    return html`
      <div role="listbox" aria-label=${this.label} aria-busy="true" class="p-2 select-none">
        <div data-grid class="grid justify-center" style=${styleMap(this.plainStyle())}>${repeat(
          SKELETON_INDEXES,
          (index) => index,
          (index) =>
            html`<apm-file-card data-index=${index} .index=${index} .size=${size}></apm-file-card>`,
        )}</div>
      </div>
    `;
  }

  private renderListbox(total: number, loading: boolean): TemplateResult {
    const virtualised = total > VIRTUALIZE_ABOVE;
    this.virtualList = virtualised;
    // Until the virtualiser starts there are no cards, so no range to keep the tab stop on.
    if (virtualised && !this.virtualiserStarted()) this.rendered = { first: -1, last: -1 };
    this.cardHasTabStop = !virtualised || this.focusIsRendered();
    return html`
      <div
        role="listbox"
        aria-label=${this.label}
        aria-multiselectable="true"
        aria-busy=${loading ? 'true' : nothing}
        tabindex=${this.cardHasTabStop ? nothing : '0'}
        class="p-2 select-none"
        @keydown=${this.onKeydown}
        @mousedown=${this.onMousedown}
        @focusin=${this.onFocusin}
        @focus=${this.onListFocus}
      >
        ${virtualised ? this.renderVirtualised(total) : this.renderPlain(total)}
      </div>
    `;
  }

  private renderError(message: string): TemplateResult {
    return html`
      <div role="alert" class="alert alert-error shadow-lg">
        <apm-icon name="circle-x"></apm-icon>
        <span class="min-w-0 break-words">${message}</span>
        <button type="button" class="btn btn-sm" @click=${this.onRetry}>Retry</button>
      </div>
    `;
  }

  protected override render(): TemplateResult {
    const { paged } = this;
    const total = paged.total.value;
    const error = paged.error.value;
    const loading = paged.loading.value;
    // Reading the version subscribes the element: a bump means cached items changed, so the cells
    // render again and read them.
    void paged.version.value;
    this.virtualList = false;

    if (total === null) {
      return error === null
        ? this.renderSkeletons()
        : html`<div class="flex justify-center p-8">${this.renderError(error)}</div>`;
    }
    if (this.slots.length !== total) this.slots = new Array<unknown>(total);
    const banner =
      error === null
        ? nothing
        : html`<div class="sticky top-0 z-10 p-2 pb-0">${this.renderError(error)}</div>`;
    if (total === 0) {
      return html`${banner}<p role="status" class="px-2 py-16 text-center text-base-content/60">${this.emptyText}</p>`;
    }
    return html`${banner}${this.renderListbox(total, loading)}`;
  }
}

customElements.define('apm-file-grid', ApmFileGrid);

declare global {
  interface HTMLElementTagNameMap {
    'apm-file-grid': ApmFileGrid;
  }
}
