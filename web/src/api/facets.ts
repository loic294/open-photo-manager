import { api } from '@/api/client';
import type { FetchOptions } from '@/api/files';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FacetCount } from '@/api/generated/FacetCount';
import type { Facets } from '@/api/generated/Facets';
import { textKey } from '@/util/filter';

/**
 * The values the filter bar offers for one dimension. `count` is the number of items that carry
 * the value, and is only set when the list comes from exactly one context (see `mergeFacets`).
 */
export interface FacetOption {
  value: string;
  count?: number;
}

export interface FacetOptions {
  cameras: FacetOption[];
  tags: FacetOption[];
  /** Star ratings as strings, `"0"` (unrated) to `"5"`, only those that occur. */
  ratings: FacetOption[];
}

export const NO_FACETS: Readonly<FacetOptions> = Object.freeze({
  cameras: [],
  tags: [],
  ratings: [],
});

/**
 * The values of a location (the folder and everything below it, or a whole volume, or the whole
 * library) with the number of items for each: `POST /api/facets`. Each dimension is counted under
 * every filter of `request` EXCEPT its own, so the bar can offer the alternatives to a choice.
 * Walking a large library takes about half a second: never wait for it.
 *
 * Only the named fields are sent: a caller's request may carry extras (a plan, a view model).
 */
export function fetchFacets(request: BrowseRequest, options: FetchOptions = {}): Promise<Facets> {
  const { signal, client = api } = options;
  return client.post<Facets>('/api/facets', {
    body: {
      volume_id: request.volume_id,
      dir: request.dir,
      filter: request.filter,
      plan: request.plan,
    },
    signal,
  });
}

function byName(a: FacetOption, b: FacetOption): number {
  const x = textKey(a.value);
  const y = textKey(b.value);
  return x < y ? -1 : x > y ? 1 : 0;
}

/** Every value once, in order of appearance; `key` says which values are the same. */
function unionOf(lists: readonly (readonly FacetCount[])[], key: (value: string) => string) {
  const seen = new Map<string, FacetOption>();
  for (const list of lists) {
    for (const { value } of list) {
      const id = key(value);
      if (!seen.has(id)) seen.set(id, { value });
    }
  }
  return [...seen.values()];
}

/**
 * Merges the answers for the locations a filter bar covers (one per pane).
 *
 * - One answer: used as it is, in the server's order (most items first), WITH counts.
 * - Several: the UNION of the values, names compared without case (the first spelling wins),
 *   sorted by name, ratings numerically, WITHOUT counts: two locations may share items below a
 *   common parent, so their counts cannot be added up.
 * - None: nothing.
 */
export function mergeFacets(parts: readonly Facets[]): FacetOptions {
  const [only] = parts;
  if (only !== undefined && parts.length === 1) {
    return { cameras: only.cameras, tags: only.tags, ratings: only.ratings };
  }
  return {
    cameras: unionOf(
      parts.map((part) => part.cameras),
      textKey,
    ).sort(byName),
    tags: unionOf(
      parts.map((part) => part.tags),
      textKey,
    ).sort(byName),
    ratings: unionOf(
      parts.map((part) => part.ratings),
      (value) => value,
    ).sort((a, b) => Number(a.value) - Number(b.value)),
  };
}
