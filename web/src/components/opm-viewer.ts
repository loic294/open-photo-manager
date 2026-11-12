import { html, type TemplateResult } from 'lit';
import { keyed } from 'lit/directives/keyed.js';
import { isAbortError, isApiError } from '@/api/errors';
import { fetchFileDetail } from '@/api/files';
import type { FileDetail } from '@/api/generated/FileDetail';
import type { FileItem } from '@/api/generated/FileItem';
import { OpmElement } from '@/base/opm-element';
import '@/components/opm-modal';
import '@/components/opm-viewer-stage';
import type { StageFailure, StageSettledEvent } from '@/components/opm-viewer-stage';
import {
  type ChromeActions,
  type ChromeView,
  type DetailStatus,
  type IdsStatus,
  renderInfoPanel,
  renderStepButtons,
  renderToolbar,
} from '@/components/viewer-chrome';
import { viewerInfoOpen } from '@/components/viewer-info';
import {
  announcement,
  DETAIL_DEBOUNCE_MS,
  type Direction,
  directionAfter,
  isInMedia,
  isNavAction,
  type NavAction,
  neighbourIndex,
  OPEN_GUARD_MS,
  targetIndex,
  viewerKeyAction,
} from '@/components/viewer-nav';
import { ViewerPreloader } from '@/components/viewer-preload';

type Id = FileItem['id'];

/** What `openViewer()` pages through. */
export interface ViewerSource {
  /** The file to show first. */
  startId: Id;
  /** Its listing row, when the caller has it: it is shown at once and the server is not asked. */
  startItem?: FileItem | undefined;
  /**
   * Every file id of the pane's listing in the grid's order (real files only). Called once;
   * `signal` aborts when the viewer closes or its source is replaced.
   */
  loadIds(signal: AbortSignal): Promise<readonly Id[]>;
}

/** Details kept for stepping back; the current file's is never dropped. */
const DETAILS_MAX = 24;

const waiting: (() => void)[] = [];
let host: OpmViewer | undefined;

/**
 * Opens the photo and video viewer and resolves once it is closed. The viewer is one dialog,
 * mounted on first use; calling this while it is open swaps the source (every pending promise
 * still resolves, once, when the viewer closes). Only the tool-view framework calls it.
 *
 *   // the pane, on `item-open` from the grid (ITEM_OPEN_EVENT, detail `{ id, index }`)
 *   void openViewer({
 *     startId: event.detail.id,
 *     startItem: pagedItems.get(event.detail.index),
 *     loadIds: (signal) => fetchIds(request, sort, order, { signal }),
 *   });
 *
 * Pass the pane's CURRENT request and sort so the viewer steps through what the grid shows, in
 * the grid's order. If `startId` is not in the list, or the list cannot be loaded, the viewer
 * shows that one file without stepping and says so in a badge.
 *
 * Left, Right, Home and End step (no wrap-around), `i` toggles the info panel, `z` and a double
 * click on the picture toggle 100% zoom, Esc closes. The last file shown stays painted in the
 * closed dialog, for its fade-out, until the next `openViewer()`.
 */
export function openViewer(source: ViewerSource): Promise<void> {
  return new Promise((resolve) => {
    waiting.push(resolve);
    ensureHost().begin(source);
  });
}

function ensureHost(): OpmViewer {
  if (host?.isConnected) return host;
  host = document.createElement('opm-viewer');
  document.body.append(host);
  return host;
}

function settleAll(): void {
  for (const resolve of waiting.splice(0)) resolve();
}

function isNotFound(error: unknown): boolean {
  return isApiError(error) && (error.status === 404 || error.code === 'not_found');
}

/**
 * The view behind `openViewer()`. Not for direct use. Removing it from the page closes the
 * viewer and settles every promise `openViewer()` handed out.
 *
 * It knows the file's id, not its row, until `startItem` or the detail says more: the heading
 * and the stage wait for it (a stale answer for a file the user already left is cached and
 * otherwise ignored). The pieces: `opm-viewer-stage` shows the file, `viewer-info` the panel,
 * `viewer-nav` the keys and the labels, `viewer-preload` warms the next file.
 */
export class OpmViewer extends OpmElement {
  static override properties = {
    opened: { state: true },
    session: { state: true },
    currentId: { state: true },
    ids: { state: true },
    idsStatus: { state: true },
    index: { state: true },
    detailStatus: { state: true },
    zoomed: { state: true },
  };

  declare opened: boolean;
  /** Counts the sources: each one gets a fresh stage, so nothing of the last one leaks in. */
  declare session: number;
  declare currentId: Id;
  declare ids: readonly Id[];
  declare idsStatus: IdsStatus;
  /** Where `currentId` is in `ids`; `-1` until the list is `ready`. */
  declare index: number;
  declare detailStatus: DetailStatus;
  declare zoomed: boolean;

  private startItem: FileItem | undefined;
  private readonly details = new Map<Id, FileDetail>();
  private idsAbort: AbortController | undefined;
  private detailAbort: AbortController | undefined;
  private detailTimer: ReturnType<typeof setTimeout> | undefined;
  /** How long the next detail request waits: stepping debounces, everything else asks at once. */
  private detailDelay = 0;
  private guardTimer: ReturnType<typeof setTimeout> | undefined;
  /** Pointer actions are swallowed for `OPEN_GUARD_MS` after opening. */
  private guarded = false;
  /** The file the stage has finished showing (or failed to); the next one is preloaded then. */
  private settledId: Id | undefined;
  private lastDirection: Direction = 1;
  private focusPending = false;
  private readonly preloader = new ViewerPreloader((id, signal) => this.resolveItem(id, signal));
  /** What the chrome calls; arrow wrappers, because the chrome renders inside `opm-modal`. */
  private readonly chrome: ChromeActions = {
    toggleInfo: () => this.toggleInfo(),
    toggleZoom: () => this.toggleZoom(),
    step: (action) => this.go(action),
    retryDetail: () => this.retryDetail(),
  };

  constructor() {
    super();
    this.opened = false;
    this.session = 0;
    this.currentId = 0;
    this.ids = [];
    this.idsStatus = 'loading';
    this.index = -1;
    this.detailStatus = 'idle';
    this.zoomed = false;
    this.addEventListener('click', this.onPointerAction, { capture: true });
    this.addEventListener('dblclick', this.onPointerAction, { capture: true });
    this.addEventListener('click', this.onClick);
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.end();
  }

  /** Shows `source`: the first one opens the dialog, a later one replaces what is showing. */
  begin(source: ViewerSource): void {
    const reopening = !this.opened;
    this.abortSession();
    this.details.clear();
    this.session += 1;
    this.startItem = source.startItem;
    this.currentId = source.startId;
    this.ids = [];
    this.idsStatus = 'loading';
    this.index = -1;
    this.detailStatus = 'idle';
    this.detailDelay = 0;
    this.zoomed = false;
    this.lastDirection = 1;
    this.settledId = undefined;
    this.focusPending = reopening;
    this.opened = true;
    this.guarded = true;
    this.guardTimer = setTimeout(() => {
      this.guardTimer = undefined;
      this.guarded = false;
    }, OPEN_GUARD_MS);
    this.loadIds(source);
  }

  /**
   * Closes: abandons every request and the playback. Nothing that is painted is cleared, so the
   * dialog does not go blank while it fades out.
   */
  private end(): void {
    this.abortSession();
    this.querySelector('opm-viewer-stage')?.release();
    this.opened = false;
    settleAll();
  }

  private abortSession(): void {
    this.idsAbort?.abort();
    this.idsAbort = undefined;
    this.cancelDetail();
    clearTimeout(this.guardTimer);
    this.guardTimer = undefined;
    this.guarded = false;
    this.preloader.clear();
  }

  private loadIds(source: ViewerSource): void {
    const controller = new AbortController();
    this.idsAbort = controller;
    // Through a promise, so a `loadIds` that throws instead of rejecting is handled the same way.
    new Promise<readonly Id[]>((resolve) => resolve(source.loadIds(controller.signal))).then(
      (ids) => {
        if (!controller.signal.aborted) this.useIds(ids);
      },
      () => {
        if (!controller.signal.aborted) this.idsStatus = 'failed';
      },
    );
  }

  private useIds(ids: readonly Id[]): void {
    const index = ids.indexOf(this.currentId);
    if (index < 0) {
      this.idsStatus = 'missing';
      return;
    }
    this.ids = ids;
    this.index = index;
    this.idsStatus = 'ready';
    this.maybePreload();
  }

  private itemOf(id: Id): FileItem | undefined {
    const known = this.details.get(id)?.item;
    if (known) return known;
    return this.startItem?.id === id ? this.startItem : undefined;
  }

  /** The row to show: none when the server says the file is gone. */
  private shownItem(): FileItem | undefined {
    return this.detailStatus === 'missing' ? undefined : this.itemOf(this.currentId);
  }

  /** The detail is asked for when the row is unknown, and for the panel (the row lacks its fields). */
  private needsDetail(id: Id): boolean {
    if (this.details.has(id)) return false;
    return this.itemOf(id) === undefined || viewerInfoOpen.peek();
  }

  private remember(id: Id, detail: FileDetail): void {
    this.details.delete(id);
    this.details.set(id, detail);
    for (const key of this.details.keys()) {
      if (this.details.size <= DETAILS_MAX) break;
      if (key !== this.currentId) this.details.delete(key);
    }
    if (id === this.currentId) this.requestUpdate();
  }

  private async resolveItem(id: Id, signal: AbortSignal): Promise<FileItem> {
    const known = this.itemOf(id);
    if (known) return known;
    const detail = await fetchFileDetail(id, signal);
    this.remember(id, detail);
    return detail.item;
  }

  private cancelDetail(): void {
    this.detailAbort?.abort();
    this.detailAbort = undefined;
    clearTimeout(this.detailTimer);
    this.detailTimer = undefined;
  }

  /** Asks for the current file's detail once, after `detailDelay`; `willUpdate()` decides when. */
  private requestDetail(): void {
    const id = this.currentId;
    const controller = new AbortController();
    this.detailAbort = controller;
    this.detailStatus = 'pending';
    const delay = this.detailDelay;
    this.detailDelay = 0;
    const run = (): void => {
      this.detailTimer = undefined;
      void this.loadDetail(id, controller);
    };
    if (delay > 0) this.detailTimer = setTimeout(run, delay);
    else run();
  }

  private async loadDetail(id: Id, controller: AbortController): Promise<void> {
    try {
      this.remember(id, await fetchFileDetail(id, controller.signal));
      if (!controller.signal.aborted) this.detailStatus = 'idle';
    } catch (error) {
      if (controller.signal.aborted || isAbortError(error)) return;
      this.detailStatus = isNotFound(error) ? 'missing' : 'failed';
    }
  }

  private readonly retryDetail = (): void => {
    this.detailStatus = 'idle';
  };

  private go(action: NavAction): void {
    const target = targetIndex(action, this.index, this.ids.length);
    const id = target === null ? undefined : this.ids[target];
    if (target === null || id === undefined) return;
    this.cancelDetail();
    this.preloader.cancel();
    this.index = target;
    this.currentId = id;
    this.lastDirection = directionAfter(action);
    this.zoomed = false;
    this.settledId = undefined;
    this.detailStatus = 'idle';
    this.detailDelay = DETAIL_DEBOUNCE_MS;
  }

  /** The neighbour in the direction of travel, once the current file has been shown. */
  private maybePreload(): void {
    if (this.idsStatus !== 'ready' || this.settledId !== this.currentId) return;
    const next = neighbourIndex(this.index, this.lastDirection, this.ids.length);
    const id = next === null ? undefined : this.ids[next];
    if (id !== undefined) this.preloader.start(id);
  }

  private readonly onStageSettled = (event: StageSettledEvent): void => {
    if (event.detail.id !== this.currentId) return;
    this.settledId = event.detail.id;
    this.maybePreload();
  };

  private readonly toggleInfo = (): void => {
    viewerInfoOpen.value = !viewerInfoOpen.peek();
  };

  private readonly toggleZoom = (): void => {
    if (this.shownItem()?.kind === 'image') this.zoomed = !this.zoomed;
  };

  private readonly onKeyDown = (event: KeyboardEvent): void => {
    if (event.defaultPrevented || isInMedia(event.target)) return;
    const action = viewerKeyAction(event);
    if (action === null) return;
    if (isNavAction(action) && this.idsStatus !== 'ready') return;
    event.preventDefault();
    if (isNavAction(action)) this.go(action);
    else if (action === 'info') this.toggleInfo();
    else this.toggleZoom();
  };

  /**
   * The grid opens the viewer on the first click of a double click: its second click and the
   * `dblclick` must not zoom the picture or press a button. Capture phase, on the host, so they
   * never reach what is inside.
   */
  private readonly onPointerAction = (event: Event): void => {
    if (!this.guarded) return;
    event.stopPropagation();
    event.preventDefault();
  };

  private readonly onClick = (): void => {
    void this.keepFocusInside();
  };

  private readonly onClose = (): void => {
    if (this.opened) this.end();
  };

  /**
   * The current file's detail is derived here, not at the places that change what is current: it
   * is asked for when the row is unknown or the panel is open (also when another tab opens it),
   * and each file at most once. Setting `detailStatus` in `willUpdate` is part of this update.
   */
  protected override willUpdate(): void {
    if (!this.opened || this.detailStatus !== 'idle') return;
    if (this.needsDetail(this.currentId)) this.requestDetail();
    else this.detailDelay = 0;
  }

  protected override updated(): void {
    void this.keepFocusInside();
  }

  /**
   * The keys work on the dialog, so focus must stay in it: on the root (not a button) when the
   * viewer opens, and back there when it is lost to the page (a click on a bare area) or sits on a
   * button that has just been disabled (Next at the last file).
   */
  private async keepFocusInside(): Promise<void> {
    if (!this.opened) return;
    await this.querySelector('opm-modal')?.updateComplete;
    const dialog = this.querySelector('dialog');
    const root = this.querySelector<HTMLElement>('[data-viewer-root]');
    if (!this.opened || !dialog?.open || !root) return;
    const active = document.activeElement;
    const lost =
      !active ||
      !dialog.contains(active) ||
      (active instanceof HTMLButtonElement && active.disabled);
    if (this.focusPending || lost) {
      this.focusPending = false;
      root.focus();
    }
  }

  private headingFor(): string {
    const name = this.itemOf(this.currentId)?.name;
    if (name) return name;
    if (this.detailStatus === 'missing') return 'File unavailable';
    return this.detailStatus === 'failed' ? "Couldn't load the file" : 'Loading…';
  }

  /** Why there is no row to show, for the stage; a failed detail with a row to show is not one. */
  private failureFor(item: FileItem | undefined): StageFailure | undefined {
    if (item) return undefined;
    return this.detailStatus === 'missing' || this.detailStatus === 'failed'
      ? this.detailStatus
      : undefined;
  }

  private announcementFor(item: FileItem | undefined): string {
    if (!item || this.idsStatus === 'loading') return '';
    const position =
      this.idsStatus === 'ready' ? { index: this.index, count: this.ids.length } : null;
    return announcement(item, position);
  }

  private chromeView(item: FileItem | undefined): ChromeView {
    return {
      id: this.currentId,
      item,
      detail: this.details.get(this.currentId),
      idsStatus: this.idsStatus,
      index: this.index,
      count: this.ids.length,
      detailStatus: this.detailStatus,
      zoomed: this.zoomed,
      infoOpen: viewerInfoOpen.value,
    };
  }

  private renderBody(item: FileItem | undefined): TemplateResult {
    const view = this.chromeView(item);
    return html`
      <div class="flex min-h-0 flex-1 flex-col outline-none" tabindex="-1" data-viewer-root>
        ${renderToolbar(view, this.chrome)}
        <div class="relative flex min-h-0 flex-1">
          <div class="relative flex min-h-0 min-w-0 flex-1">
            ${keyed(
              this.session,
              html`<opm-viewer-stage
                .item=${item}
                .zoomed=${this.zoomed}
                .failure=${this.failureFor(item)}
                @stage-settled=${this.onStageSettled}
                @stage-retry=${this.retryDetail}
                @zoom-toggle=${this.toggleZoom}
              ></opm-viewer-stage>`,
            )}
            ${renderStepButtons(view, this.chrome)}
          </div>
          ${renderInfoPanel(view, this.chrome)}
        </div>
        <div class="sr-only" role="status" aria-live="polite">${this.announcementFor(item)}</div>
      </div>
    `;
  }

  protected override render() {
    const item = this.shownItem();
    return html`
      <opm-modal
        size="full"
        .open=${this.opened}
        .heading=${this.headingFor()}
        .body=${this.renderBody(item)}
        @close=${this.onClose}
        @keydown=${this.onKeyDown}
      ></opm-modal>
    `;
  }
}

customElements.define('opm-viewer', OpmViewer);

declare global {
  interface HTMLElementTagNameMap {
    'opm-viewer': OpmViewer;
  }
}
