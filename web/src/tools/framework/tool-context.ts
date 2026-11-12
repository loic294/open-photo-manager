import { type Signal, signal } from '@preact/signals-core';
import { requestFor } from '@/api/browse';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { Filter } from '@/api/generated/Filter';
import type { FooterCounts } from '@/api/generated/FooterCounts';
import type { Scope } from '@/api/generated/Scope';
import type { OpmPane } from '@/components/opm-pane';
import { filterFor, scopeFor } from '@/stores/filters';
import { locations, type PaneId, type PaneLocation, type ToolId } from '@/stores/nav';
import { clearAllSelections, type PaneSelection, selection } from '@/stores/selection';
import { pinnedBase, recursiveFor, setPinnedBase } from '@/stores/tool-state';
import { volumeIdByName } from '@/stores/volumes';
import { createPlanBinding, NO_PLAN, type PlanBinding } from '@/tools/framework/plan-binding';
import type { ToolConfig } from '@/tools/framework/tool-config';
import { EMPTY_FILTER, type FilterScope } from '@/util/filter';

/**
 * What a tool's callbacks (`controls`, `actions`, `setup`, `plan.params`, an action's `run`) get:
 * one object per connected `opm-tool-view`, the tool's window on everything it works with.
 *
 * The getters read signals, so reading them in a `render()` (the view's, or a callback that runs
 * inside it) or in an `effect` subscribes the reader: it runs again when the answer changes.
 * Nothing here writes the stores except `setBase`, `reportCounts` and `clearSelections`.
 *
 * - `config`, `tool` (its id), `host` (the view element) and `plan` (the `PlanBinding`).
 * - Panes: `location(pane)` is what the pane browses, `base(pane)` the folder the pane stands for
 *   (see below) and `setBase(pane, base)` pins one. `baseIsDefault(pane)` is true while the base is
 *   the pane's `defaultBase`.
 * - What the tool works on: `scope(pane)` (a `Scope` for the server, `null` when the folder cannot
 *   be resolved), `recursive` (the "Include subfolders" switch, always `true` unless the config
 *   says `'toggle'`), `toolFilter` (what the user set in the filter bar), `filter(pane)` (the
 *   EFFECTIVE filter of that pane: `toolFilter` or `EMPTY_FILTER`, as `config.filter` says) and
 *   `filterScope` (which pane(s) the filter applies to, `null` for a tool that takes no filter).
 * - What the panes report: `counts(pane)` (`null` while they load), `selection(pane)`.
 * - `request(pane)`: the `BrowseRequest` of what the pane shows, with its effective filter and the
 *   plan of its `planSide`.
 * - `refresh()` asks both panes to reload, `clearSelections()` clears both selections.
 *
 * `base(pane)` by `baseMode`: `'none'` has none (`null`); `'current'` is the location; `'pinned'`
 * is the folder the user pinned (`@/stores/tool-state`), else the base of the pane `defaultBase.from`
 * when the pane's spec has a `defaultBase`, else `null`.
 *
 * `scope(pane)` is `{ volume_id, dir, recursive }` of `base(pane)`, or of the location when the
 * pane's `baseMode` is `'none'`. It is `null` while there is nothing to scope: no base, the
 * volumes root, a volume that is unknown or not loaded yet.
 */
export class ToolContext {
  readonly config: ToolConfig;
  readonly host: HTMLElement;
  readonly plan: PlanBinding = NO_PLAN;

  private readonly counted: Record<PaneId, Signal<FooterCounts | null>> = {
    source: signal(null),
    comparison: signal(null),
  };
  private disposed = false;

  constructor(config: ToolConfig, host: HTMLElement) {
    this.config = config;
    this.host = host;
    // Last, so the binding can read a complete context at once.
    this.plan = createPlanBinding(this);
  }

  get tool(): ToolId {
    return this.config.id;
  }

  location(pane: PaneId): PaneLocation {
    return locations[pane].value;
  }

  base(pane: PaneId): PaneLocation | null {
    return this.resolveBase(pane, true);
  }

  /** Pins `base` for a `'pinned'` pane, or goes back to the default with `null`. Else ignored. */
  setBase(pane: PaneId, base: PaneLocation | null): void {
    if (this.config.panes[pane].baseMode === 'pinned') setPinnedBase(this.tool, pane, base);
  }

  baseIsDefault(pane: PaneId): boolean {
    return (
      this.config.panes[pane].defaultBase !== undefined && pinnedBase(this.tool, pane) === null
    );
  }

  scope(pane: PaneId): Scope | null {
    const place =
      this.config.panes[pane].baseMode === 'none' ? this.location(pane) : this.base(pane);
    if (place === null || place.volume === null) return null;
    const volumeId = volumeIdByName(place.volume);
    if (volumeId === undefined) return null;
    return { volume_id: volumeId, dir: place.dir, recursive: this.recursive };
  }

  get recursive(): boolean {
    return this.config.recursive === 'toggle' ? recursiveFor(this.tool) : true;
  }

  get toolFilter(): Filter {
    return filterFor(this.tool);
  }

  filter(pane: PaneId): Filter {
    const scope = this.filterScope;
    return scope === 'both' || scope === pane ? this.toolFilter : EMPTY_FILTER;
  }

  /** Which pane(s) the filter applies to. `null` for `applies: 'none'`: it applies to neither. */
  get filterScope(): FilterScope | null {
    const spec = this.config.filter;
    if (spec.applies === 'none') return null;
    if (spec.applies === 'choice') return scopeFor(this.tool, spec.initial ?? 'both');
    return spec.applies;
  }

  counts(pane: PaneId): FooterCounts | null {
    return this.counted[pane].value;
  }

  /** The view feeds the panes' `pane-counts` events in; a tool never calls it. */
  reportCounts(pane: PaneId, counts: FooterCounts | null): void {
    const current = this.counted[pane].peek();
    if (current === counts) return;
    if (
      current !== null &&
      counts !== null &&
      current.in_folder === counts.in_folder &&
      current.in_subfolders === counts.in_subfolders
    ) {
      return;
    }
    this.counted[pane].value = counts;
  }

  selection(pane: PaneId): PaneSelection {
    return selection[pane];
  }

  request(pane: PaneId): BrowseRequest | null {
    const side = this.config.panes[pane].planSide;
    const plan = side === null ? null : this.plan.ref(side);
    return requestFor(this.location(pane), this.filter(pane), plan);
  }

  /** Asks both panes to fetch their lists again (what is shown stays until the answer). */
  refresh(): void {
    for (const pane of this.host.querySelectorAll<OpmPane>('opm-pane')) pane.refresh();
  }

  clearSelections(): void {
    clearAllSelections();
  }

  /** Stops what the context started (the plan client). The view calls it; idempotent. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.plan.dispose?.();
  }

  /** `followDefault` is off for the pane a default points at: a bad config cannot loop. */
  private resolveBase(pane: PaneId, followDefault: boolean): PaneLocation | null {
    const spec = this.config.panes[pane];
    switch (spec.baseMode) {
      case 'none':
        return null;
      case 'current':
        return this.location(pane);
      case 'pinned': {
        const pinned = pinnedBase(this.tool, pane);
        if (pinned !== null || !followDefault || !spec.defaultBase) return pinned;
        return this.resolveBase(spec.defaultBase.from, false);
      }
    }
  }
}

export function createToolContext(config: ToolConfig, host: HTMLElement): ToolContext {
  return new ToolContext(config, host);
}
