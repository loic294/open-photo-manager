import { effect, untracked } from '@preact/signals-core';
import { html, nothing, type PropertyValues, type TemplateResult } from 'lit';
import { keyed } from 'lit/directives/keyed.js';
import { fetchIds } from '@/api/files';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { PlanRef } from '@/api/generated/PlanRef';
import { OpmElement } from '@/base/opm-element';
import type {
  ClearSelectionEvent,
  PaneFooter,
  SelectAllEvent,
  SizeChangeEvent,
} from '@/components/opm-bottom-bar';
import '@/components/opm-bottom-bar';
import type {
  FilterChangeEvent,
  HideUnmatchedChangeEvent,
  ScopeChangeEvent,
} from '@/components/opm-filter-bar';
import '@/components/opm-filter-bar';
import '@/components/opm-modal';
import type {
  BaseChangeEvent,
  BaseDefault,
  OpmPane,
  PaneCountsEvent,
  PlanStaleEvent,
} from '@/components/opm-pane';
import '@/components/opm-pane';
import { openViewer } from '@/components/opm-viewer';
import type { ItemOpenEvent } from '@/controllers/selection-controller';
import { installSelectionShortcuts } from '@/controllers/selection-shortcuts';
import { setFilter, setScope } from '@/stores/filters';
import {
  currentTool,
  locations,
  PANES,
  type PaneId,
  sameLocation,
  setLocation,
} from '@/stores/nav';
import { GRID_SIZE, gridSize, hideUnmatchedFolders, setGridSize } from '@/stores/prefs';
import { activePane, clearAllSelections, selection } from '@/stores/selection';
import { filtersOpen, toolHelpOpen } from '@/stores/tool-chrome';
import {
  handoffFor,
  navigationSynced,
  setHandoff,
  setNavigationSynced,
  setRecursive,
} from '@/stores/tool-state';
import { renderPlanStatus } from '@/tools/framework/plan-status';
import { renderActions } from '@/tools/framework/tool-actions';
import type { ToolConfig } from '@/tools/framework/tool-config';
import { createToolContext, type ToolContext } from '@/tools/framework/tool-context';
import { DEFAULT_SCOPE_LABELS } from '@/util/filter';

/**
 * The layout and the wiring shared by every tool: render a `ToolConfig` (`./tool-config.ts`) and
 * the tool needs no layout code of its own.
 *
 *   html`<opm-tool-view .config=${browseTool}></opm-tool-view>`
 *
 * Top to bottom: tool controls with "Include subfolders" at the right, collapsible filters,
 * the two panes side by side (stacked below the
 * `md` breakpoint) and the bottom bar with the tool's actions. The host fills its parent
 * (`flex h-full min-h-0 min-w-0 flex-col`) and nothing but a pane's own body scrolls, so the
 * parent of the view needs a definite height (a page element is `block h-full min-h-0`). Every
 * pane is at least 383 px wide when side by side (the shell's content area is 768 px at the
 * `md` breakpoint), and the full width of the content area when stacked.
 *
 * While connected the view owns a `ToolContext` (`config.setup` and the plan binding start with
 * it and stop with it), installs the selection shortcuts and starts with both selections empty.
 * Giving it another `config` while connected starts the new tool the same way. A new context
 * (another tool, a reconnection) also gets new panes and a new bar, so that nothing of the
 * previous tool stays on screen and the panes report their counts to the new context.
 *
 * It wires the components to the stores and the context, so a tool never does:
 * - the filter bar: `filter-change`, `scope-change` (only for `applies: 'choice'`) and
 *   `selection-clear` (which also drops the hand-off label) write `@/stores/filters` and
 *   `@/stores/tool-state`; `hide-unmatched-change` writes the `hideUnmatchedFolders` preference.
 * - the panes: `pane-counts` feed `ctx.counts`, `plan-stale` goes to `ctx.plan.stale`,
 *   `base-change` to `ctx.setBase`, and `item-open` opens the viewer on THAT pane's listing.
 * - the bottom bar: `size-change` writes the grid size, `select-all` marks the pane active and
 *   selects every file of its listing, `clear-selection` clears it.
 */
export class OpmToolView extends OpmElement {
  static override properties = {
    config: { attribute: false },
    controlsOpen: { state: true },
  };

  declare config: ToolConfig | undefined;
  private controlsOpen = false;

  private ctx: ToolContext | null = null;
  private startedFor: ToolConfig | undefined;
  private stopSetup: (() => void) | null = null;
  private stopNavigation: (() => void) | null = null;
  private stopShortcuts: (() => void) | null = null;
  private readonly mirroredScroll = new WeakMap<HTMLElement, number>();
  /** Stable objects for the panes' properties: a new object each render would re-render them. */
  private readonly planRefs: Record<PaneId, PlanRef | null> = { source: null, comparison: null };
  private readonly baseDefaults: Record<PaneId, BaseDefault | null> = {
    source: null,
    comparison: null,
  };

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('flex', 'h-full', 'min-h-0', 'min-w-0', 'flex-col');
    this.stopShortcuts ??= installSelectionShortcuts();
    if (this.config) {
      this.start(this.config);
      // The first connection renders by itself; a reconnected view has to be asked.
      if (this.hasUpdated) this.requestUpdate();
    }
  }

  override disconnectedCallback(): void {
    this.stop();
    this.stopShortcuts?.();
    this.stopShortcuts = null;
    super.disconnectedCallback();
  }

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (!changed.has('config') || !this.isConnected || this.config === this.startedFor) return;
    this.stop();
    if (this.config) {
      const { config } = this;
      untracked(() => this.start(config));
    }
  }

  /** Builds the context first: `setup` and the plan binding may read it at once. */
  private start(config: ToolConfig): void {
    if (this.startedFor === config) return;
    this.startedFor = config;
    filtersOpen.value = false;
    toolHelpOpen.value = false;
    this.controlsOpen = false;
    clearAllSelections();
    const ctx = createToolContext(config, this);
    this.ctx = ctx;
    const cleanup = config.setup?.(ctx);
    this.stopSetup = typeof cleanup === 'function' ? cleanup : null;
    this.stopNavigation = effect(() => {
      if (currentTool.value !== config.id || !navigationSynced(config.id)) return;
      if (sameLocation(locations.source.value, locations.comparison.value)) return;
      untracked(() => {
        const pane = activePane.peek();
        setLocation(pane, locations[pane].peek(), { sync: true, replace: true });
      });
    });
  }

  private stop(): void {
    this.stopNavigation?.();
    this.stopNavigation = null;
    this.stopSetup?.();
    this.stopSetup = null;
    this.ctx?.dispose();
    this.ctx = null;
    this.startedFor = undefined;
    this.planRefs.source = null;
    this.planRefs.comparison = null;
    this.baseDefaults.source = null;
    this.baseDefaults.comparison = null;
  }

  private stablePlanRef(pane: PaneId, next: PlanRef | null): PlanRef | null {
    const current = this.planRefs[pane];
    if (next !== null && current?.plan_id === next.plan_id && current.side === next.side) {
      return current;
    }
    this.planRefs[pane] = next;
    return next;
  }

  private stableBaseDefault(ctx: ToolContext, pane: PaneId): BaseDefault | null {
    const spec = ctx.config.panes[pane].defaultBase;
    if (!spec) {
      this.baseDefaults[pane] = null;
      return null;
    }
    const active = ctx.baseIsDefault(pane);
    const current = this.baseDefaults[pane];
    if (current?.label === spec.label && current.active === active) return current;
    const next = { label: spec.label, active };
    this.baseDefaults[pane] = next;
    return next;
  }

  private readonly onFilterChange = (event: FilterChangeEvent): void => {
    if (this.ctx) setFilter(this.ctx.tool, event.detail);
  };

  private readonly onScopeChange = (event: ScopeChangeEvent): void => {
    if (this.ctx) setScope(this.ctx.tool, event.detail);
  };

  private readonly onHideUnmatchedChange = (event: HideUnmatchedChangeEvent): void => {
    hideUnmatchedFolders.value = event.detail;
  };

  private readonly onSelectionClear = (): void => {
    const { ctx } = this;
    if (!ctx) return;
    setFilter(ctx.tool, { ...ctx.toolFilter, selection_id: null });
    setHandoff(ctx.tool, null);
  };

  private readonly onPaneCounts = (event: PaneCountsEvent): void => {
    this.ctx?.reportCounts(event.detail.pane, event.detail.counts);
  };

  private readonly onPlanStale = (event: PlanStaleEvent): void => {
    this.ctx?.plan.stale(event.detail.plan);
  };

  private readonly onBaseChange = (event: BaseChangeEvent): void => {
    this.ctx?.setBase(event.detail.pane, event.detail.base);
  };

  /** The pane that holds the item knows the request and the sort the viewer must page through. */
  private readonly onItemOpen = (event: ItemOpenEvent): void => {
    const target = event.target;
    const pane = target instanceof Element ? target.closest<OpmPane>('opm-pane') : null;
    const request = pane?.browseRequest;
    if (!pane || !request) return;
    const { key, order } = pane.sortSpec;
    const { id, index } = event.detail;
    void openViewer({
      startId: id,
      startItem: pane.itemAt(index),
      loadIds: (signal) => fetchIds(request, key, order, { signal }),
    });
  };

  private readonly onSizeChange = (event: SizeChangeEvent): void => {
    setGridSize(event.detail.pane, event.detail.size);
  };

  private readonly onPaneScroll = (event: CustomEvent<{ pane: PaneId }>): void => {
    const ctx = this.ctx;
    if (!ctx || !navigationSynced(ctx.tool)) return;
    const panes = [...this.querySelectorAll<OpmPane>('opm-pane')];
    const body = panes
      .find((pane) => pane.pane === event.detail.pane)
      ?.querySelector<HTMLElement>('[data-pane-body]');
    const other = panes
      .find((pane) => pane.pane !== event.detail.pane)
      ?.querySelector<HTMLElement>('[data-pane-body]');
    if (!body || !other) return;
    const mirrored = this.mirroredScroll.get(body);
    this.mirroredScroll.delete(body);
    if (body.scrollTop === mirrored || body.scrollTop === other.scrollTop) return;
    other.scrollTop = body.scrollTop;
    this.mirroredScroll.set(other, other.scrollTop);
  };

  private readonly onSelectAll = (event: SelectAllEvent): void => {
    const { pane } = event.detail;
    activePane.value = pane;
    void selection[pane].selectAll();
  };

  private readonly onClearSelection = (event: ClearSelectionEvent): void => {
    const { pane } = event.detail;
    activePane.value = pane;
    selection[pane].clear();
  };

  private renderRecursiveSwitch(ctx: ToolContext) {
    if (ctx.config.recursive !== 'toggle') return nothing;
    return html`
      <label class="flex shrink-0 cursor-pointer items-center gap-2 text-sm">
        <input
          type="checkbox"
          class="toggle toggle-sm"
          role="switch"
          .checked=${ctx.recursive}
          @change=${(event: Event) =>
            setRecursive(ctx.tool, (event.target as HTMLInputElement).checked)}
        />
        Include subfolders
      </label>
    `;
  }

  private renderToolBar(ctx: ToolContext): TemplateResult {
    const { config } = ctx;
    const controls = config.controls?.(ctx);
    return html`
      <header
        class="flex min-h-10 flex-none flex-wrap items-center gap-x-3 gap-y-2 border-b border-base-300 bg-base-100 px-3 py-2"
      >
        ${
          controls
            ? html`<div class="flex min-w-56 flex-1 flex-wrap items-center gap-x-3 gap-y-2">${controls}</div>`
            : nothing
        }
        ${
          config.controlsDialog
            ? html`<button type="button" class="btn btn-sm" aria-haspopup="dialog"
                @click=${() => (this.controlsOpen = true)}>
                <opm-icon name="pencil" size="sm"></opm-icon>${config.controlsDialog.label}
              </button>`
            : nothing
        }
        ${config.plan ? renderPlanStatus(ctx) : nothing}
        <div class="ml-auto flex flex-wrap items-center gap-3">
          ${this.renderRecursiveSwitch(ctx)}
          <label class="flex shrink-0 cursor-pointer items-center gap-2 text-sm">
            <input type="checkbox" class="toggle toggle-sm" role="switch"
              .checked=${navigationSynced(ctx.tool)}
              @change=${(event: Event) => {
                const on = (event.target as HTMLInputElement).checked;
                setNavigationSynced(ctx.tool, on);
              }} />
            Sync navigation
          </label>
        </div>
      </header>
    `;
  }

  private renderFilterBar(ctx: ToolContext): TemplateResult {
    const spec = ctx.config.filter;
    const scope = ctx.filterScope;
    const contexts = PANES.filter((pane) => scope === 'both' || scope === pane)
      .map((pane) => ctx.request(pane))
      .filter((request): request is BrowseRequest => request !== null);
    return html`
      <div id="tool-filters" ?hidden=${!filtersOpen.value}
        class="flex-none border-b border-base-300 bg-base-100 px-2 py-2">
        <opm-filter-bar
          .filter=${ctx.toolFilter}
          .contexts=${contexts}
          .scope=${spec.applies === 'choice' ? scope : null}
          .scopeLabels=${(spec.applies === 'choice' && spec.labels) || DEFAULT_SCOPE_LABELS}
          .hideUnmatched=${hideUnmatchedFolders.value}
          .selectionLabel=${handoffFor(ctx.tool)?.label ?? null}
          .disabled=${spec.applies === 'none'}
          .disabledReason=${spec.applies === 'none' ? spec.reason : undefined}
          @filter-change=${this.onFilterChange}
          @scope-change=${this.onScopeChange}
          @hide-unmatched-change=${this.onHideUnmatchedChange}
          @selection-clear=${this.onSelectionClear}
        ></opm-filter-bar>
      </div>
    `;
  }

  private renderPane(ctx: ToolContext, pane: PaneId): TemplateResult {
    const spec = ctx.config.panes[pane];
    const plan = spec.planSide === null ? null : ctx.plan.ref(spec.planSide);
    return html`
      <div class="min-h-0 min-w-0">
        <opm-pane
          .pane=${pane}
          .heading=${spec.heading}
          .filter=${ctx.filter(pane)}
          .plan=${this.stablePlanRef(pane, plan)}
          .hideUnmatched=${hideUnmatchedFolders.value}
          .showMatches=${spec.showMatches}
          .baseMode=${spec.baseMode}
          .base=${ctx.base(pane)}
          .baseDefault=${this.stableBaseDefault(ctx, pane)}
          .syncNavigation=${navigationSynced(ctx.tool)}
          .plannedOnly=${ctx.tool === 'rename' && pane === 'comparison' && plan !== null}
        ></opm-pane>
      </div>
    `;
  }

  private renderPanes(ctx: ToolContext): TemplateResult {
    return html`
      <div
        class="relative grid min-h-0 flex-1 grid-cols-1 grid-rows-2 divide-y divide-base-300 md:grid-cols-2 md:grid-rows-1 md:divide-x md:divide-y-0"
        @pane-counts=${this.onPaneCounts}
        @plan-stale=${this.onPlanStale}
        @base-change=${this.onBaseChange}
        @item-open=${this.onItemOpen}
        @pane-scroll=${this.onPaneScroll}
      >
        ${PANES.map((pane) => this.renderPane(ctx, pane))}
        ${
          ctx.config.floatingActions
            ? html`<div data-floating-actions
                class="pointer-events-none absolute right-3 bottom-3 z-10 max-w-[calc(100%-1.5rem)] rounded-field border-0 bg-base-100 shadow-lg [&>*]:pointer-events-auto">
                ${renderActions(ctx)}
              </div>`
            : nothing
        }
      </div>
    `;
  }

  private footerOf(ctx: ToolContext, pane: PaneId): PaneFooter {
    const chosen = ctx.selection(pane);
    return {
      counts: ctx.counts(pane),
      selectedCount: chosen.count.value,
      selecting: chosen.busy.value,
      active: activePane.value === pane,
      size: gridSize.value[pane],
      sizeMin: GRID_SIZE.min,
      sizeMax: GRID_SIZE.max,
      sizeStep: GRID_SIZE.step,
      selectable: !(ctx.tool === 'rename' && pane === 'comparison' && ctx.plan.ref('comparison')),
    };
  }

  private renderBottomBar(ctx: ToolContext): TemplateResult {
    return html`
      <opm-bottom-bar
        class="flex-none"
        .source=${this.footerOf(ctx, 'source')}
        .comparison=${this.footerOf(ctx, 'comparison')}
        .actions=${ctx.config.floatingActions ? undefined : renderActions(ctx)}
        @size-change=${this.onSizeChange}
        @select-all=${this.onSelectAll}
        @clear-selection=${this.onClearSelection}
      ></opm-bottom-bar>
    `;
  }

  protected override render() {
    const { ctx } = this;
    if (!ctx) return nothing;
    // A new context (another tool, a reconnection) gets new panes: they report their counts again.
    return keyed(
      ctx,
      html`
        ${this.renderToolBar(ctx)} ${this.renderFilterBar(ctx)} ${this.renderPanes(ctx)}
        ${this.renderBottomBar(ctx)}
        <opm-modal .open=${toolHelpOpen.value} .heading=${ctx.config.title}
          .body=${html`<p class="text-sm text-base-content/80">${ctx.config.summary}</p>
            <h4 class="mt-5 mb-2 font-semibold">How to use it</h4>
            <ol class="list-decimal space-y-3 pl-5 text-sm">
              ${(ctx.config.help ?? ['Open a folder in either pane to get started.']).map((step) => html`<li>${step}</li>`)}
            </ol>`}
          @close=${() => (toolHelpOpen.value = false)}></opm-modal>
        ${
          ctx.config.controlsDialog
            ? html`<opm-modal .open=${this.controlsOpen}
                .heading=${ctx.config.controlsDialog.label} size="lg"
                .body=${ctx.config.controlsDialog.render(ctx)}
                .actions=${html`<button type="button" class="btn btn-primary btn-sm"
                  @click=${() => (this.controlsOpen = false)}>Done</button>`}
                @close=${() => (this.controlsOpen = false)}></opm-modal>`
            : nothing
        }
      `,
    );
  }
}

customElements.define('opm-tool-view', OpmToolView);

declare global {
  interface HTMLElementTagNameMap {
    'opm-tool-view': OpmToolView;
  }
}
