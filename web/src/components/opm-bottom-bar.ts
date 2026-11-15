import { html, nothing, type TemplateResult } from 'lit';
import { live } from 'lit/directives/live.js';
import type { FooterCounts } from '@/api/generated/FooterCounts';
import { OpmElement } from '@/base/opm-element';
import '@/components/opm-icon';
import type { PaneId } from '@/stores/nav';
import { formatCount } from '@/util/format-count';

/** What one pane shows in the bottom bar. */
export interface PaneFooter {
  /** `null` while the folder is loading: skeletons are shown and Select all is disabled. */
  counts: FooterCounts | null;
  selectedCount: number;
  /** A select-all request is in flight. */
  selecting: boolean;
  /** False when the pane is a non-selectable proposed-name preview. */
  selectable?: boolean;
  /** The pane that Cmd/Ctrl+A and Esc act on: its segment gets a primary top border. */
  active: boolean;
  /** Thumbnail edge length, in the unit of the slider bounds. */
  size: number;
  sizeMin: number;
  sizeMax: number;
  sizeStep: number;
}

export interface PaneEventDetail {
  pane: PaneId;
}

export interface SizeChangeDetail extends PaneEventDetail {
  size: number;
}

export type SizeChangeEvent = CustomEvent<SizeChangeDetail>;
export type SelectAllEvent = CustomEvent<PaneEventDetail>;
export type ClearSelectionEvent = CustomEvent<PaneEventDetail>;

const PANE_LABELS: Record<PaneId, string> = {
  source: 'Source pane',
  comparison: 'Comparison pane',
};

/**
 * The action bar at the bottom of a tool: per pane the item counts, the selection and the
 * thumbnail size, and at the far right the tool's own actions. It is presentational: properties in,
 * events out, no state and no store.
 *
 *   html`<opm-bottom-bar
 *     .source=${sourceFooter}
 *     .comparison=${comparisonFooter}
 *     .actions=${html`<opm-split-button persist-key="duplicates" .options=${options}>...`}
 *     @size-change=${(e: SizeChangeEvent) => setGridSize(e.detail.pane, e.detail.size)}
 *     @select-all=${(e: SelectAllEvent) => this.selectAll(e.detail.pane)}
 *     @clear-selection=${(e: ClearSelectionEvent) => this.clear(e.detail.pane)}
 *   ></opm-bottom-bar>`
 *
 * - `source`: the source pane's `PaneFooter`.
 * - `comparison`: the comparison pane's `PaneFooter`. `null` or `undefined` (a tool without a
 *   comparison pane) renders no segment.
 * - `actions`: a template for the area at the far right. A property because slots do not exist in
 *   the light DOM.
 *
 * Events (all bubble; `detail.pane` says which pane's control was used):
 * - `size-change` `{ pane, size }`: on EVERY `input` of the slider, so the grid resizes live.
 * - `select-all` `{ pane }`, `clear-selection` `{ pane }`.
 *
 * The segments line up with the panes; narrow segments place actions below their controls.
 * Container queries hide the slider below 14rem, show the Select all label from 32rem and
 * "selected" in the badge from 24rem. Truncated counts retain their full text in a tooltip.
 */
export class OpmBottomBar extends OpmElement {
  static override properties = {
    source: { attribute: false },
    comparison: { attribute: false },
    actions: { attribute: false },
  };

  declare source: PaneFooter;
  declare comparison: PaneFooter | null | undefined;
  declare actions: TemplateResult | undefined;

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
  }

  private emit(type: 'select-all' | 'clear-selection', pane: PaneId): void;
  private emit(type: 'size-change', pane: PaneId, size: number): void;
  private emit(type: string, pane: PaneId, size?: number): void {
    const detail = size === undefined ? { pane } : { pane, size };
    this.dispatchEvent(new CustomEvent(type, { detail, bubbles: true }));
  }

  private renderCounts(counts: FooterCounts | null) {
    const inFolder = counts && `${formatCount(counts.in_folder)} in folder`;
    const inSubfolders = counts && `${formatCount(counts.in_subfolders)} in subfolders`;
    // One status element for both states, so that a change of its content is announced.
    return html`
      <div
        role="status"
        class="min-w-0 shrink grow basis-24 text-left ${counts ? 'tooltip tooltip-start' : ''}"
        data-tip=${counts ? `Matching the current filters: ${inFolder}, ${inSubfolders}` : nothing}
      >
        ${
          counts
            ? html`
                <span class="block truncate text-xs">${inFolder}</span>
                <span class="block truncate text-xs">${inSubfolders}</span>
              `
            : html`
                <span class="skeleton mb-1 block h-3 w-24 max-w-full"></span>
                <span class="skeleton block h-3 w-28 max-w-full"></span>
                <span class="sr-only">Counting items…</span>
              `
        }
      </div>
    `;
  }

  private renderSelection(pane: PaneId, footer: PaneFooter) {
    const { selectedCount } = footer;
    const count = formatCount(selectedCount);
    return html`
      <span class="sr-only" role="status">${selectedCount > 0 ? `${count} selected` : nothing}</span>
      ${
        selectedCount > 0
          ? html`
              <span class="badge badge-primary badge-sm gap-1 whitespace-nowrap" aria-hidden="true">
                ${count}<span class="hidden @sm:inline"> selected</span>
              </span>
              <div class="tooltip" data-tip="Clear selection">
                <button
                  type="button"
                  class="btn btn-ghost btn-xs btn-circle"
                  aria-label="Clear selection"
                  @click=${() => this.emit('clear-selection', pane)}
                >
                  <opm-icon name="x" size="sm"></opm-icon>
                </button>
              </div>
            `
          : nothing
      }
    `;
  }

  private renderSelectAll(pane: PaneId, footer: PaneFooter) {
    const { counts, selecting } = footer;
    const unavailable =
      footer.selectable === false || !counts || counts.in_folder === 0 || selecting;
    return html`
      <div class="tooltip" data-tip="Select all">
        <button
          type="button"
          class="btn btn-ghost btn-xs"
          aria-label="Select all"
          aria-busy=${selecting ? 'true' : nothing}
          ?disabled=${unavailable}
          @click=${() => this.emit('select-all', pane)}
        >
          ${
            selecting
              ? html`<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>`
              : html`<opm-icon name="square-check" size="sm"></opm-icon>`
          }
          <span class="hidden @lg:inline">Select all</span>
        </button>
      </div>
    `;
  }

  private renderSlider(pane: PaneId, footer: PaneFooter) {
    // The bounds come before the value: a range input clamps its value to the bounds it has.
    return html`
      <input
        type="range"
        class="range range-xs hidden w-24 min-w-12 shrink @[14rem]:block"
        aria-label="Thumbnail size"
        min=${footer.sizeMin}
        max=${footer.sizeMax}
        step=${footer.sizeStep}
        .value=${live(String(footer.size))}
        @input=${(event: Event) =>
          this.emit('size-change', pane, Number((event.target as HTMLInputElement).value))}
      />
    `;
  }

  private renderSegment(
    pane: PaneId,
    footer: PaneFooter,
    divided: boolean,
    actions?: TemplateResult,
  ) {
    const top = footer.active ? 'border-t-primary' : 'border-t-transparent';
    const start = divided ? 'border-s border-s-base-300' : '';
    return html`
      <div
        role="group"
        aria-label=${PANE_LABELS[pane]}
        class="@container flex min-h-14 min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-1 self-stretch border-t-2 px-2 ${top} ${start}"
      >
        ${this.renderCounts(footer.counts)} ${this.renderSelection(pane, footer)}
        ${this.renderSelectAll(pane, footer)} ${pane === 'source' ? this.renderSlider(pane, footer) : nothing}
        ${actions ? html`<div class="flex min-w-0 flex-none basis-full items-center justify-end py-1 @lg:basis-auto">${actions}</div>` : nothing}
      </div>
    `;
  }

  protected override render() {
    const { source, comparison, actions } = this;
    return html`
      <div class="navbar grid ${comparison ? 'grid-cols-1 md:grid-cols-2' : 'grid-cols-1'} min-h-14 border-t border-base-300 bg-base-100 p-0">
        ${this.renderSegment('source', source, false, comparison ? undefined : actions)}
        ${comparison ? this.renderSegment('comparison', comparison, true, actions) : nothing}
      </div>
    `;
  }
}

customElements.define('opm-bottom-bar', OpmBottomBar);

declare global {
  interface HTMLElementTagNameMap {
    'opm-bottom-bar': OpmBottomBar;
  }
  interface HTMLElementEventMap {
    'size-change': SizeChangeEvent;
    'select-all': SelectAllEvent;
    'clear-selection': ClearSelectionEvent;
  }
}
