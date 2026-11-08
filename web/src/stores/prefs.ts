import type { SortKey } from '@/api/generated/SortKey';
import type { SortOrder } from '@/api/generated/SortOrder';
import { PANES, type PaneId, type ToolId } from '@/stores/nav';
import {
  dictionaryOf,
  isBoolean,
  isString,
  numberBetween,
  oneOf,
  persistedSignal,
  recordOf,
  type Validator,
} from '@/stores/persisted';

/** User preferences. Every signal persists to localStorage and syncs across tabs. */

export const THEMES = ['system', 'light', 'dark'] as const;
export type ThemePref = (typeof THEMES)[number];

/** `system` follows the operating system. Applied to `<html>` by `@/app/theme`. */
export const theme = persistedSignal<ThemePref>('theme', 'system', oneOf(THEMES));

/** Thumbnail edge length in px. `step` is for the slider; `setGridSize` clamps. */
export const GRID_SIZE = { min: 96, max: 320, step: 16, default: 160 } as const;

/** Thumbnail size of each pane. Change it with `setGridSize`. */
export const gridSize = persistedSignal<Record<PaneId, number>>(
  'gridSize',
  { source: GRID_SIZE.default, comparison: GRID_SIZE.default },
  recordOf(PANES, numberBetween(GRID_SIZE.min, GRID_SIZE.max)),
);

export function setGridSize(pane: PaneId, px: number): void {
  if (!Number.isFinite(px)) return;
  const size = Math.min(GRID_SIZE.max, Math.max(GRID_SIZE.min, Math.round(px)));
  if (gridSize.peek()[pane] !== size) gridSize.value = { ...gridSize.peek(), [pane]: size };
}

/** Folders that do not match the filters: hidden when true, greyed out when false (default). */
export const hideUnmatchedFolders = persistedSignal<boolean>(
  'hideUnmatchedFolders',
  false,
  isBoolean,
);

/** Host warnings dismissed by code; absent codes reappear if the condition returns later. */
export const dismissedHostWarnings = persistedSignal<Record<string, boolean>>(
  'dismissedHostWarnings',
  {},
  dictionaryOf(isBoolean),
);

/** The last choice made in each tool's split button: tool id -> action id (an opaque string). */
export const lastAction = persistedSignal<Record<string, string>>(
  'lastAction',
  {},
  dictionaryOf(isString),
);

export function lastActionOf(tool: ToolId): string | undefined {
  return lastAction.value[tool];
}

export function setLastAction(tool: ToolId, action: string): void {
  if (lastAction.peek()[tool] !== action)
    lastAction.value = { ...lastAction.peek(), [tool]: action };
}

export const SORT_KEYS = ['name', 'date', 'size'] as const satisfies readonly SortKey[];
export const SORT_ORDERS = ['asc', 'desc'] as const satisfies readonly SortOrder[];

export interface SortSpec {
  key: SortKey;
  order: SortOrder;
}

const isSortKey = oneOf(SORT_KEYS);
const isSortOrder = oneOf(SORT_ORDERS);

const isSortSpec: Validator<SortSpec> = (value): value is SortSpec => {
  if (typeof value !== 'object' || value === null) return false;
  const { key, order } = value as Record<string, unknown>;
  return isSortKey(key) && isSortOrder(order);
};

/** Sort of each pane. Change it with `setSort`. */
export const sort = persistedSignal<Record<PaneId, SortSpec>>(
  'sort',
  { source: { key: 'name', order: 'asc' }, comparison: { key: 'name', order: 'asc' } },
  recordOf(PANES, isSortSpec),
);

export function setSort(pane: PaneId, next: SortSpec): void {
  const current = sort.peek()[pane];
  if (current.key === next.key && current.order === next.order) return;
  sort.value = { ...sort.peek(), [pane]: { key: next.key, order: next.order } };
}
