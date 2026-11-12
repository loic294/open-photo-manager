import { effect } from '@preact/signals-core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Filter } from '@/api/generated/Filter';
import { EMPTY_FILTER } from '@/util/filter';

type Filters = typeof import('@/stores/filters');

let loaded: Filters | undefined;

async function loadFilters(): Promise<Filters> {
  vi.resetModules();
  loaded = await import('@/stores/filters');
  return loaded;
}

function stored(key: string): unknown {
  const raw = localStorage.getItem(`opm.v1.${key}`);
  return raw === null ? undefined : JSON.parse(raw);
}

function filter(overrides: Partial<Filter> = {}): Filter {
  return { ...EMPTY_FILTER, ...overrides };
}

// Each test loads a fresh module graph, so compare with the frozen empty filter by value.
function expectEmpty(actual: Filter): void {
  expect(actual).toEqual(EMPTY_FILTER);
  expect(Object.isFrozen(actual)).toBe(true);
}

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  loaded?.filterScopes.dispose();
  loaded = undefined;
});

describe('filterFor', () => {
  it('is the empty filter for a tool that has none', async () => {
    const { filterFor } = await loadFilters();

    expectEmpty(filterFor('browse'));
    expectEmpty(filterFor('move'));
  });

  it('returns the same object until the filter changes', async () => {
    const { filterFor, setFilter } = await loadFilters();
    setFilter('browse', filter({ tags: ['a'] }));

    expect(filterFor('browse')).toBe(filterFor('browse'));
  });
});

describe('setFilter', () => {
  it('keeps the normalised copy', async () => {
    const { filterFor, setFilter } = await loadFilters();

    setFilter(
      'browse',
      filter({ cameras: ['b', 'A', 'a'], ratings: [5, 1, 9], flags: ['rejected', 'selected'] }),
    );

    expect(filterFor('browse')).toEqual(
      filter({ cameras: ['A', 'b'], ratings: [1, 5], flags: ['selected', 'rejected'] }),
    );
  });

  it('keeps one filter per tool', async () => {
    const { filterFor, setFilter } = await loadFilters();

    setFilter('browse', filter({ tags: ['a'] }));
    setFilter('duplicates', filter({ ratings: [3] }));

    expect(filterFor('browse')).toEqual(filter({ tags: ['a'] }));
    expect(filterFor('duplicates')).toEqual(filter({ ratings: [3] }));
    expectEmpty(filterFor('move'));
  });

  it('does not share its input: later edits of it change nothing', async () => {
    const { filterFor, setFilter } = await loadFilters();
    const input = filter({ cameras: ['a'] });

    setFilter('browse', input);
    input.cameras.push('b');

    expect(filterFor('browse').cameras).toEqual(['a']);
  });

  it('stores the frozen empty filter for a filter that constrains nothing', async () => {
    const { filterFor, setFilter } = await loadFilters();
    setFilter('browse', filter({ tags: ['a'] }));

    setFilter('browse', filter({ tags: [' ', ''], ratings: [9] }));

    expectEmpty(filterFor('browse'));
  });

  it('keeps a selection id (a hand-off sets only that)', async () => {
    const { filterFor, setFilter } = await loadFilters();

    setFilter('move', { ...EMPTY_FILTER, selection_id: 42 });

    expect(filterFor('move')).toEqual(filter({ selection_id: 42 }));
  });

  it('notifies when the filter changes', async () => {
    const { filterFor, setFilter } = await loadFilters();
    const seen: Filter[] = [];
    const stop = effect(() => {
      seen.push(filterFor('browse'));
    });

    setFilter('browse', filter({ tags: ['a'] }));
    stop();

    expect(seen).toEqual([EMPTY_FILTER, filter({ tags: ['a'] })]);
  });

  it('does not notify for the same filter in another spelling', async () => {
    const { filterFor, setFilter } = await loadFilters();
    setFilter('browse', filter({ tags: ['a', 'B'] }));
    let runs = 0;
    const stop = effect(() => {
      filterFor('browse');
      runs += 1;
    });

    setFilter('browse', filter({ tags: ['B', 'a', 'A', ' '] }));
    stop();

    expect(runs).toBe(1);
  });

  it('keeps the stored spelling when only the case of a name differs', async () => {
    const { filterFor, setFilter } = await loadFilters();
    setFilter('browse', filter({ cameras: ['Canon EOS R5'] }));
    let runs = 0;
    const stop = effect(() => {
      filterFor('browse');
      runs += 1;
    });

    setFilter('browse', filter({ cameras: ['CANON EOS R5'] }));
    stop();

    expect(runs).toBe(1);
    expect(filterFor('browse').cameras).toEqual(['Canon EOS R5']);
  });

  it('does not notify when an empty filter replaces a missing one', async () => {
    const { filterFor, setFilter } = await loadFilters();
    let runs = 0;
    const stop = effect(() => {
      filterFor('browse');
      runs += 1;
    });

    setFilter('browse', EMPTY_FILTER);
    stop();

    expect(runs).toBe(1);
  });

  it('is not persisted: a reload starts clean', async () => {
    const first = await loadFilters();
    first.setFilter('browse', filter({ tags: ['a'] }));
    first.filterScopes.dispose();

    const { filterFor } = await loadFilters();

    expectEmpty(filterFor('browse'));
    expect(localStorage.length).toBe(0);
  });
});

describe('resetFilters', () => {
  it('clears one tool and leaves the others, selection id included', async () => {
    const { filterFor, resetFilters, setFilter } = await loadFilters();
    setFilter('browse', filter({ tags: ['a'], selection_id: 3 }));
    setFilter('move', filter({ ratings: [1] }));

    resetFilters('browse');

    expectEmpty(filterFor('browse'));
    expect(filterFor('move')).toEqual(filter({ ratings: [1] }));
  });

  it('clears every tool without an argument', async () => {
    const { filterFor, resetFilters, setFilter } = await loadFilters();
    setFilter('browse', filter({ tags: ['a'] }));
    setFilter('cleanup', filter({ flags: ['rejected'] }));

    resetFilters();

    expectEmpty(filterFor('browse'));
    expectEmpty(filterFor('cleanup'));
  });

  it('notifies once for all tools', async () => {
    const { filterFor, resetFilters, setFilter } = await loadFilters();
    setFilter('browse', filter({ tags: ['a'] }));
    setFilter('move', filter({ tags: ['b'] }));
    let runs = 0;
    const stop = effect(() => {
      filterFor('browse');
      filterFor('move');
      runs += 1;
    });

    resetFilters();
    stop();

    expect(runs).toBe(2);
  });

  it('keeps the scope', async () => {
    const { resetFilters, scopeFor, setScope } = await loadFilters();
    setScope('duplicates', 'source');

    resetFilters('duplicates');

    expect(scopeFor('duplicates')).toBe('source');
  });
});

describe('scopeFor and setScope', () => {
  it('is both until the user chooses', async () => {
    const { scopeFor } = await loadFilters();

    expect(scopeFor('duplicates')).toBe('both');
    expect(localStorage.length).toBe(0);
  });

  it('starts from the tool configuration when there is no choice', async () => {
    const { scopeFor } = await loadFilters();

    expect(scopeFor('duplicates', 'comparison')).toBe('comparison');
  });

  it('prefers the choice over the configured start', async () => {
    const { scopeFor, setScope } = await loadFilters();

    setScope('duplicates', 'source');

    expect(scopeFor('duplicates', 'comparison')).toBe('source');
  });

  it('keeps one scope per tool', async () => {
    const { scopeFor, setScope } = await loadFilters();

    setScope('duplicates', 'comparison');

    expect(scopeFor('duplicates')).toBe('comparison');
    expect(scopeFor('move')).toBe('both');
  });

  it('persists the choice and restores it after a reload', async () => {
    const first = await loadFilters();
    first.setScope('duplicates', 'comparison');
    first.filterScopes.dispose();

    expect(stored('filterScope')).toEqual({ duplicates: 'comparison' });
    const { scopeFor } = await loadFilters();

    expect(scopeFor('duplicates')).toBe('comparison');
  });

  it('ignores a stored value it does not know', async () => {
    localStorage.setItem('opm.v1.filterScope', JSON.stringify({ duplicates: 'everything' }));

    const { scopeFor } = await loadFilters();

    expect(scopeFor('duplicates')).toBe('both');
  });

  it('writes nothing when the scope is already the stored one', async () => {
    const { setScope } = await loadFilters();
    setScope('duplicates', 'source');
    const setItem = vi.spyOn(Storage.prototype, 'setItem');

    setScope('duplicates', 'source');

    expect(setItem).not.toHaveBeenCalled();
    setItem.mockRestore();
  });

  it('notifies when the scope changes', async () => {
    const { scopeFor, setScope } = await loadFilters();
    const seen: string[] = [];
    const stop = effect(() => {
      seen.push(scopeFor('duplicates'));
    });

    setScope('duplicates', 'source');
    stop();

    expect(seen).toEqual(['both', 'source']);
  });
});
