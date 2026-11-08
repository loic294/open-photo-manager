import { describe, expect, it } from 'vitest';
import type { Filter } from '@/api/generated/Filter';
import {
  activeFilterCount,
  DEFAULT_SCOPE_LABELS,
  EMPTY_FILTER,
  FILTER_SCOPES,
  filterKey,
  isFilterActive,
  normalizeFilter,
  sameFilter,
  sameText,
  textKey,
  toggleIn,
} from '@/util/filter';

function filter(overrides: Partial<Filter> = {}): Filter {
  return { ...EMPTY_FILTER, ...overrides };
}

describe('EMPTY_FILTER', () => {
  it('constrains nothing', () => {
    expect(EMPTY_FILTER).toEqual({
      cameras: [],
      ratings: [],
      flags: [],
      tags: [],
      selection_id: null,
    });
    expect(isFilterActive(EMPTY_FILTER)).toBe(false);
    expect(activeFilterCount(EMPTY_FILTER)).toBe(0);
  });

  it('is frozen, lists included, so it cannot be edited by mistake', () => {
    expect(Object.isFrozen(EMPTY_FILTER)).toBe(true);
    for (const list of [
      EMPTY_FILTER.cameras,
      EMPTY_FILTER.ratings,
      EMPTY_FILTER.flags,
      EMPTY_FILTER.tags,
    ]) {
      expect(Object.isFrozen(list)).toBe(true);
    }
    expect(() => EMPTY_FILTER.cameras.push('x')).toThrow(TypeError);
  });

  it('can be copied and extended (the hand-off sets only selection_id)', () => {
    const next = { ...EMPTY_FILTER, selection_id: 7 };

    expect(next.selection_id).toBe(7);
    expect(EMPTY_FILTER.selection_id).toBeNull();
    expect(Object.isFrozen(next)).toBe(false);
  });
});

describe('textKey and sameText', () => {
  it.each([
    ['Canon EOS R5', 'canon eos r5'],
    ['CANON', 'canon'],
    ['\u00e9clair', 'e\u0301clair'],
    ['Stra\u00dfe', 'stra\u00dfe'],
  ])('%j and %j are one name', (a, b) => {
    expect(sameText(a, b)).toBe(true);
    expect(textKey(a)).toBe(textKey(b));
  });

  it.each([
    ['Canon', 'Canon '],
    ['Canon EOS', 'Canon  EOS'],
    ['Canon', 'Nikon'],
  ])('%j and %j are different names (inner and outer spaces count)', (a, b) => {
    expect(sameText(a, b)).toBe(false);
  });
});

describe('normalizeFilter', () => {
  it.each<[string, Partial<Filter>, Partial<Filter>]>([
    [
      'sorts names',
      { cameras: ['Sony A7', 'Canon R5', 'Nikon Z6'] },
      { cameras: ['Canon R5', 'Nikon Z6', 'Sony A7'] },
    ],
    [
      'sorts without regard to case',
      { tags: ['beta', 'Alpha', 'Gamma'] },
      { tags: ['Alpha', 'beta', 'Gamma'] },
    ],
    ['trims names', { tags: ['  sunset  '] }, { tags: ['sunset'] }],
    ['keeps inner spaces', { cameras: ['Canon  EOS'] }, { cameras: ['Canon  EOS'] }],
    ['drops blank names', { tags: ['', '   ', 'a', '\t'] }, { tags: ['a'] }],
    ['a list of only blanks is empty', { cameras: ['', ' '] }, { cameras: [] }],
    [
      'keeps the first spelling of a duplicate',
      { cameras: ['Canon R5', 'canon r5', 'CANON R5'] },
      { cameras: ['Canon R5'] },
    ],
    [
      'keeps the first spelling whatever the order',
      { cameras: ['canon r5', 'Canon R5'] },
      { cameras: ['canon r5'] },
    ],
    [
      'treats NFC and NFD as one name',
      { tags: ['\u00e9t\u00e9', 'e\u0301te\u0301'] },
      { tags: ['\u00e9t\u00e9'] },
    ],
    ['sorts ratings ascending', { ratings: [5, 1, 3] }, { ratings: [1, 3, 5] }],
    ['de-duplicates ratings', { ratings: [2, 2, 2, 0, 0] }, { ratings: [0, 2] }],
    ['keeps 0 (unrated) and 5', { ratings: [5, 0] }, { ratings: [0, 5] }],
    ['drops ratings above 5', { ratings: [6, 5, 100] }, { ratings: [5] }],
    ['drops negative ratings', { ratings: [-1, 1] }, { ratings: [1] }],
    ['drops fractional ratings', { ratings: [2.5, 3] }, { ratings: [3] }],
    [
      'drops NaN and infinite ratings',
      { ratings: [Number.NaN, Number.POSITIVE_INFINITY, 4] },
      { ratings: [4] },
    ],
    [
      'puts flags in the fixed order',
      { flags: ['rejected', 'selected'] },
      { flags: ['selected', 'rejected'] },
    ],
    ['de-duplicates flags', { flags: ['rejected', 'rejected'] }, { flags: ['rejected'] }],
    ['drops unknown flags', { flags: ['selected', 'bogus' as never] }, { flags: ['selected'] }],
    ['keeps a selection id', { selection_id: 12 }, { selection_id: 12 }],
    ['drops a missing selection id', { selection_id: undefined as never }, { selection_id: null }],
    ['drops a fractional selection id', { selection_id: 1.5 }, { selection_id: null }],
  ])('%s', (_name, input, expected) => {
    expect(normalizeFilter(filter(input))).toEqual(filter(expected));
  });

  it('is idempotent', () => {
    const messy = filter({
      cameras: ['b', 'A', 'a', ' '],
      ratings: [4, 2, 9, 4],
      flags: ['rejected', 'selected'],
      tags: ['X', 'x', 'y'],
      selection_id: 3,
    });
    const once = normalizeFilter(messy);

    expect(normalizeFilter(once)).toEqual(once);
  });

  it('does not touch its input', () => {
    const input = filter({ cameras: ['b', 'a'], ratings: [3, 1], flags: ['rejected', 'selected'] });
    const snapshot = structuredClone(input);

    normalizeFilter(input);

    expect(input).toEqual(snapshot);
  });

  it('returns lists that can be edited, even for the frozen empty filter', () => {
    const copy = normalizeFilter(EMPTY_FILTER);

    expect(Object.isFrozen(copy.cameras)).toBe(false);
    expect(() => copy.cameras.push('x')).not.toThrow();
    expect(EMPTY_FILTER.cameras).toEqual([]);
  });
});

describe('activeFilterCount and isFilterActive', () => {
  it.each<[string, Partial<Filter>, number]>([
    ['nothing', {}, 0],
    ['cameras', { cameras: ['a'] }, 1],
    ['two cameras are still one dimension', { cameras: ['a', 'b'] }, 1],
    ['ratings', { ratings: [1, 2, 3] }, 1],
    ['flags', { flags: ['selected'] }, 1],
    ['tags', { tags: ['a', 'b'] }, 1],
    ['the selection counts as one', { selection_id: 4 }, 1],
    ['blank names do not count', { cameras: [' '], tags: [''] }, 0],
    ['invalid ratings do not count', { ratings: [9] }, 0],
    ['two dimensions', { cameras: ['a'], ratings: [5] }, 2],
    [
      'all five',
      { cameras: ['a'], ratings: [5], flags: ['rejected'], tags: ['t'], selection_id: 1 },
      5,
    ],
  ])('%s: %i', (_name, overrides, count) => {
    expect(activeFilterCount(filter(overrides))).toBe(count);
    expect(isFilterActive(filter(overrides))).toBe(count > 0);
  });
});

describe('filterKey and sameFilter', () => {
  it('is stable for the same filter', () => {
    const f = filter({ cameras: ['a'], ratings: [1] });

    expect(filterKey(f)).toBe(filterKey(structuredClone(f)));
    expect(typeof filterKey(f)).toBe('string');
  });

  it.each<[string, Partial<Filter>, Partial<Filter>]>([
    ['order of names', { cameras: ['b', 'a'] }, { cameras: ['a', 'b'] }],
    ['order of ratings', { ratings: [3, 1] }, { ratings: [1, 3] }],
    ['order of flags', { flags: ['rejected', 'selected'] }, { flags: ['selected', 'rejected'] }],
    ['duplicates', { tags: ['a', 'a'] }, { tags: ['a'] }],
    ['blank names', { tags: ['a', ' '] }, { tags: ['a'] }],
    ['surrounding spaces', { tags: [' a '] }, { tags: ['a'] }],
    ['an invalid rating', { ratings: [1, 7] }, { ratings: [1] }],
    ['the case of a camera', { cameras: ['Canon EOS R5'] }, { cameras: ['canon eos r5'] }],
    ['the case of a tag', { tags: ['Trip'] }, { tags: ['TRIP'] }],
    ['order of spellings', { tags: ['Trip', 'TRIP'] }, { tags: ['TRIP', 'Trip'] }],
    ['the Unicode form of a tag', { tags: ['Cafe\u0301'] }, { tags: ['caf\u00e9'] }],
  ])('ignores the %s', (_name, a, b) => {
    expect(filterKey(filter(a))).toBe(filterKey(filter(b)));
    expect(sameFilter(filter(a), filter(b))).toBe(true);
  });

  it.each<[string, Partial<Filter>, Partial<Filter>]>([
    ['another camera', { cameras: ['a'] }, { cameras: ['b'] }],
    ['a camera or a tag with the same name', { cameras: ['a'] }, { tags: ['a'] }],
    ['another rating', { ratings: [1] }, { ratings: [2] }],
    ['unrated or nothing', { ratings: [0] }, {}],
    ['another flag', { flags: ['selected'] }, { flags: ['rejected'] }],
    ['a selection', { selection_id: 1 }, { selection_id: 2 }],
    ['a selection or none', { selection_id: 1 }, {}],
  ])('tells apart %s', (_name, a, b) => {
    expect(filterKey(filter(a))).not.toBe(filterKey(filter(b)));
    expect(sameFilter(filter(a), filter(b))).toBe(false);
  });
});

describe('toggleIn', () => {
  it('appends a value that is not in the list', () => {
    expect(toggleIn(['a'], 'b')).toEqual(['a', 'b']);
  });

  it('removes a value that is in the list', () => {
    expect(toggleIn(['a', 'b', 'c'], 'b')).toEqual(['a', 'c']);
  });

  it('works on numbers and from the empty list', () => {
    expect(toggleIn([], 3)).toEqual([3]);
    expect(toggleIn([3], 3)).toEqual([]);
  });

  it('returns a new list and keeps the input', () => {
    const list = Object.freeze(['a']);

    const next = toggleIn(list, 'b');

    expect(next).not.toBe(list);
    expect(list).toEqual(['a']);
  });

  it('compares with `same` when given, removing every match', () => {
    expect(toggleIn(['Canon', 'canon', 'Nikon'], 'CANON', sameText)).toEqual(['Nikon']);
    expect(toggleIn(['Nikon'], 'CANON', sameText)).toEqual(['Nikon', 'CANON']);
  });
});

describe('scopes', () => {
  it('lists the three scopes with default labels for each', () => {
    expect(FILTER_SCOPES).toEqual(['both', 'source', 'comparison']);
    expect(Object.keys(DEFAULT_SCOPE_LABELS).sort()).toEqual([...FILTER_SCOPES].sort());
  });
});
