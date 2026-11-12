import type { Filter } from '@/api/generated/Filter';
import type { FlagFilter } from '@/api/generated/FlagFilter';

/**
 * Pure helpers for the item `Filter` (cameras, ratings, flags, tags, selection). They mirror the
 * server's canonical form (`Filter::normalise` in `opm-core`) closely enough that two filters the
 * server treats alike get the same key here.
 */

/** Flag states in the order the server keeps them. */
export const FLAGS: readonly FlagFilter[] = ['selected', 'rejected'];

/** The highest star rating; 0 means unrated. */
export const MAX_RATING = 5;

/** The server rejects (never truncates) longer lists and entries, see `FilterError`. */
export const MAX_LIST_ENTRIES = 200;
export const MAX_ENTRY_CHARS = 200;

/** Which pane(s) of a two-pane tool the filter applies to (the duplicate finder's choice). */
export const FILTER_SCOPES = ['both', 'source', 'comparison'] as const;
export type FilterScope = (typeof FILTER_SCOPES)[number];
export type ScopeLabels = Readonly<Record<FilterScope, string>>;

export const DEFAULT_SCOPE_LABELS: ScopeLabels = {
  both: 'Both',
  source: 'Source',
  comparison: 'Comparison',
};

function frozenList<T>(): T[] {
  return Object.freeze<T[]>([]) as T[];
}

/** Matches everything. Frozen: copy it (`{ ...EMPTY_FILTER, tags: [...] }`), never edit it. */
export const EMPTY_FILTER: Filter = Object.freeze({
  cameras: frozenList<string>(),
  ratings: frozenList<number>(),
  flags: frozenList<FlagFilter>(),
  tags: frozenList<string>(),
  selection_id: null,
});

/**
 * What the server compares camera and tag names by: Unicode NFC, then lower case, then NFC
 * again, so `Canon EOS R5` and `canon eos r5` are one value.
 */
export function textKey(text: string): string {
  return text.normalize('NFC').toLowerCase().normalize('NFC');
}

export function sameText(a: string, b: string): boolean {
  return textKey(a) === textKey(b);
}

/** Trimmed, blanks dropped, one entry per `textKey` (the first spelling wins), sorted by key. */
function normalizeTexts(values: readonly string[]): string[] {
  const byKey = new Map<string, string>();
  for (const raw of values) {
    const text = raw.trim();
    if (text === '') continue;
    const key = textKey(text);
    if (!byKey.has(key)) byKey.set(key, text);
  }
  return [...byKey].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([, text]) => text);
}

/** Integers from 0 to 5, ascending, once each. Anything else is dropped, never clamped. */
function normalizeRatings(values: readonly number[]): number[] {
  const ratings = new Set<number>();
  for (const value of values) {
    if (Number.isInteger(value) && value >= 0 && value <= MAX_RATING) ratings.add(value);
  }
  return [...ratings].sort((a, b) => a - b);
}

function normalizeFlags(values: readonly FlagFilter[]): FlagFilter[] {
  return FLAGS.filter((flag) => values.includes(flag));
}

/**
 * The canonical copy of a filter: names trimmed, de-duplicated without case and sorted, ratings
 * ascending and within 0 to 5, flags in the fixed order. Idempotent. The spelling of a name is
 * kept as given (it has to keep matching the facet values), only the comparison is NFC and
 * case-insensitive.
 */
export function normalizeFilter(filter: Filter): Filter {
  return {
    cameras: normalizeTexts(filter.cameras),
    ratings: normalizeRatings(filter.ratings),
    flags: normalizeFlags(filter.flags),
    tags: normalizeTexts(filter.tags),
    selection_id: Number.isSafeInteger(filter.selection_id) ? filter.selection_id : null,
  };
}

/** The number of dimensions that constrain the result (the selection counts as one). */
export function activeFilterCount(filter: Filter): number {
  const { cameras, ratings, flags, tags, selection_id } = normalizeFilter(filter);
  return (
    [cameras, ratings, flags, tags].filter((list) => list.length > 0).length +
    (selection_id === null ? 0 : 1)
  );
}

export function isFilterActive(filter: Filter): boolean {
  return activeFilterCount(filter) > 0;
}

/**
 * A string that is equal for filters that select the same files, whatever the order, duplicates,
 * case, Unicode form or blank entries of their lists (like `Filter::cache_key` on the server): the
 * "did it change?" test and the cache key of a request.
 */
export function filterKey(filter: Filter): string {
  const { cameras, tags, ...rest } = normalizeFilter(filter);
  return JSON.stringify({ ...rest, cameras: cameras.map(textKey), tags: tags.map(textKey) });
}

export function sameFilter(a: Filter, b: Filter): boolean {
  return filterKey(a) === filterKey(b);
}

/**
 * A new list without `value` when it is in `list`, else with it appended. `same` decides what
 * "in" means (default: `Object.is`; pass `sameText` for names).
 */
export function toggleIn<T>(
  list: readonly T[],
  value: T,
  same: (a: T, b: T) => boolean = Object.is,
): T[] {
  return list.some((entry) => same(entry, value))
    ? list.filter((entry) => !same(entry, value))
    : [...list, value];
}
