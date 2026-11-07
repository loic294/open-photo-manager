import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

type Prefs = typeof import('@/stores/prefs');

let loaded: Prefs | undefined;

async function loadPrefs(): Promise<Prefs> {
  vi.resetModules();
  loaded = await import('@/stores/prefs');
  return loaded;
}

function stored(key: string): unknown {
  const raw = localStorage.getItem(`apm.v1.${key}`);
  return raw === null ? undefined : JSON.parse(raw);
}

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  if (loaded) {
    for (const store of [
      loaded.theme,
      loaded.gridSize,
      loaded.hideUnmatchedFolders,
      loaded.lastAction,
      loaded.sort,
    ]) {
      store.dispose();
    }
    loaded = undefined;
  }
});

describe('defaults', () => {
  it('starts with documented defaults and writes nothing', async () => {
    const prefs = await loadPrefs();

    expect(prefs.theme.value).toBe('system');
    expect(prefs.gridSize.value).toEqual({ source: 160, comparison: 160 });
    expect(prefs.hideUnmatchedFolders.value).toBe(false);
    expect(prefs.lastAction.value).toEqual({});
    expect(prefs.sort.value).toEqual({
      source: { key: 'name', order: 'asc' },
      comparison: { key: 'name', order: 'asc' },
    });
    expect(localStorage.length).toBe(0);
  });
});

describe('theme', () => {
  it('persists the choice', async () => {
    const prefs = await loadPrefs();

    prefs.theme.value = 'dark';

    expect(stored('theme')).toBe('dark');
  });

  it('restores a stored choice', async () => {
    localStorage.setItem('apm.v1.theme', '"light"');

    expect((await loadPrefs()).theme.value).toBe('light');
  });

  it.each(['"blue"', '3', 'null', '{bad'])('ignores the stored value %s', async (value) => {
    localStorage.setItem('apm.v1.theme', value);

    expect((await loadPrefs()).theme.value).toBe('system');
  });

  it('lists exactly the supported choices', async () => {
    expect((await loadPrefs()).THEMES).toEqual(['system', 'light', 'dark']);
  });
});

describe('gridSize', () => {
  it('is per pane and persisted', async () => {
    const prefs = await loadPrefs();

    prefs.setGridSize('comparison', 224);

    expect(prefs.gridSize.value).toEqual({ source: 160, comparison: 224 });
    expect(stored('gridSize')).toEqual({ source: 160, comparison: 224 });
  });

  it('clamps to the slider range and rounds to whole pixels', async () => {
    const prefs = await loadPrefs();

    prefs.setGridSize('source', 5);
    expect(prefs.gridSize.value.source).toBe(prefs.GRID_SIZE.min);

    prefs.setGridSize('source', 9000);
    expect(prefs.gridSize.value.source).toBe(prefs.GRID_SIZE.max);

    prefs.setGridSize('source', 130.6);
    expect(prefs.gridSize.value.source).toBe(131);
  });

  it('ignores values that are not numbers', async () => {
    const prefs = await loadPrefs();

    prefs.setGridSize('source', Number.NaN);
    prefs.setGridSize('source', Number.POSITIVE_INFINITY);

    expect(prefs.gridSize.value.source).toBe(160);
  });

  it('does not notify for an unchanged size', async () => {
    const prefs = await loadPrefs();
    const before = prefs.gridSize.value;

    prefs.setGridSize('source', 160);

    expect(prefs.gridSize.value).toBe(before);
  });

  it.each([
    ['an out-of-range size', '{"source":10,"comparison":160}'],
    ['a missing pane', '{"source":160}'],
    ['an extra key', '{"source":160,"comparison":160,"third":1}'],
    ['a number', '160'],
  ])('falls back on %s', async (_label, value) => {
    localStorage.setItem('apm.v1.gridSize', value);

    expect((await loadPrefs()).gridSize.value).toEqual({ source: 160, comparison: 160 });
  });
});

describe('hideUnmatchedFolders', () => {
  it('persists and restores', async () => {
    const first = await loadPrefs();
    first.hideUnmatchedFolders.value = true;
    expect(stored('hideUnmatchedFolders')).toBe(true);

    expect((await loadPrefs()).hideUnmatchedFolders.value).toBe(true);
  });

  it('ignores a stored value that is not a boolean', async () => {
    localStorage.setItem('apm.v1.hideUnmatchedFolders', '"yes"');

    expect((await loadPrefs()).hideUnmatchedFolders.value).toBe(false);
  });
});

describe('lastAction', () => {
  it('remembers the last choice per tool', async () => {
    const prefs = await loadPrefs();

    expect(prefs.lastActionOf('duplicates')).toBeUndefined();
    prefs.setLastAction('duplicates', 'keep-newest');
    prefs.setLastAction('move', 'copy');
    prefs.setLastAction('duplicates', 'keep-oldest');

    expect(prefs.lastActionOf('duplicates')).toBe('keep-oldest');
    expect(prefs.lastActionOf('move')).toBe('copy');
    expect(stored('lastAction')).toEqual({ duplicates: 'keep-oldest', move: 'copy' });
  });

  it('does not rewrite an unchanged choice', async () => {
    const prefs = await loadPrefs();
    prefs.setLastAction('move', 'copy');
    const before = prefs.lastAction.value;

    prefs.setLastAction('move', 'copy');

    expect(prefs.lastAction.value).toBe(before);
  });

  it('ignores stored values that are not strings', async () => {
    localStorage.setItem('apm.v1.lastAction', '{"move":3}');

    expect((await loadPrefs()).lastAction.value).toEqual({});
  });
});

describe('sort', () => {
  it('is independent per pane and persisted', async () => {
    const prefs = await loadPrefs();

    prefs.setSort('comparison', { key: 'size', order: 'desc' });

    expect(prefs.sort.value.source).toEqual({ key: 'name', order: 'asc' });
    expect(prefs.sort.value.comparison).toEqual({ key: 'size', order: 'desc' });
    expect(stored('sort')).toEqual({
      source: { key: 'name', order: 'asc' },
      comparison: { key: 'size', order: 'desc' },
    });
  });

  it('restores a stored sort', async () => {
    localStorage.setItem(
      'apm.v1.sort',
      '{"source":{"key":"date","order":"desc"},"comparison":{"key":"size","order":"asc"}}',
    );

    const prefs = await loadPrefs();

    expect(prefs.sort.value.source).toEqual({ key: 'date', order: 'desc' });
    expect(prefs.sort.value.comparison).toEqual({ key: 'size', order: 'asc' });
  });

  it.each([
    [
      'an unknown key',
      '{"source":{"key":"rating","order":"asc"},"comparison":{"key":"name","order":"asc"}}',
    ],
    [
      'an unknown order',
      '{"source":{"key":"name","order":"up"},"comparison":{"key":"name","order":"asc"}}',
    ],
    ['a missing pane', '{"source":{"key":"name","order":"asc"}}'],
    ['a string', '"name"'],
  ])('falls back on %s', async (_label, value) => {
    localStorage.setItem('apm.v1.sort', value);

    expect((await loadPrefs()).sort.value.source).toEqual({ key: 'name', order: 'asc' });
  });

  it('stores a copy, so later edits of the argument do not leak in', async () => {
    const prefs = await loadPrefs();
    const next = { key: 'date', order: 'desc' } as const;

    prefs.setSort('source', next);

    expect(prefs.sort.value.source).not.toBe(next);
    expect(prefs.sort.value.source).toEqual(next);
  });

  it('lists the keys and orders the backend understands', async () => {
    const prefs = await loadPrefs();

    expect(prefs.SORT_KEYS).toEqual(['name', 'date', 'size']);
    expect(prefs.SORT_ORDERS).toEqual(['asc', 'desc']);
  });
});

describe('cross-tab sync', () => {
  it('applies a change made in another tab', async () => {
    const prefs = await loadPrefs();

    localStorage.setItem('apm.v1.theme', '"dark"');
    window.dispatchEvent(
      new StorageEvent('storage', {
        key: 'apm.v1.theme',
        newValue: '"dark"',
        storageArea: localStorage,
      }),
    );

    expect(prefs.theme.value).toBe('dark');
  });
});
