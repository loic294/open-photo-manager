import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  dictionaryOf,
  isBoolean,
  isString,
  numberBetween,
  oneOf,
  type PersistedSignal,
  persistedSignal,
  recordOf,
  STORAGE_PREFIX,
} from '@/stores/persisted';

const created: PersistedSignal<unknown>[] = [];

function persisted<T>(...args: Parameters<typeof persistedSignal<T>>): PersistedSignal<T> {
  const value = persistedSignal(...args);
  created.push(value as PersistedSignal<unknown>);
  return value;
}

const isColor = oneOf(['red', 'green'] as const);

function remoteChange(key: string | null, newValue: string | null, area: Storage = localStorage) {
  window.dispatchEvent(new StorageEvent('storage', { key, newValue, storageArea: area }));
}

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  for (const value of created.splice(0)) value.dispose();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('persistedSignal', () => {
  it('uses the versioned key prefix', () => {
    expect(STORAGE_PREFIX).toBe('apm.v1.');
  });

  it('starts from the fallback and does not write it back', () => {
    const color = persisted('color', 'red', isColor);

    expect(color.value).toBe('red');
    expect(localStorage.getItem('apm.v1.color')).toBeNull();
  });

  it('starts from a valid stored value', () => {
    localStorage.setItem('apm.v1.color', '"green"');

    expect(persisted('color', 'red', isColor).value).toBe('green');
  });

  it.each([
    ['a value that fails validation', '"blue"'],
    ['corrupt JSON', '{nope'],
    ['a value of the wrong type', '12'],
  ])('falls back on %s', (_label, stored) => {
    localStorage.setItem('apm.v1.color', stored);

    expect(persisted('color', 'red', isColor).value).toBe('red');
  });

  it('writes every assignment as JSON', () => {
    const color = persisted('color', 'red', isColor);

    color.value = 'green';

    expect(localStorage.getItem('apm.v1.color')).toBe('"green"');
  });

  it('round-trips object values', () => {
    const guard = dictionaryOf(isString);
    const map = persisted<Record<string, string>>('map', {}, guard);

    map.value = { ...map.value, browse: 'open' };

    expect(JSON.parse(localStorage.getItem('apm.v1.map') ?? 'null')).toEqual({ browse: 'open' });
    expect(persisted('map', {}, guard).value).toEqual({ browse: 'open' });
  });
});

describe('cross-tab sync', () => {
  it('applies another tab\u2019s change without writing it back', () => {
    const color = persisted('color', 'red', isColor);
    color.value = 'green';
    const setItem = vi.spyOn(Storage.prototype, 'setItem');

    remoteChange('apm.v1.color', '"red"');

    expect(color.value).toBe('red');
    expect(setItem).not.toHaveBeenCalled();
  });

  it('falls back when the other tab wrote something invalid or removed the key', () => {
    const color = persisted('color', 'red', isColor);
    color.value = 'green';

    remoteChange('apm.v1.color', '"blue"');
    expect(color.value).toBe('red');

    color.value = 'green';
    remoteChange('apm.v1.color', null);
    expect(color.value).toBe('red');
  });

  it('resets to the fallback when storage is cleared', () => {
    const color = persisted('color', 'red', isColor);
    color.value = 'green';

    remoteChange(null, null);

    expect(color.value).toBe('red');
  });

  it('ignores other keys and other storage areas', () => {
    const color = persisted('color', 'red', isColor);

    remoteChange('apm.v1.other', '"green"');
    remoteChange('apm.v1.color', '"green"', sessionStorage);

    expect(color.value).toBe('red');
  });

  it('does not notify subscribers when the remote value is unchanged', () => {
    const map = persisted('map', { a: 'b' }, dictionaryOf(isString));
    const seen = vi.fn();
    map.subscribe(seen);
    seen.mockClear();

    remoteChange('apm.v1.map', '{"a":"b"}');

    expect(seen).not.toHaveBeenCalled();
  });

  it('stops syncing after dispose()', () => {
    const color = persisted('color', 'red', isColor);
    color.dispose();

    remoteChange('apm.v1.color', '"green"');
    color.value = 'green';

    expect(color.value).toBe('green');
    expect(localStorage.getItem('apm.v1.color')).toBeNull();
  });
});

describe('when storage is unavailable', () => {
  it('keeps working in memory when reading localStorage throws', () => {
    vi.spyOn(window, 'localStorage', 'get').mockImplementation(() => {
      throw new DOMException('denied', 'SecurityError');
    });

    const color = persisted('color', 'red', isColor);
    color.value = 'green';

    expect(color.value).toBe('green');
  });

  it('keeps the in-memory value when the quota is exceeded', () => {
    vi.stubGlobal('localStorage', {
      getItem: () => null,
      setItem: () => {
        throw new DOMException('full', 'QuotaExceededError');
      },
    });

    const color = persisted('color', 'red', isColor);
    color.value = 'green';

    expect(color.value).toBe('green');
  });

  it('keeps the fallback when getItem throws', () => {
    vi.stubGlobal('localStorage', {
      getItem: () => {
        throw new Error('broken');
      },
      setItem: () => {},
    });

    expect(persisted('color', 'red', isColor).value).toBe('red');
  });
});

describe('validators', () => {
  it('oneOf accepts only the listed strings', () => {
    expect(isColor('red')).toBe(true);
    expect(isColor('blue')).toBe(false);
    expect(isColor(1)).toBe(false);
  });

  it('numberBetween is inclusive and rejects NaN, Infinity and non-numbers', () => {
    const guard = numberBetween(1, 5);
    expect([1, 5, 3.5].every(guard)).toBe(true);
    expect([0, 6, Number.NaN, Number.POSITIVE_INFINITY, '3', null].some(guard)).toBe(false);
  });

  it('recordOf needs exactly the listed keys with valid values', () => {
    const guard = recordOf(['a', 'b'], isBoolean);
    expect(guard({ a: true, b: false })).toBe(true);
    expect(guard({ a: true })).toBe(false);
    expect(guard({ a: true, b: 1 })).toBe(false);
    expect(guard({ a: true, b: true, c: true })).toBe(false);
    expect(guard([true, false])).toBe(false);
    expect(guard(null)).toBe(false);
  });

  it('dictionaryOf accepts any keys with valid values', () => {
    const guard = dictionaryOf(isString);
    expect(guard({})).toBe(true);
    expect(guard({ x: 'y' })).toBe(true);
    expect(guard({ x: 1 })).toBe(false);
    expect(guard([])).toBe(false);
    expect(guard(null)).toBe(false);
  });
});
