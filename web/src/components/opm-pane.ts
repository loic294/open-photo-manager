import { html, nothing, type TemplateResult } from 'lit';
import { requestFor } from '@/api/browse';
import { fetchIds } from '@/api/files';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FileItem } from '@/api/generated/FileItem';
import type { Filter } from '@/api/generated/Filter';
import type { FolderEntry } from '@/api/generated/FolderEntry';
import type { FooterCounts } from '@/api/generated/FooterCounts';
import type { PlanRef } from '@/api/generated/PlanRef';
import type { SortKey } from '@/api/generated/SortKey';
import type { VolumeDto } from '@/api/generated/VolumeDto';
import { events, type ServerEventOf } from '@/api/sse';
import { OpmElement } from '@/base/opm-element';
import type { OpmFileGrid } from '@/components/opm-file-grid';
import '@/components/opm-file-grid';
import type { FolderOpenEvent, OpmFolderList } from '@/components/opm-folder-list';
import '@/components/opm-folder-list';
import type { IconName } from '@/components/opm-icon';
import '@/components/opm-icon';
import type { SelectChangeEvent, SelectOption } from '@/components/opm-select';
import '@/components/opm-select';
import { toast } from '@/components/toast';
import { FoldersController } from '@/controllers/folders-controller';
import { SelectionController } from '@/controllers/selection-controller';
import {
  formatLocation,
  linkToLocation,
  locations,
  type PaneId,
  type PaneLocation,
  ROOT_LOCATION,
  sameLocation,
  setLocation,
} from '@/stores/nav';
import { gridSize, SORT_KEYS, type SortSpec, setSort, sort } from '@/stores/prefs';
import { selection, selectionContextKey } from '@/stores/selection';
import { loadVolumes, volumeByName, volumesError, volumesStatus } from '@/stores/volumes';
import {
  type Crumb,
  crumbsFor,
  locationName,
  parentLocation,
  ROOT_CRUMB_LABEL,
} from '@/util/breadcrumbs';
import { EMPTY_FILTER, isFilterActive } from '@/util/filter';
import { emptyTextFor, folderState } from '@/util/folder-state';
import { fsChangeAffects } from '@/util/fs-change';
import { isPlainClick } from '@/util/plain-click';

/** How the pane treats its base folder (the folder the other pane is compared with). */
export type BaseMode = 'none' | 'current' | 'pinned';

/** The tool's default base: its `label` names it, `active` says the pane's base IS the default. */
export interface BaseDefault {
  label: string;
  active: boolean;
}

export interface PaneCountsDetail {
  pane: PaneId;
  counts: FooterCounts | null;
}

export interface PlanStaleDetail {
  pane: PaneId;
  plan: PlanRef;
}

export interface BaseChangeDetail {
  pane: PaneId;
  base: PaneLocation | null;
}

export type PaneCountsEvent = CustomEvent<PaneCountsDetail>;
export type PlanStaleEvent = CustomEvent<PlanStaleDetail>;
export type BaseChangeEvent = CustomEvent<BaseChangeDetail>;

declare global {
  interface GlobalEventHandlersEventMap {
    'base-change': BaseChangeEvent;
    'pane-counts': PaneCountsEvent;
    'plan-stale': PlanStaleEvent;
  }
}

const PANE_NAMES: Readonly<Record<PaneId, string>> = {
  source: 'Source',
  comparison: 'Comparison',
};

const SORT_LABELS: Readonly<Record<SortKey, string>> = {
  name: 'Name',
  date: 'Date',
  size: 'Size',
};

const SORT_OPTIONS: SelectOption[] = SORT_KEYS.map((key) => ({
  value: key,
  label: SORT_LABELS[key],
}));

/** Complete class names, so Tailwind finds them. */
const ALERT: Readonly<Record<'error' | 'warning' | 'info', { classes: string; icon: IconName }>> = {
  error: { classes: 'alert-error', icon: 'circle-x' },
  warning: { classes: 'alert-warning', icon: 'triangle-alert' },
  info: { classes: 'alert-info', icon: 'circle-alert' },
};

type AlertTone = keyof typeof ALERT;

function isSortKey(value: string): value is SortKey {
  return (SORT_KEYS as readonly string[]).includes(value);
}

function sameCounts(a: FooterCounts | null, b: FooterCounts | null): boolean {
  if (a === null || b === null) return a === b;
  return a.in_folder === b.in_folder && a.in_subfolders === b.in_subfolders;
}

/**
 * One file-system pane: the folder you are in with its subfolders as a list, then its files as a
 * grid. Every tool view shows two of them (`source` and `comparison`) and configures each by
 * properties; the pane reads the per-pane stores itself (location, sort, grid size, selection).
 *
 *   html`<opm-pane
 *     pane="comparison"
 *     heading="Comparison"
 *     .filter=${filter}
 *     .plan=${plan}
 *     ?hide-unmatched=${hideUnmatchedFolders.value}
 *     show-matches
 *     @pane-counts=${(e: PaneCountsEvent) => (footers[e.detail.pane] = e.detail.counts)}
 *     @item-open=${(e: ItemOpenEvent) => openViewer(e.detail)}
 *   ></opm-pane>`
 *
 * Properties:
 * - `pane`: `'source'` or `'comparison'`: which `locations`, `sort`, `gridSize` and `selection` it uses.
 * - `heading`: the visible label of the pane (a badge in the header, the name of its region). Never
 *   `title`.
 * - `filter`: the `Filter` of the tool view (`filterFor(tool)`); `EMPTY_FILTER` (the default) when it does
 *   not apply to this pane.
 * - `plan`: the `PlanRef` whose overlay the lists show, or `null`.
 * - `hideUnmatched` (attribute `hide-unmatched`) and `showMatches` (attribute `show-matches`):
 *   passed to the folder list (see `folderState`).
 * - `baseMode` (attribute `base-mode`): `'none'` (default), `'current'` (the browsed folder is the
 *   base: a badge) or `'pinned'` (a chip with the chosen `base` and its buttons). `base` is the
 *   EFFECTIVE base, `null` for none. `baseDefault` is only for `pinned`: `null` means the tool has no
 *   default base, otherwise `active` tells whether `base` is that default or the user's pinned folder.
 * - `disabled` and `disabledReason` (attribute `disabled-reason`): the pane stays on screen but is
 *   inert: its heading and the reason, no lists, no requests, no counts.
 *
 * Read-only: `browseRequest` (the effective `BrowseRequest`, `null` while the volumes are unknown or
 * the pane is disabled; the viewer's id list uses it), `sortSpec` and `itemAt(index)` (the loaded
 * item of the grid, the viewer's `startItem`). `refresh()` fetches the lists and the counts again.
 *
 * Events (all bubble): `pane-counts { pane, counts }` (the footer counts, `null` while they load),
 * `plan-stale { pane, plan }` (once per plan: the server forgot it, build a new one and hand it
 * over), `base-change { pane, base }` (`null` resets to the default base) and, from the selection
 * controller, `item-open`. The pane never opens the viewer or changes the base by itself.
 *
 * Layout: the host is `block h-full min-h-0 min-w-0` (give it a parent with a definite height) and
 * its root `section` a flex column. ONLY the body scrolls (the folder list, then the file grid,
 * which is not a scroller).
 *
 * Navigation: a folder row, a breadcrumb and "Up" call `setLocation(pane, ...)` (one history entry
 * each, the other pane stays) and every one is a real link, so "open in a new tab" works. A folder
 * that disappears (`folder_not_found`) is left for its nearest existing ancestor with one toast.
 */
export class OpmPane extends OpmElement {
  static override properties = {
    pane: { type: String },
    heading: { type: String },
    filter: { attribute: false },
    plan: { attribute: false },
    hideUnmatched: { type: Boolean, attribute: 'hide-unmatched' },
    showMatches: { type: Boolean, attribute: 'show-matches' },
    baseMode: { type: String, attribute: 'base-mode' },
    base: { attribute: false },
    baseDefault: { attribute: false },
    disabled: { type: Boolean },
    disabledReason: { type: String, attribute: 'disabled-reason' },
    crumbsCollapsed: { state: true },
  };

  declare pane: PaneId;
  declare heading: string;
  declare filter: Filter;
  declare plan: PlanRef | null;
  declare hideUnmatched: boolean;
  declare showMatches: boolean;
  declare baseMode: BaseMode;
  declare base: PaneLocation | null;
  declare baseDefault: BaseDefault | null;
  declare disabled: boolean;
  declare disabledReason: string;

  private readonly subfolders = new FoldersController(this);
  private readonly selecting = new SelectionController(this, { pane: () => this.pane });

  /** The request the lists show now; kept by value so an equal request is the same object. */
  private request: BrowseRequest | null = null;
  private requestKey = '';
  /** `selectionContextKey` of `request`: the listing the selection and the scroll belong to. */
  private listingKey: string | null = null;
  private providerPane: PaneId | null = null;
  private stopFsWatch: (() => void) | null = null;
  private reported: { pane: PaneId; counts: FooterCounts | null } | null = null;
  private staleKey: string | null = null;
  private handledMissing: string | null = null;
  private walkedUp = false;
  private scrollKey: string | null = null;
  private crumbsKey: string | null = null;
  private crumbsCollapsed = false;
  private crumbsWidth = 0;
  private crumbsObserver: ResizeObserver | null = null;
  private crumbsElement: HTMLElement | null = null;
  private refocus = false;

  constructor() {
    super();
    this.pane = 'source';
    this.heading = '';
    this.filter = EMPTY_FILTER;
    this.plan = null;
    this.hideUnmatched = false;
    this.showMatches = false;
    this.baseMode = 'none';
    this.base = null;
    this.baseDefault = null;
    this.disabled = false;
    this.disabledReason = '';
  }

  /** The request behind what the pane shows; the tool view builds the viewer's id list with it. */
  get browseRequest(): BrowseRequest | null {
    if (this.disabled) return null;
    return requestFor(locations[this.pane].value, this.filter, this.plan);
  }

  get sortSpec(): SortSpec {
    return sort.value[this.pane];
  }

  /**
   * The loaded item at `index` of the file grid, `undefined` while its page is not in: the viewer's
   * `startItem` for an `item-open` event (`detail.index`).
   */
  itemAt(index: number): FileItem | undefined {
    return this.querySelector<OpmFileGrid>('opm-file-grid')?.itemAt(index);
  }

  /**
   * Fetches the folders, the footer counts and the files again, now. What is shown stays until the
   * answer arrives. The pane does this by itself on `fs_change` events; the tool view's
   * `ctx.refresh()` calls it for a change the server does not announce.
   */
  refresh(): void {
    this.subfolders.invalidate();
    this.querySelector<OpmFileGrid>('opm-file-grid')?.refresh();
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0', 'min-w-0');
    this.bindIdsProvider();
    this.stopFsWatch = events.on('fs_change', this.onFsChange);
  }

  override disconnectedCallback(): void {
    this.unbindIdsProvider();
    this.stopFsWatch?.();
    this.stopFsWatch = null;
    this.crumbsObserver?.disconnect();
    this.crumbsObserver = null;
    this.crumbsElement = null;
    super.disconnectedCallback();
  }

  private get paneName(): string {
    return PANE_NAMES[this.pane] ?? PANE_NAMES.source;
  }

  private stableRequest(request: BrowseRequest | null): BrowseRequest | null {
    if (request === null) {
      this.requestKey = '';
      return null;
    }
    const key = JSON.stringify(request);
    if (key === this.requestKey && this.request !== null) return this.request;
    this.requestKey = key;
    return request;
  }

  private bindIdsProvider(): void {
    this.unbindIdsProvider();
    selection[this.pane].setIdsProvider((signal) => this.loadIds(signal));
    this.providerPane = this.pane;
  }

  private unbindIdsProvider(): void {
    if (this.providerPane !== null) selection[this.providerPane].setIdsProvider(null);
    this.providerPane = null;
  }

  /** Real files of the folder in the CURRENT sort: the request and the sort are read per call. */
  private async loadIds(signal: AbortSignal): Promise<readonly number[]> {
    const { request } = this;
    // The volumes root lists volumes only: there are no files to select.
    if (request === null || request.volume_id === null) return [];
    const { key, order } = this.sortSpec;
    return fetchIds(request, key, order, { signal });
  }

  private readonly onFsChange = (event: ServerEventOf<'fs_change'>): void => {
    const { request } = this;
    if (request === null) return;
    // The ids of the real files do not depend on the plan: only a change in this folder counts.
    if (fsChangeAffects({ volume_id: request.volume_id, dir: request.dir }, event)) {
      void selection[this.pane].reconcile();
    }
  };

  protected override willUpdate(): void {
    const request = this.stableRequest(this.browseRequest);
    this.request = request;
    this.subfolders.setRequest(request);
    this.listingKey = request === null ? null : selectionContextKey(request);
    // A pane that left the page (an update can still be queued) must not touch the shared store.
    if (!this.isConnected) return;
    if (this.listingKey !== null) selection[this.pane].setContext(this.listingKey);
    if (this.providerPane !== this.pane) this.bindIdsProvider();
  }

  protected override updated(): void {
    if (!this.isConnected) return;
    this.reportCounts();
    this.reportStalePlan();
    this.walkUp();
    this.resetScroll();
    this.followCrumbs();
    this.restoreFocus();
  }

  private emit<D>(type: string, detail: D): void {
    this.dispatchEvent(new CustomEvent<D>(type, { detail, bubbles: true }));
  }

  /** `pane-counts`: the footer counts when they change, `null` while they load or the pane is off. */
  private reportCounts(): void {
    const { pane } = this;
    const counts = this.disabled ? null : this.subfolders.counts;
    const last = this.reported;
    if (last && last.pane === pane && sameCounts(last.counts, counts)) return;
    this.reported = { pane, counts };
    this.emit<PaneCountsDetail>('pane-counts', { pane, counts });
  }

  /** `plan-stale`: once per plan reference the server answered `plan_not_found` for. */
  private reportStalePlan(): void {
    const { plan } = this;
    if (plan === null || !this.subfolders.planMissing) return;
    const key = `${plan.plan_id}:${plan.side}`;
    if (key === this.staleKey) return;
    this.staleKey = key;
    this.emit<PlanStaleDetail>('plan-stale', { pane: this.pane, plan });
  }

  /**
   * A folder that is gone is replaced by its nearest ancestor (the history entry too: Back must not
   * return to it). A chain of missing folders shows ONE toast, for the folder the user was in.
   */
  private walkUp(): void {
    const { request, subfolders } = this;
    if (request === null) return;
    if (!subfolders.missing) {
      if (subfolders.status === 'ready') {
        this.walkedUp = false;
        this.handledMissing = null;
      }
      return;
    }
    const location = locations[this.pane].peek();
    if (!this.canWalkUp(location) || this.listingKey === this.handledMissing) return;
    this.handledMissing = this.listingKey;
    if (!this.walkedUp) {
      this.walkedUp = true;
      toast.info(`The folder ${locationName(location)} no longer exists`);
    }
    setLocation(this.pane, parentLocation(location), { replace: true });
  }

  private canWalkUp(location: PaneLocation): boolean {
    return location.volume !== null && location.dir !== '';
  }

  /** A new folder, sort or filter starts at the top: the browser would only clamp the old offset. */
  private resetScroll(): void {
    const { key, order } = this.sortSpec;
    const scrollKey = this.listingKey === null ? null : `${this.listingKey}|${key}|${order}`;
    if (scrollKey === this.scrollKey) return;
    this.scrollKey = scrollKey;
    const body = this.querySelector<HTMLElement>('[data-pane-body]');
    if (body) body.scrollTop = 0;
  }

  /** Measure the complete path once, then react to the space left by the pane's controls. */
  private followCrumbs(): void {
    const key = formatLocation(locations[this.pane].peek()) ?? '';
    const crumbs = this.querySelector<HTMLElement>('[data-crumbs]');
    if (!crumbs) return;
    if (crumbs !== this.crumbsElement) {
      this.crumbsObserver?.disconnect();
      this.crumbsElement = crumbs;
      this.crumbsObserver = new ResizeObserver(() => this.fitCrumbs());
      this.crumbsObserver.observe(crumbs);
    }
    if (key !== this.crumbsKey || !this.crumbsCollapsed) {
      this.crumbsKey = key;
      this.crumbsWidth = crumbs.querySelector('ul')?.scrollWidth ?? 0;
    }
    this.fitCrumbs();
  }

  private fitCrumbs(): void {
    const available = this.crumbsElement?.clientWidth ?? 0;
    if (available > 0) this.crumbsCollapsed = this.crumbsWidth > available;
  }

  /**
   * The row a keyboard user opened goes away with its listing and the focus falls to the page: give
   * it back to the new listing once it is shown (only when nothing else took it meanwhile).
   */
  private restoreFocus(): void {
    if (!this.refocus) return;
    const { status } = this.subfolders;
    if (status !== 'ready' && status !== 'error') return;
    this.refocus = false;
    void this.focusNewListing();
  }

  private async focusNewListing(): Promise<void> {
    // The lists render on their own cycle, after the pane's.
    const folderList = this.querySelector<OpmFolderList>('opm-folder-list');
    await folderList?.updateComplete;
    const { activeElement } = document;
    if (
      !this.isConnected ||
      (activeElement && activeElement !== document.body && activeElement.isConnected)
    ) {
      return;
    }
    const row = this.querySelector<HTMLElement>('opm-folder-list a[data-name]');
    if (row) row.focus();
    else this.querySelector<OpmFileGrid>('opm-file-grid')?.focus();
  }

  /** Follows a link as a plain click; a modified click is left to the browser (a new tab). */
  private follow(event: MouseEvent, location: PaneLocation): void {
    if (!isPlainClick(event)) return;
    event.preventDefault();
    setLocation(this.pane, location);
  }

  private readonly onUp = (): void => {
    setLocation(this.pane, parentLocation(locations[this.pane].peek()));
  };

  private readonly onFolderOpen = (event: FolderOpenEvent): void => {
    this.refocus = this.contains(document.activeElement);
    setLocation(this.pane, this.entryLocation(event.detail.entry));
  };

  private readonly onSortKey = (event: SelectChangeEvent): void => {
    event.stopPropagation();
    const key = event.detail.value;
    if (isSortKey(key)) setSort(this.pane, { ...this.sortSpec, key });
  };

  private readonly onSortOrder = (): void => {
    const spec = this.sortSpec;
    setSort(this.pane, { ...spec, order: spec.order === 'asc' ? 'desc' : 'asc' });
  };

  private readonly onUseFolder = (): void => {
    const { volume, dir } = locations[this.pane].peek();
    this.emit<BaseChangeDetail>('base-change', { pane: this.pane, base: { volume, dir } });
  };

  private readonly onGoToBase = (): void => {
    if (this.base) setLocation(this.pane, this.base);
  };

  private readonly onResetBase = (): void => {
    this.emit<BaseChangeDetail>('base-change', { pane: this.pane, base: null });
  };

  private readonly onRetryVolumes = (): void => {
    void loadVolumes();
  };

  private readonly onRetryFolders = (): void => {
    this.subfolders.retry();
  };

  /** Where a folder row goes: a volume at the volumes root, else a directory of this volume. */
  private entryLocation(entry: FolderEntry): PaneLocation {
    const { volume } = locations[this.pane].value;
    return volume === null ? { volume: entry.name, dir: '' } : { volume, dir: entry.path };
  }

  /** Read by the folder list while it renders, so the links follow the other pane's moves too. */
  private readonly hrefFor = (entry: FolderEntry): string =>
    linkToLocation(this.pane, this.entryLocation(entry));

  private renderAlert(
    tone: AlertTone,
    message: string,
    action: TemplateResult | typeof nothing = nothing,
  ): TemplateResult {
    const { classes, icon } = ALERT[tone];
    return html`
      <div role="alert" class="alert alert-soft ${classes} mx-2 my-1">
        <opm-icon name=${icon} size="sm"></opm-icon>
        <span class="min-w-0 break-words">${message}</span>
        ${action}
      </div>
    `;
  }

  private renderRetry(onRetry: () => void): TemplateResult {
    return html`<button type="button" class="btn btn-xs" @click=${onRetry}>Retry</button>`;
  }

  private renderRootLink(): TemplateResult {
    return html`
      <a
        class="link"
        href=${linkToLocation(this.pane, ROOT_LOCATION)}
        @click=${(event: MouseEvent) => this.follow(event, ROOT_LOCATION)}
        >Show all volumes</a
      >
    `;
  }

  private renderCrumb(crumb: Crumb, last: boolean): TemplateResult {
    const path = formatLocation(locations[this.pane].value) ?? ROOT_CRUMB_LABEL;
    return html`
      <li class=${last ? 'min-w-0 flex-1 before:shrink-0' : 'shrink-0 before:shrink-0'}>
        <span class="tooltip tooltip-bottom ${crumb.location.volume === null ? 'tooltip-start' : ''} block min-w-0 max-w-full before:break-all" data-tip=${path}>
        <a
          href=${linkToLocation(this.pane, crumb.location)}
          class="block min-w-0 truncate"
          title=${crumb.label}
          aria-current=${last ? 'page' : nothing}
          @click=${(event: MouseEvent) => this.follow(event, crumb.location)}
          >${crumb.label}</a
        >
        </span>
      </li>
    `;
  }

  private renderSortControls(): TemplateResult {
    const spec = this.sortSpec;
    const name = this.heading || this.paneName;
    const ascending = spec.order === 'asc';
    const orderLabel = `Sort order (${name}): ${ascending ? 'ascending' : 'descending'}. Switch to ${
      ascending ? 'descending' : 'ascending'
    }`;
    return html`
      <div class="flex items-center gap-1">
        <opm-select
          size="xs"
          label="Sort by (${name})"
          .options=${SORT_OPTIONS}
          .value=${spec.key}
          @select-change=${this.onSortKey}
        ></opm-select>
        <button
          type="button"
          class="btn btn-ghost btn-square btn-xs"
          aria-label=${orderLabel}
          title=${orderLabel}
          @click=${this.onSortOrder}
        >
          <opm-icon name=${ascending ? 'arrow-up' : 'arrow-down'} size="sm"></opm-icon>
        </button>
      </div>
    `;
  }

  private renderHeader(location: PaneLocation): TemplateResult {
    const crumbs = crumbsFor(location);
    const path = formatLocation(location) ?? ROOT_CRUMB_LABEL;
    const first = crumbs[0];
    const last = crumbs[crumbs.length - 1];
    const collapsed = this.crumbsCollapsed && this.crumbsKey === (formatLocation(location) ?? '');
    const { heading } = this;
    return html`
      <header class="flex flex-none flex-wrap items-center gap-x-2 gap-y-1 px-2 py-1">
        <button
          type="button"
          class="btn btn-ghost btn-xs"
          aria-label="Up one folder"
          title="Up one folder"
          ?disabled=${location.volume === null}
          @click=${this.onUp}
        >
          <opm-icon name="corner-left-up" size="sm"></opm-icon>
          Up
        </button>
        <nav
          data-crumbs
          aria-label="${this.paneName} folder path"
          class="breadcrumbs min-w-56 flex-1 overflow-visible py-0 text-sm"
          title=${path}
        >
          <ul class=${collapsed ? 'w-full min-w-0' : 'w-max'}>
            ${
              collapsed && crumbs.length > 2 && first && last
                ? html`
                    ${this.renderCrumb(first, false)}
                    <li class="shrink-0 before:shrink-0">
                      <details class="dropdown" @keydown=${(event: KeyboardEvent) => {
                        if (event.key !== 'Escape') return;
                        event.preventDefault();
                        event.stopPropagation();
                        const details = event.currentTarget as HTMLDetailsElement;
                        details.open = false;
                        details.querySelector('summary')?.focus();
                      }}>
                        <summary class="tooltip tooltip-bottom cursor-pointer list-none rounded px-1 before:break-all focus-visible:outline"
                          data-tip=${path} aria-label="Show parent folders">...</summary>
                        <ul class="menu dropdown-content z-20 mt-2 max-h-64 w-64 max-w-[80vw] flex-col overflow-y-auto rounded-box border border-base-300 bg-base-100 p-2 shadow-lg">
                          ${crumbs.slice(1, -1).map(
                            (crumb) => html`
                            <li>
                              <a class="tooltip tooltip-bottom before:break-all" data-tip=${path}
                                href=${linkToLocation(this.pane, crumb.location)}
                                title=${formatLocation(crumb.location) ?? ROOT_CRUMB_LABEL}
                                @click=${(event: MouseEvent) => this.follow(event, crumb.location)}>
                                <span class="truncate">${crumb.label}</span>
                              </a>
                            </li>`,
                          )}
                        </ul>
                      </details>
                    </li>
                    ${this.renderCrumb(last, true)}
                  `
                : crumbs.map((crumb, index) => this.renderCrumb(crumb, index === crumbs.length - 1))
            }
          </ul>
        </nav>
        ${heading ? html`<span class="badge badge-neutral badge-sm">${heading}</span>` : nothing}
        ${
          this.subfolders.planMissing
            ? html`<span
                class="loading loading-spinner loading-xs"
                role="status"
                aria-label="Waiting for the plan"
              ></span>`
            : nothing
        }
        ${this.renderSortControls()}
      </header>
    `;
  }

  private renderPinnedBase(location: PaneLocation): TemplateResult {
    const { base, baseDefault } = this;
    const activeDefault = baseDefault?.active ? baseDefault : null;
    const atBase = base !== null && sameLocation(base, location);
    const canUse = location.volume !== null && (!atBase || activeDefault !== null);
    const text = base === null ? 'Not selected' : (formatLocation(base) ?? ROOT_CRUMB_LABEL);
    return html`
      <span
        class="badge badge-outline min-w-0 max-w-full gap-1"
        title=${activeDefault ? activeDefault.label : text}
      >
        <opm-icon name="pin" size="xs"></opm-icon>
        <span class="truncate">${text}</span>
      </span>
      ${
        activeDefault
          ? html`<span class="badge badge-ghost badge-sm">${activeDefault.label}</span>`
          : nothing
      }
      <button type="button" class="btn btn-xs" ?disabled=${!canUse} @click=${this.onUseFolder}>
        Use this folder
      </button>
      <button
        type="button"
        class="btn btn-ghost btn-xs"
        ?disabled=${base === null || atBase}
        @click=${this.onGoToBase}
      >
        Go to base
      </button>
      ${
        baseDefault !== null && !baseDefault.active
          ? html`<button
              type="button"
              class="btn btn-ghost btn-xs"
              title=${`Use again: ${baseDefault.label}`}
              @click=${this.onResetBase}
            >
              Reset
            </button>`
          : nothing
      }
    `;
  }

  private renderBase(location: PaneLocation): TemplateResult | typeof nothing {
    if (this.baseMode === 'none') return nothing;
    return html`
      <div
        role="group"
        aria-label="Base folder"
        data-base
        class="flex flex-none flex-wrap items-center gap-x-2 gap-y-1 px-2 pb-1 text-xs"
      >
        ${
          this.baseMode === 'current'
            ? html`<span class="badge badge-outline">Base folder: this folder</span>`
            : this.renderPinnedBase(location)
        }
      </div>
    `;
  }

  private renderGrid(request: BrowseRequest | undefined, emptyText: string): TemplateResult {
    const spec = this.sortSpec;
    return html`
      <opm-file-grid
        .request=${request}
        .sort=${spec.key}
        .order=${spec.order}
        .size=${gridSize.value[this.pane]}
        .selectedIds=${this.selecting.selectedIds}
        .selectionActive=${this.selecting.active}
        .emptyText=${emptyText}
        .label=${`${this.paneName} files`}
      ></opm-file-grid>
    `;
  }

  private renderFolders(loading: boolean): TemplateResult {
    return html`
      <opm-folder-list
        pane=${this.pane}
        .folders=${this.subfolders.folders}
        ?hide-unmatched=${this.hideUnmatched}
        ?show-matches=${this.showMatches}
        .hrefFor=${this.hrefFor}
        ?loading=${loading}
        @folder-open=${this.onFolderOpen}
      ></opm-folder-list>
    `;
  }

  /** The volumes are not known yet (or are gone): there is no request to show. */
  private renderWithoutRequest(location: PaneLocation): TemplateResult {
    const status = volumesStatus.value;
    if (status === 'ready') {
      return html`${this.renderAlert(
        'warning',
        `Volume ${location.volume} was not found`,
        this.renderRootLink(),
      )}`;
    }
    if (status === 'error') {
      const reason = volumesError.value;
      return html`${this.renderAlert(
        'error',
        reason ? `Could not load the volumes: ${reason}` : 'Could not load the volumes',
        this.renderRetry(this.onRetryVolumes),
      )}`;
    }
    return html`${this.renderFolders(true)}${this.renderGrid(undefined, '')}`;
  }

  private renderMissing(location: PaneLocation): TemplateResult {
    // A folder that can be left is left in `updated()`: nothing to show for that one render.
    if (this.canWalkUp(location)) return html``;
    return this.renderAlert(
      'warning',
      `The folder ${locationName(location)} no longer exists`,
      this.renderRootLink(),
    );
  }

  private renderListing(
    location: PaneLocation,
    request: BrowseRequest,
    volume: VolumeDto | undefined,
  ): TemplateResult {
    const { subfolders, hideUnmatched } = this;
    const atRoot = request.volume_id === null;
    const folders = subfolders.folders;
    const filterActive = isFilterActive(this.filter);
    const visible = folders.some((entry) => folderState(entry, hideUnmatched) !== 'hide');
    const alerts = html`
      ${
        volume !== undefined && !volume.online
          ? this.renderAlert(
              'warning',
              `Volume ${volume.name} is offline: showing what was last indexed`,
            )
          : nothing
      }
      ${
        subfolders.status === 'error'
          ? this.renderAlert(
              'error',
              `Could not load the folders: ${subfolders.error ?? 'unknown error'}`,
              this.renderRetry(this.onRetryFolders),
            )
          : nothing
      }
    `;
    if (subfolders.missing) return html`${alerts}${this.renderMissing(location)}`;

    const foldersKnown = subfolders.status === 'ready';
    if (atRoot) {
      const text = emptyTextFor({ folders, hideUnmatched, filterActive, atRoot, foldersKnown });
      return html`
        ${alerts}${this.renderFolders(subfolders.status === 'loading')}
        ${
          foldersKnown && !visible
            ? html`<p role="status" class="px-2 py-16 text-center text-base-content/60">${text}</p>`
            : nothing
        }
      `;
    }
    const emptyText = emptyTextFor({ folders, hideUnmatched, filterActive, foldersKnown });
    return html`
      ${alerts}${this.renderFolders(subfolders.status === 'loading')}
      ${visible ? html`<div role="separator" class="divider my-0"></div>` : nothing}
      ${this.renderGrid(subfolders.planMissing ? undefined : request, emptyText)}
    `;
  }

  private renderBody(location: PaneLocation): TemplateResult {
    const { request } = this;
    const volume = volumeByName(location.volume);
    return html`
      <div
        data-pane-body
        class="min-h-0 flex-1 overflow-y-auto"
        aria-busy=${this.subfolders.refreshing ? 'true' : nothing}
      >
        ${
          request === null
            ? this.renderWithoutRequest(location)
            : this.renderListing(location, request, volume)
        }
      </div>
    `;
  }

  private renderDisabled(): TemplateResult {
    const { heading, disabledReason } = this;
    return html`
      ${
        heading
          ? html`<header class="flex flex-none items-center gap-2 px-2 py-1">
              <span class="badge badge-neutral badge-sm">${heading}</span>
            </header>`
          : nothing
      }
      ${disabledReason ? this.renderAlert('info', disabledReason) : nothing}
    `;
  }

  protected override render(): TemplateResult {
    const active = this.selecting.isActivePane;
    const location = locations[this.pane].value;
    return html`
      <section
        aria-label=${this.heading || `${this.paneName} pane`}
        aria-current=${active ? 'true' : nothing}
        class="flex h-full min-h-0 min-w-0 flex-col ${active ? 'ring-1 ring-inset ring-primary/40' : ''}"
      >
        ${active ? html`<span class="sr-only">active pane</span>` : nothing}
        ${
          this.disabled
            ? this.renderDisabled()
            : html`${this.renderHeader(location)}${this.renderBase(location)}${this.renderBody(location)}`
        }
      </section>
    `;
  }
}

customElements.define('opm-pane', OpmPane);

declare global {
  interface HTMLElementTagNameMap {
    'opm-pane': OpmPane;
  }
}
