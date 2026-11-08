import { effect } from '@preact/signals-core';
import { afterEach, describe, expect, it } from 'vitest';
import {
  handoffFor,
  pinnedBase,
  recursiveFor,
  resetToolState,
  resetToolStateForTests,
  setHandoff,
  setPinnedBase,
  setRecursive,
} from '@/stores/tool-state';

afterEach(() => {
  resetToolStateForTests();
});

/** The values `read` returns each time a signal it reads changes, starting with the first read. */
function watch<T>(read: () => T): { seen: T[]; stop: () => void } {
  const seen: T[] = [];
  const stop = effect(() => {
    seen.push(read());
  });
  return { seen, stop };
}

describe('pinnedBase', () => {
  it('is null until a base is pinned', () => {
    expect(pinnedBase('move', 'comparison')).toBeNull();
  });

  it('keeps one base per tool and pane', () => {
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: '2024' });
    setPinnedBase('move', 'source', { volume: 'backup', dir: '' });

    expect(pinnedBase('move', 'comparison')).toEqual({ volume: 'photos', dir: '2024' });
    expect(pinnedBase('move', 'source')).toEqual({ volume: 'backup', dir: '' });
    expect(pinnedBase('format', 'comparison')).toBeNull();
  });

  it('keeps a copy, so the caller cannot change it afterwards', () => {
    const base = { volume: 'photos', dir: '2024' };
    setPinnedBase('move', 'comparison', base);
    base.dir = 'elsewhere';

    expect(pinnedBase('move', 'comparison')).toEqual({ volume: 'photos', dir: '2024' });
  });

  it('clears the base with null', () => {
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: '2024' });
    setPinnedBase('move', 'comparison', null);

    expect(pinnedBase('move', 'comparison')).toBeNull();
  });

  it('replaces the base', () => {
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: '2024' });
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: '2025' });

    expect(pinnedBase('move', 'comparison')).toEqual({ volume: 'photos', dir: '2025' });
  });

  it('notifies readers only when the base changes', () => {
    const { seen, stop } = watch(() => pinnedBase('move', 'comparison'));
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: '2024' });
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: '2024' });
    setPinnedBase('move', 'comparison', null);
    setPinnedBase('move', 'comparison', null);
    stop();

    expect(seen).toEqual([null, { volume: 'photos', dir: '2024' }, null]);
  });

  it('keeps the same object while the base does not change', () => {
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: '2024' });
    const first = pinnedBase('move', 'comparison');
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: '2024' });
    setPinnedBase('move', 'source', { volume: 'photos', dir: '' });

    expect(pinnedBase('move', 'comparison')).toBe(first);
  });
});

describe('recursiveFor', () => {
  it('is on until the user turns it off', () => {
    expect(recursiveFor('duplicates')).toBe(true);
  });

  it('is kept per tool', () => {
    setRecursive('duplicates', false);

    expect(recursiveFor('duplicates')).toBe(false);
    expect(recursiveFor('move')).toBe(true);
  });

  it('turns back on', () => {
    setRecursive('duplicates', false);
    setRecursive('duplicates', true);

    expect(recursiveFor('duplicates')).toBe(true);
  });

  it('notifies readers only when the switch changes', () => {
    const { seen, stop } = watch(() => recursiveFor('duplicates'));
    setRecursive('duplicates', true);
    setRecursive('duplicates', false);
    setRecursive('duplicates', false);
    stop();

    expect(seen).toEqual([true, false]);
  });
});

describe('handoffFor', () => {
  it('is null until a hand-off is set', () => {
    expect(handoffFor('duplicates')).toBeNull();
  });

  it('keeps a copy of the hand-off per tool', () => {
    const handoff = { label: '12 selected files' };
    setHandoff('duplicates', handoff);
    handoff.label = 'changed';

    expect(handoffFor('duplicates')).toEqual({ label: '12 selected files' });
    expect(handoffFor('move')).toBeNull();
  });

  it('clears the hand-off with null', () => {
    setHandoff('duplicates', { label: '12 selected files' });
    setHandoff('duplicates', null);

    expect(handoffFor('duplicates')).toBeNull();
  });

  it('notifies readers only when the hand-off changes', () => {
    const { seen, stop } = watch(() => handoffFor('duplicates'));
    setHandoff('duplicates', { label: 'a' });
    setHandoff('duplicates', { label: 'a' });
    setHandoff('duplicates', { label: 'b' });
    setHandoff('duplicates', null);
    setHandoff('duplicates', null);
    stop();

    expect(seen).toEqual([null, { label: 'a' }, { label: 'b' }, null]);
  });
});

describe('resetToolState', () => {
  it('forgets the bases, the switch and the hand-off of one tool only', () => {
    setPinnedBase('move', 'source', { volume: 'photos', dir: 'a' });
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: 'b' });
    setRecursive('move', false);
    setHandoff('move', { label: 'x' });
    setPinnedBase('format', 'comparison', { volume: 'photos', dir: 'c' });
    setRecursive('format', false);
    setHandoff('format', { label: 'y' });

    resetToolState('move');

    expect(pinnedBase('move', 'source')).toBeNull();
    expect(pinnedBase('move', 'comparison')).toBeNull();
    expect(recursiveFor('move')).toBe(true);
    expect(handoffFor('move')).toBeNull();
    expect(pinnedBase('format', 'comparison')).toEqual({ volume: 'photos', dir: 'c' });
    expect(recursiveFor('format')).toBe(false);
    expect(handoffFor('format')).toEqual({ label: 'y' });
  });

  it('notifies a reader once', () => {
    setPinnedBase('move', 'source', { volume: 'photos', dir: 'a' });
    setPinnedBase('move', 'comparison', { volume: 'photos', dir: 'b' });
    const { seen, stop } = watch(
      () => `${pinnedBase('move', 'source')?.dir}/${pinnedBase('move', 'comparison')?.dir}`,
    );
    resetToolState('move');
    stop();

    expect(seen).toEqual(['a/b', 'undefined/undefined']);
  });
});

describe('resetToolStateForTests', () => {
  it('forgets every tool', () => {
    setPinnedBase('move', 'source', { volume: 'photos', dir: 'a' });
    setRecursive('format', false);
    setHandoff('duplicates', { label: 'x' });

    resetToolStateForTests();

    expect(pinnedBase('move', 'source')).toBeNull();
    expect(recursiveFor('format')).toBe(true);
    expect(handoffFor('duplicates')).toBeNull();
  });
});
