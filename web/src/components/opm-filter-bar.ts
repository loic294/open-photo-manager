import { html, nothing, type PropertyValues } from 'lit';
import { errorMessage, isAbortError } from '@/api/errors';
import { type FacetOptions, fetchFacets, mergeFacets, NO_FACETS } from '@/api/facets';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { Filter } from '@/api/generated/Filter';
import type { FlagFilter } from '@/api/generated/FlagFilter';
import type { TaskState } from '@/api/generated/TaskState';
import { events, type ServerEventOf } from '@/api/sse';
import { OpmElement } from '@/base/opm-element';
import '@/components/opm-icon';
import '@/components/opm-multi-select';
import type { MultiSelectChangeEvent } from '@/components/opm-multi-select';
import '@/components/opm-segmented';
import type { SegmentedChangeEvent, SegmentOption } from '@/components/opm-segmented';
import {
  activeFilterCount,
  DEFAULT_SCOPE_LABELS,
  EMPTY_FILTER,
  FILTER_SCOPES,
  type FilterScope,
  FLAGS,
  filterKey,
  MAX_RATING,
  normalizeFilter,
  type ScopeLabels,
} from '@/util/filter';
import { fsChangeAffects } from '@/util/fs-change';

/** `detail` is the new filter in its canonical form (see `normalizeFilter`). */
export type FilterChangeEvent = CustomEvent<Filter>;
export type ScopeChangeEvent = CustomEvent<FilterScope>;
/** `detail` is the new state of the toggle. */
export type HideUnmatchedChangeEvent = CustomEvent<boolean>;
/** The user dropped the selection restriction (its chip, or "Clear filters"). */
export type SelectionClearEvent = CustomEvent<null>;

/** Waits for a filter to settle (a click on a button, typing in a search box) before asking. */
const FILTER_DEBOUNCE_MS = 250;
/** `fs_change` events come in bursts (a scan, an import): wait for the burst to end... */
const FS_DEBOUNCE_MS = 2000;
/** ...but never longer than this after the first event of the burst. */
const FS_MAX_WAIT_MS = 10_000;

// A record, so a state added to the backend enum fails the type check until it is classified here.
const IS_TERMINAL: Readonly<Record<TaskState, boolean>> = {
  queued: false,
  running: false,
  done: true,
  failed: true,
  cancelled: true,
  interrupted: true,
};

const RATING_OPTIONS: readonly SegmentOption[] = [
  { value: '0', label: 'Unrated' },
  ...Array.from({ length: MAX_RATING }, (_, index): SegmentOption => {
    const stars = index + 1;
    return {
      value: String(stars),
      label: String(stars),
      name: stars === 1 ? '1 star' : `${stars} stars`,
      icon: 'star',
    };
  }),
];

const FLAG_OPTIONS: readonly (SegmentOption & { value: FlagFilter })[] = [
  { value: 'selected', label: 'Selected', icon: 'flag' },
  { value: 'rejected', label: 'Rejected', icon: 'ban' },
];

let instanceCounter = 0;

/**
 * The filter controls every tool shows above its listing: cameras, star ratings, flags, tags, a
 * restriction to a selection, and the toggle that hides folders without matches.
 *
 *   html`<opm-filter-bar
 *     .filter=${filterFor('browse')}
 *     .contexts=${[{ volume_id: 1, dir: 'trips/2024', filter, plan: null }]}
 *     .hideUnmatched=${hideUnmatchedFolders.value}
 *     @filter-change=${(e: FilterChangeEvent) => setFilter('browse', e.detail)}
 *     @hide-unmatched-change=${(e: HideUnmatchedChangeEvent) => { hideUnmatchedFolders.value = e.detail; }}
 *   ></opm-filter-bar>`
 *
 * The bar reads no store: everything comes in by property and every change goes out as an event
 * whose `detail` is the new value (the host stores it and feeds it back; the bar is CONTROLLED).
 * Only its own four events leave it, the events of the controls inside are stopped.
 *
 * - `filter-change`: the new `Filter`, normalised. Fired on every toggle and click, never while
 *   typing in a search box.
 * - `scope-change`: the new `FilterScope` (`both`, `source` or `comparison`). The "Apply to"
 *   selector shows only when `scope` is not `null`; `scopeLabels` renames its three options.
 * - `hide-unmatched-change`: the new state of the toggle.
 * - `selection-clear`: the restriction to a selection (`filter.selection_id`) was dropped. The
 *   chip shows while `selection_id` is set, labelled `selectionLabel` (or "Selection"). "Clear
 *   filters" fires `filter-change` with the empty filter and then this event too, when a
 *   selection was active.
 *
 * `disabled` locks every control (a tool that takes no filter) and shows `disabledReason` as a
 * line of text.
 *
 * The values offered for cameras and tags (and the counts on the rating buttons) come from
 * `POST /api/facets`, one request per DISTINCT location of `contexts` (only `volume_id` and `dir`
 * are used, with the bar's own filter and no plan), merged by `mergeFacets`. The bar never waits
 * for them: the controls work at once, with the values known so far, and the lists fill in when
 * the answer arrives. A request starts 250 ms after the location or the filter changed (the
 * previous one is aborted, a late answer is dropped), immediately after the event stream
 * reconnects or an index task ends, and 2 s after the last `fs_change` event for a location
 * (never later than 10 s after the first). A failure keeps the values already shown and offers a
 * retry. Values chosen but absent from the facets stay listed and removable.
 */
export class OpmFilterBar extends OpmElement {
  static override properties = {
    filter: { attribute: false },
    contexts: { attribute: false },
    scope: { type: String },
    scopeLabels: { attribute: false },
    hideUnmatched: { type: Boolean, attribute: 'hide-unmatched' },
    selectionLabel: { type: String, attribute: 'selection-label' },
    disabled: { type: Boolean },
    disabledReason: { type: String, attribute: 'disabled-reason' },
    facets: { state: true },
    facetCounted: { state: true },
    loading: { state: true },
    facetError: { state: true },
  };

  /** What is filtered now. */
  declare filter: Filter;
  /**
   * The locations whose values the bar offers (one per pane the filter applies to). Anything
   * else in a `BrowseRequest` is ignored, so a tool passes its pane requests as they are.
   */
  declare contexts: readonly BrowseRequest[];
  /** Which pane(s) the filter applies to. `null` hides the selector (a one-pane tool). */
  declare scope: FilterScope | null;
  declare scopeLabels: ScopeLabels;
  declare hideUnmatched: boolean;
  /** Names the selection the listing is restricted to. */
  declare selectionLabel: string | null;
  declare disabled: boolean;
  /** Why the bar is locked, shown while it is. */
  declare disabledReason: string | undefined;
  declare private facets: FacetOptions;
  /** True when `facets` come from one location, so they carry counts. */
  declare private facetCounted: boolean;
  declare private loading: boolean;
  declare private facetError: string | null;

  private readonly uid = `opm-filter-bar-${++instanceCounter}`;
  private controller: AbortController | undefined;
  private generation = 0;
  private lastFacetKey = '';
  private filterTimer: number | undefined;
  private fsTimer: number | undefined;
  private fsBurstStart = 0;
  private stops: Array<() => void> = [];

  constructor() {
    super();
    this.filter = EMPTY_FILTER;
    this.contexts = [];
    this.scope = null;
    this.scopeLabels = DEFAULT_SCOPE_LABELS;
    this.hideUnmatched = false;
    this.selectionLabel = null;
    this.disabled = false;
    this.facets = NO_FACETS;
    this.facetCounted = false;
    this.loading = false;
    this.facetError = null;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
    this.stops = [
      events.on('task', this.onTask),
      events.on('fs_change', this.onFsChange),
      events.onResync(this.onResync),
    ];
    if (this.hasUpdated) {
      this.lastFacetKey = this.facetKey();
      this.scheduleFacets();
    }
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    for (const stop of this.stops) stop();
    this.stops = [];
    this.clearTimers();
    this.abortFacets();
    this.lastFacetKey = '';
    this.loading = false;
  }

  protected override willUpdate(changed: PropertyValues): void {
    if (!changed.has('contexts') && !changed.has('filter') && !changed.has('disabled')) return;
    const key = this.facetKey();
    if (key === this.lastFacetKey) return;
    this.lastFacetKey = key;
    this.scheduleFacets();
  }

  /** The distinct locations to ask about, in the order given. */
  private locations(): Array<{ volume_id: number | null; dir: string }> {
    const seen = new Set<string>();
    const locations: Array<{ volume_id: number | null; dir: string }> = [];
    for (const { volume_id, dir } of this.contexts) {
      const id = JSON.stringify([volume_id, dir]);
      if (seen.has(id)) continue;
      seen.add(id);
      locations.push({ volume_id, dir });
    }
    return locations;
  }

  /** Equal while what the facets depend on is equal. */
  private facetKey(): string {
    return JSON.stringify([this.disabled, this.locations(), filterKey(this.filter)]);
  }

  private clearTimers(): void {
    window.clearTimeout(this.filterTimer);
    window.clearTimeout(this.fsTimer);
    this.filterTimer = undefined;
    this.fsTimer = undefined;
  }

  /** Cancels the request in flight; whatever it still delivers is dropped. */
  private abortFacets(): void {
    this.controller?.abort();
    this.controller = undefined;
    this.generation += 1;
  }

  private resetFacets(): void {
    this.abortFacets();
    this.facets = NO_FACETS;
    this.facetCounted = false;
    this.loading = false;
    this.facetError = null;
  }

  /** Something the facets depend on changed: ask again once it settles. */
  private scheduleFacets(): void {
    this.clearTimers();
    if (!this.isConnected) return;
    if (this.disabled || this.locations().length === 0) {
      this.resetFacets();
      return;
    }
    this.abortFacets();
    this.filterTimer = window.setTimeout(() => {
      this.filterTimer = undefined;
      void this.loadFacets();
    }, FILTER_DEBOUNCE_MS);
  }

  /** The data may have changed under the same inputs: ask again at once. */
  private refreshNow(): void {
    if (!this.isConnected || this.disabled || this.locations().length === 0) return;
    void this.loadFacets();
  }

  private async loadFacets(): Promise<void> {
    this.clearTimers();
    this.abortFacets();
    const controller = new AbortController();
    this.controller = controller;
    const { generation } = this;
    const filter = normalizeFilter(this.filter);
    const requests = this.locations().map(
      ({ volume_id, dir }): BrowseRequest => ({ volume_id, dir, filter, plan: null }),
    );
    this.loading = true;
    try {
      const parts = await Promise.all(
        requests.map((request) => fetchFacets(request, { signal: controller.signal })),
      );
      if (generation !== this.generation) return;
      this.facets = mergeFacets(parts);
      this.facetCounted = parts.length === 1;
      this.facetError = null;
    } catch (failure) {
      if (generation !== this.generation || isAbortError(failure)) return;
      // All or nothing: a half-answer would offer values of one pane only.
      controller.abort();
      this.facetError = errorMessage(failure, 'Could not load the filter options');
    } finally {
      if (generation === this.generation) this.loading = false;
    }
  }

  private refreshSoon(): void {
    const now = Date.now();
    if (this.fsTimer === undefined) this.fsBurstStart = now;
    else window.clearTimeout(this.fsTimer);
    const wait = Math.min(FS_DEBOUNCE_MS, Math.max(0, this.fsBurstStart + FS_MAX_WAIT_MS - now));
    this.fsTimer = window.setTimeout(() => {
      this.fsTimer = undefined;
      this.refreshNow();
    }, wait);
  }

  private readonly onResync = (): void => this.refreshNow();

  private readonly onTask = ({ task }: ServerEventOf<'task'>): void => {
    if (task.kind === 'index' && IS_TERMINAL[task.state]) this.refreshNow();
  };

  private readonly onFsChange = (event: ServerEventOf<'fs_change'>): void => {
    const affected = this.locations().some((location) =>
      fsChangeAffects(location, event, { subtree: true }),
    );
    if (affected) this.refreshSoon();
  };

  private dispatch<T>(type: string, detail: T): void {
    this.dispatchEvent(new CustomEvent<T>(type, { detail, bubbles: true }));
  }

  private change(patch: Partial<Filter>): void {
    if (this.disabled) return;
    this.dispatch<Filter>('filter-change', normalizeFilter({ ...this.filter, ...patch }));
  }

  private readonly onCameras = (event: MultiSelectChangeEvent): void => {
    event.stopPropagation();
    this.change({ cameras: event.detail });
  };

  private readonly onTags = (event: MultiSelectChangeEvent): void => {
    event.stopPropagation();
    this.change({ tags: event.detail });
  };

  private readonly onRatings = (event: SegmentedChangeEvent): void => {
    event.stopPropagation();
    const values = Array.isArray(event.detail) ? event.detail : [event.detail];
    this.change({ ratings: values.map(Number) });
  };

  private readonly onFlags = (event: SegmentedChangeEvent): void => {
    event.stopPropagation();
    const values = Array.isArray(event.detail) ? event.detail : [event.detail];
    this.change({ flags: FLAGS.filter((flag) => values.includes(flag)) });
  };

  private readonly onScope = (event: SegmentedChangeEvent): void => {
    event.stopPropagation();
    if (this.disabled) return;
    const next = FILTER_SCOPES.find((scope) => scope === event.detail);
    if (next !== undefined && next !== this.scope) this.dispatch<FilterScope>('scope-change', next);
  };

  private readonly onHideUnmatched = (event: Event): void => {
    const input = event.currentTarget as HTMLInputElement;
    const next = input.checked;
    // Controlled: the switch shows the property until the host feeds the change back.
    input.checked = this.hideUnmatched;
    if (this.disabled) return;
    this.dispatch<boolean>('hide-unmatched-change', next);
  };

  private readonly onClear = (): void => {
    const hadSelection = this.filter.selection_id !== null;
    this.dispatch<Filter>('filter-change', normalizeFilter(EMPTY_FILTER));
    if (hadSelection) this.dispatch<null>('selection-clear', null);
  };

  private readonly onSelectionClear = (): void => this.dispatch<null>('selection-clear', null);

  private readonly onRetry = (): void => this.refreshNow();

  /** The rating buttons, with the count of each when the facets come from one location. */
  private ratingOptions(): SegmentOption[] {
    if (!this.facetCounted) return [...RATING_OPTIONS];
    const counts = new Map(this.facets.ratings.map((rating) => [rating.value, rating.count]));
    return RATING_OPTIONS.map((option) => ({ ...option, count: counts.get(option.value) ?? 0 }));
  }

  private renderScope() {
    if (this.scope === null) return nothing;
    const labels = { ...DEFAULT_SCOPE_LABELS, ...this.scopeLabels };
    const options = FILTER_SCOPES.map((value): SegmentOption => ({ value, label: labels[value] }));
    return html`
      <div class="flex items-center gap-2">
        <span class="text-sm text-base-content/70">Apply to</span>
        <opm-segmented
          label="Apply to"
          mode="single"
          .options=${options}
          .value=${this.scope}
          .disabled=${this.disabled}
          @segmented-change=${this.onScope}
        ></opm-segmented>
      </div>
    `;
  }

  private renderSelection() {
    if (this.filter.selection_id === null) return nothing;
    const label = this.selectionLabel || 'Selection';
    return html`
      <span class="badge badge-info h-7 min-w-0 max-w-full gap-1 pr-0.5">
        <span class="truncate" title=${label}>${label}</span>
        <button
          type="button"
          class="btn btn-ghost btn-circle btn-xs size-5 min-h-0 shrink-0"
          aria-label="Remove selection filter"
          ?disabled=${this.disabled}
          @click=${this.onSelectionClear}
        >
          <opm-icon name="x" size="xs"></opm-icon>
        </button>
      </span>
    `;
  }

  private renderClear(active: number) {
    return html`
      <button
        type="button"
        class="btn btn-ghost btn-xs"
        aria-label=${active > 0 ? `Clear filters (${active} active)` : nothing}
        ?disabled=${this.disabled || active === 0}
        @click=${this.onClear}
      >
        <opm-icon name="funnel-x" size="xs"></opm-icon>
        Clear filters
        ${active > 0 ? html`<span class="badge badge-primary badge-xs">${active}</span>` : nothing}
      </button>
    `;
  }

  private renderUnmatchedToggle() {
    const title = this.hideUnmatched
      ? 'Folders without matching files are hidden'
      : 'Folders without matching files are greyed out';
    return html`
      <label
        class="flex items-center gap-2 text-sm ${this.disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer'}"
        title=${title}
      >
        <input
          type="checkbox"
          class="toggle toggle-sm"
          role="switch"
          .checked=${this.hideUnmatched}
          ?disabled=${this.disabled}
          @change=${this.onHideUnmatched}
        />
        Hide folders without matches
      </label>
    `;
  }

  private renderHint() {
    if (this.disabled) {
      return this.disabledReason
        ? html`<p id="${this.uid}-reason" class="text-sm text-base-content/70">${this.disabledReason}</p>`
        : nothing;
    }
    if (this.facetError === null) return nothing;
    return html`
      <p
        class="flex items-center gap-1 text-sm text-warning"
        role="status"
        title=${this.facetError}
      >
        <opm-icon name="triangle-alert" size="sm"></opm-icon>
        Filter options unavailable
        <button
          type="button"
          class="btn btn-ghost btn-xs"
          ?disabled=${this.loading}
          @click=${this.onRetry}
        >
          Retry
        </button>
      </p>
    `;
  }

  protected override render() {
    const { filter, disabled } = this;
    const active = activeFilterCount(filter);
    const described = disabled && this.disabledReason ? `${this.uid}-reason` : nothing;
    return html`
      <div
        class="flex flex-wrap items-center gap-x-3 gap-y-2"
        role="group"
        aria-label="Filters"
        aria-describedby=${described}
      >
        ${this.renderScope()}
        <opm-multi-select
          label="Cameras"
          icon="camera"
          placeholder="Search cameras"
          .options=${this.facets.cameras}
          .selected=${filter.cameras}
          .loading=${this.loading}
          .disabled=${disabled}
          @multi-select-change=${this.onCameras}
        ></opm-multi-select>
        <opm-segmented
          label="Rating"
          mode="multi"
          .options=${this.ratingOptions()}
          .value=${filter.ratings.map(String)}
          .disabled=${disabled}
          @segmented-change=${this.onRatings}
        ></opm-segmented>
        <opm-segmented
          label="Flags"
          mode="multi"
          .options=${FLAG_OPTIONS}
          .value=${filter.flags}
          .disabled=${disabled}
          @segmented-change=${this.onFlags}
        ></opm-segmented>
        <opm-multi-select
          label="Tags"
          icon="tag"
          placeholder="Search tags"
          allow-custom
          .options=${this.facets.tags}
          .selected=${filter.tags}
          .loading=${this.loading}
          .disabled=${disabled}
          @multi-select-change=${this.onTags}
        ></opm-multi-select>
        ${this.renderSelection()} ${this.renderClear(active)} ${this.renderUnmatchedToggle()}
        ${this.renderHint()}
      </div>
    `;
  }
}

customElements.define('opm-filter-bar', OpmFilterBar);

declare global {
  interface HTMLElementTagNameMap {
    'opm-filter-bar': OpmFilterBar;
  }
  interface HTMLElementEventMap {
    'filter-change': FilterChangeEvent;
    'hide-unmatched-change': HideUnmatchedChangeEvent;
    'scope-change': ScopeChangeEvent;
    'selection-clear': SelectionClearEvent;
  }
}
