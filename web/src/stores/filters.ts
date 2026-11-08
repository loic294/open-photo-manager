import { batch, signal } from '@preact/signals-core';
import type { Filter } from '@/api/generated/Filter';
import { TOOLS, type ToolId } from '@/stores/nav';
import { dictionaryOf, oneOf, persistedSignal } from '@/stores/persisted';
import {
  EMPTY_FILTER,
  FILTER_SCOPES,
  type FilterScope,
  filterKey,
  isFilterActive,
  normalizeFilter,
} from '@/util/filter';

/**
 * The filter each tool applies to its items, and which pane(s) it applies to.
 *
 * The filter lives in memory only, one entry per tool: it survives moving between tools during
 * the session, and a reload clears it on purpose, so a filter the user cannot see never changes
 * what they find. `selection_id` is part of it: a hand-off sets it (`setFilter(tool,
 * { ...EMPTY_FILTER, selection_id })`) and the filter bar's chip clears it. The scope persists
 * (it is a preference, like `hideUnmatchedFolders` in `@/stores/prefs`).
 */

const filters = signal<Partial<Record<ToolId, Filter>>>({});

/** The tool's filter (`EMPTY_FILTER` while none was set). Read it where signals are tracked. */
export function filterFor(tool: ToolId): Filter {
  return filters.value[tool] ?? EMPTY_FILTER;
}

/**
 * Replaces the tool's filter by its normalised copy. Nothing happens, and nothing is notified,
 * when that selects what the filter in place selects (the same names in another order, case or
 * Unicode form included: the stored spelling stays).
 */
export function setFilter(tool: ToolId, filter: Filter): void {
  const next = normalizeFilter(filter);
  const stored = isFilterActive(next) ? next : EMPTY_FILTER;
  const current = filters.peek()[tool] ?? EMPTY_FILTER;
  if (filterKey(current) === filterKey(stored)) return;
  filters.value = { ...filters.peek(), [tool]: stored };
}

/** Clears the filter of one tool, or of every tool, `selection_id` included. Keeps the scope. */
export function resetFilters(tool?: ToolId): void {
  batch(() => {
    for (const id of tool === undefined ? TOOLS : [tool]) setFilter(id, EMPTY_FILTER);
  });
}

/** Tool id -> the scope last chosen there. Prefer `scopeFor` and `setScope`. */
export const filterScopes = persistedSignal<Record<string, FilterScope>>(
  'filterScope',
  {},
  dictionaryOf(oneOf(FILTER_SCOPES)),
);

/** Which pane(s) the tool's filter applies to; `initial` until the user chooses. */
export function scopeFor(tool: ToolId, initial: FilterScope = 'both'): FilterScope {
  return filterScopes.value[tool] ?? initial;
}

export function setScope(tool: ToolId, scope: FilterScope): void {
  if (filterScopes.peek()[tool] !== scope) {
    filterScopes.value = { ...filterScopes.peek(), [tool]: scope };
  }
}
