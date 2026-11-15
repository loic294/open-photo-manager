import { batch, signal } from '@preact/signals-core';
import { PANES, type PaneId, type PaneLocation, sameLocation, type ToolId } from '@/stores/nav';

/**
 * What belongs to one tool and is neither a filter (`@/stores/filters`) nor a preference: the
 * base folder the user pinned in a pane, the "Include subfolders" switch and the hand-off.
 *
 * It lives in memory only, one entry per tool: it survives moving between tools during the
 * session, and a reload clears it on purpose, so a base or a switch the user cannot see never
 * surprises them before a destructive action. The reads track the signals, so read them where
 * signals are tracked (a render, an `effect`). Prefer the `ToolContext` getters in a tool.
 */

/** What restricts a tool's items to a server-side selection: the label the filter bar's chip shows. */
export interface Handoff {
  label: string;
}

type PerTool<T> = Partial<Record<ToolId, T>>;

const pinnedBases = signal<PerTool<Partial<Record<PaneId, PaneLocation | null>>>>({});
const recursiveSwitches = signal<PerTool<boolean>>({});
const navigationSwitches = signal<PerTool<boolean>>({});
const handoffs = signal<PerTool<Handoff | null>>({});

/** The folder the user pinned as the base of the pane, `null` until they choose one. */
export function pinnedBase(tool: ToolId, pane: PaneId): PaneLocation | null {
  return pinnedBases.value[tool]?.[pane] ?? null;
}

/** Pins `base` (a copy is kept), or clears the pinned base with `null`. A no-op when unchanged. */
export function setPinnedBase(tool: ToolId, pane: PaneId, base: PaneLocation | null): void {
  const current = pinnedBases.peek()[tool] ?? {};
  const stored = current[pane] ?? null;
  if (base === null ? stored === null : stored !== null && sameLocation(stored, base)) return;
  const copy = base === null ? null : { volume: base.volume, dir: base.dir };
  pinnedBases.value = { ...pinnedBases.peek(), [tool]: { ...current, [pane]: copy } };
}

/** The "Include subfolders" switch of the tool: on until the user turns it off. */
export function recursiveFor(tool: ToolId): boolean {
  return recursiveSwitches.value[tool] ?? true;
}

export function setRecursive(tool: ToolId, on: boolean): void {
  if ((recursiveSwitches.peek()[tool] ?? true) === on) return;
  recursiveSwitches.value = { ...recursiveSwitches.peek(), [tool]: on };
}

/** Navigation and scrolling are linked only while this tool's visible switch is on. */
export function navigationSynced(tool: ToolId): boolean {
  return navigationSwitches.value[tool] ?? false;
}

export function setNavigationSynced(tool: ToolId, on: boolean): void {
  if (navigationSynced(tool) === on) return;
  navigationSwitches.value = { ...navigationSwitches.peek(), [tool]: on };
}

/** The label of the server-side selection restricting the tool's items, `null` for none. */
export function handoffFor(tool: ToolId): Handoff | null {
  return handoffs.value[tool] ?? null;
}

/** Sets the hand-off (a copy is kept) or clears it with `null`. A no-op when unchanged. */
export function setHandoff(tool: ToolId, handoff: Handoff | null): void {
  const stored = handoffs.peek()[tool] ?? null;
  if (handoff === null ? stored === null : stored?.label === handoff.label) return;
  const copy = handoff === null ? null : { label: handoff.label };
  handoffs.value = { ...handoffs.peek(), [tool]: copy };
}

/** Forgets everything the tool kept: its pinned bases, its switch and its hand-off. */
export function resetToolState(tool: ToolId): void {
  batch(() => {
    for (const pane of PANES) setPinnedBase(tool, pane, null);
    setRecursive(tool, true);
    setNavigationSynced(tool, false);
    setHandoff(tool, null);
  });
}

/** Back to the state of a fresh page load, every tool included. For tests only. */
export function resetToolStateForTests(): void {
  batch(() => {
    pinnedBases.value = {};
    recursiveSwitches.value = {};
    navigationSwitches.value = {};
    handoffs.value = {};
  });
}
