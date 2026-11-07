import { effect } from '@preact/signals-core';
import { afterEach, describe, expect, it } from 'vitest';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import {
  activePane,
  clearAllSelections,
  type IdsProvider,
  PaneSelection,
  resetSelectionForTests,
  selection,
  selectionContextKey,
} from '@/stores/selection';
import { clearToasts, toasts } from '@/stores/toasts';

const LOAD_FAILED = 'Could not load the list of items';

const stops: Array<() => void> = [];

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  resetSelectionForTests();
  clearToasts();
});

function range(from: number, to: number): number[] {
  return Array.from({ length: to - from + 1 }, (_, index) => from + index);
}

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason: unknown) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve: (value: T) => void = () => undefined;
  let reject: (reason: unknown) => void = () => undefined;
  const promise = new Promise<T>((settle, fail) => {
    resolve = settle;
    reject = fail;
  });
  return { promise, resolve, reject };
}

interface Call {
  signal: AbortSignal;
  answer: Deferred<number[]>;
}

/** A provider that waits for the test to answer each call. */
function controlled(): { provider: IdsProvider; calls: Call[] } {
  const calls: Call[] = [];
  const provider: IdsProvider = (signal) => {
    const answer = deferred<number[]>();
    calls.push({ signal, answer });
    return answer.promise;
  };
  return { provider, calls };
}

/** A selection whose provider answers at once: call n gets list n (the last list repeats). */
function answering(...lists: number[][]): { pane: PaneSelection; signals: AbortSignal[] } {
  const signals: AbortSignal[] = [];
  const pane = new PaneSelection();
  pane.setIdsProvider(async (signal) => {
    signals.push(signal);
    return lists[Math.min(signals.length, lists.length) - 1] ?? [];
  });
  return { pane, signals };
}

/** A selection whose provider waits for the test. */
function waiting(): { pane: PaneSelection; calls: Call[] } {
  const { provider, calls } = controlled();
  const pane = new PaneSelection();
  pane.setIdsProvider(provider);
  return { pane, calls };
}

function members(pane: PaneSelection): number[] {
  return [...pane.ids.value].sort((a, b) => a - b);
}

/** How many times an effect reading `read` has run, the first run included. */
function countRuns(read: () => unknown): () => number {
  let runs = 0;
  stops.push(
    effect(() => {
      read();
      runs += 1;
    }),
  );
  return () => runs;
}

function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function toastMessages(): string[] {
  return toasts.value.map((item) => item.message);
}

describe('PaneSelection: synchronous changes', () => {
  it('starts empty', () => {
    const pane = new PaneSelection();
    expect(pane.ids.value.size).toBe(0);
    expect(pane.count.value).toBe(0);
    expect(pane.active.value).toBe(false);
    expect(pane.busy.value).toBe(false);
    expect(pane.toArray()).toEqual([]);
    expect(pane.has(1)).toBe(false);
  });

  it('toggle selects, then deselects, and replaces the set each time', () => {
    const pane = new PaneSelection();
    const empty = pane.ids.value;
    pane.toggle(4);
    const one = pane.ids.value;
    expect(one).not.toBe(empty);
    expect([...empty]).toEqual([]);
    expect([...one]).toEqual([4]);
    expect(pane.has(4)).toBe(true);
    pane.toggle(4);
    expect(pane.ids.value).not.toBe(one);
    expect([...one]).toEqual([4]);
    expect(pane.ids.value.size).toBe(0);
    expect(pane.has(4)).toBe(false);
  });

  it('add keeps the current ids, replaces the set and does not mutate the old one', () => {
    const pane = new PaneSelection();
    pane.toggle(1);
    const before = pane.ids.value;
    pane.add([2, 3]);
    expect(pane.ids.value).not.toBe(before);
    expect([...before]).toEqual([1]);
    expect(members(pane)).toEqual([1, 2, 3]);
  });

  it('add writes nothing when every id is already selected', () => {
    const pane = new PaneSelection();
    pane.add([1, 2]);
    const before = pane.ids.value;
    pane.add([2, 1]);
    pane.add([]);
    expect(pane.ids.value).toBe(before);
  });

  it('set replaces the selection with any iterable', () => {
    const pane = new PaneSelection();
    pane.add([1, 2]);
    const before = pane.ids.value;
    pane.set(new Set([8, 9]));
    expect(pane.ids.value).not.toBe(before);
    expect([...before]).toEqual([1, 2]);
    expect(members(pane)).toEqual([8, 9]);
  });

  it('set writes nothing when the members are the same', () => {
    const pane = new PaneSelection();
    pane.add([1, 2]);
    const before = pane.ids.value;
    pane.set([2, 1]);
    expect(pane.ids.value).toBe(before);
  });

  it('clear empties the selection with a new set, and writes nothing when already empty', () => {
    const pane = new PaneSelection();
    pane.add([1, 2]);
    const before = pane.ids.value;
    pane.clear();
    expect(pane.ids.value).not.toBe(before);
    expect([...before]).toEqual([1, 2]);
    expect(pane.ids.value.size).toBe(0);
    const empty = pane.ids.value;
    pane.clear();
    expect(pane.ids.value).toBe(empty);
  });

  it('count and active follow the selection', () => {
    const pane = new PaneSelection();
    pane.add([5, 6, 7]);
    expect(pane.count.value).toBe(3);
    expect(pane.active.value).toBe(true);
    pane.toggle(5);
    expect(pane.count.value).toBe(2);
    pane.clear();
    expect(pane.count.value).toBe(0);
    expect(pane.active.value).toBe(false);
  });

  it('toArray lists the ids in the order they were selected', () => {
    const pane = new PaneSelection();
    pane.toggle(9);
    pane.toggle(2);
    pane.add([5, 1]);
    expect(pane.toArray()).toEqual([9, 2, 5, 1]);
  });

  it('every change is one signal write, even for a 50k-item add', () => {
    const pane = new PaneSelection();
    const runs = countRuns(() => pane.ids.value);
    expect(runs()).toBe(1);
    pane.add(range(1, 50_000));
    expect(runs()).toBe(2);
    pane.toggle(7);
    expect(runs()).toBe(3);
    pane.set(range(1, 10));
    expect(runs()).toBe(4);
    pane.clear();
    expect(runs()).toBe(5);
  });

  it('has() subscribes the reader', () => {
    const pane = new PaneSelection();
    const seen: boolean[] = [];
    stops.push(
      effect(() => {
        seen.push(pane.has(3));
      }),
    );
    pane.toggle(3);
    pane.toggle(3);
    expect(seen).toEqual([false, true, false]);
  });
});

describe('PaneSelection: the anchor', () => {
  it('toggle makes the item the anchor of the next range', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(3);
    await pane.rangeTo(6);
    expect(members(pane)).toEqual([3, 4, 5, 6]);
  });

  it('a deselecting toggle moves the anchor too', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(3);
    pane.toggle(3);
    expect(pane.count.value).toBe(0);
    await pane.rangeTo(5);
    expect(members(pane)).toEqual([3, 4, 5]);
  });

  it('add leaves the anchor where it was', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(2);
    pane.add([9]);
    await pane.rangeTo(4);
    expect(members(pane)).toEqual([2, 3, 4, 9]);
  });

  it('clear resets the anchor', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(2);
    pane.clear();
    await pane.rangeTo(4);
    expect(members(pane)).toEqual([4]);
  });

  it('set resets the anchor', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(2);
    pane.set([8]);
    await pane.rangeTo(4);
    expect(members(pane)).toEqual([4, 8]);
  });

  it('selectAll resets the anchor', async () => {
    // The second list lacks 3, so a surviving anchor at 3 would extend the range to it.
    const { pane } = answering([1, 2, 3, 4, 5], [4, 5, 6, 3, 7]);
    pane.toggle(3);
    await pane.selectAll();
    await pane.rangeTo(4);
    expect(members(pane)).toEqual([1, 2, 3, 4, 5]);
  });

  it('a range moves the anchor to its id', async () => {
    // The second list no longer has 2: from a surviving anchor at 2 the range would be only 7.
    const { pane } = answering(range(1, 10), range(5, 10));
    pane.toggle(2);
    await pane.rangeTo(5);
    await pane.rangeTo(7);
    expect(members(pane)).toEqual([2, 3, 4, 5, 6, 7]);
  });

  it('reconcile leaves the anchor where it was', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(2);
    await pane.reconcile();
    await pane.rangeTo(4);
    expect(members(pane)).toEqual([2, 3, 4]);
  });
});

describe('PaneSelection: rangeTo', () => {
  it('adds the stretch from the anchor forward', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(3);
    await pane.rangeTo(6);
    expect(members(pane)).toEqual([3, 4, 5, 6]);
  });

  it('adds the stretch backward', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(6);
    await pane.rangeTo(3);
    expect(members(pane)).toEqual([3, 4, 5, 6]);
  });

  it('follows the order of the fresh list, not the numeric order of the ids', async () => {
    const { pane } = answering([50, 10, 30, 20, 40]);
    pane.toggle(10);
    await pane.rangeTo(20);
    expect(members(pane)).toEqual([10, 20, 30]);
  });

  it('adds to the existing selection', async () => {
    const { pane } = answering(range(1, 30));
    pane.add([20, 21]);
    pane.toggle(3);
    await pane.rangeTo(5);
    expect(members(pane)).toEqual([3, 4, 5, 20, 21]);
  });

  it('selects just the item when there is no anchor, and anchors there', async () => {
    const { pane } = answering(range(1, 10));
    await pane.rangeTo(4);
    expect(members(pane)).toEqual([4]);
    await pane.rangeTo(6);
    expect(members(pane)).toEqual([4, 5, 6]);
  });

  it('selects just the item when the anchor is not in the fresh list', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(99);
    await pane.rangeTo(4);
    expect(members(pane)).toEqual([4, 99]);
    await pane.rangeTo(6);
    expect(members(pane)).toEqual([4, 5, 6, 99]);
  });

  it('selects just the item when it is not in the fresh list, and anchors there', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(2);
    await pane.rangeTo(99);
    expect(members(pane)).toEqual([2, 99]);
  });

  it('on the anchor itself selects that one item', async () => {
    const { pane } = answering(range(1, 10));
    pane.toggle(5);
    pane.toggle(5);
    await pane.rangeTo(5);
    expect(members(pane)).toEqual([5]);
  });

  it('writes nothing when the whole stretch is already selected', async () => {
    const { pane } = answering(range(1, 10));
    pane.add([3, 4, 5]);
    pane.toggle(3);
    pane.toggle(3);
    const before = pane.ids.value;
    await pane.rangeTo(5);
    expect(pane.ids.value).toBe(before);
  });

  it('writes once for a 50k-item stretch', async () => {
    const { pane } = answering(range(1, 50_000));
    pane.toggle(1);
    const runs = countRuns(() => pane.ids.value);
    await pane.rangeTo(50_000);
    expect(pane.count.value).toBe(50_000);
    expect(runs()).toBe(2);
  });

  it('asks for a fresh list every time', async () => {
    const { pane, signals } = answering(range(1, 10));
    pane.toggle(1);
    await pane.rangeTo(2);
    await pane.rangeTo(3);
    expect(signals).toHaveLength(2);
  });
});

describe('PaneSelection: selectAll', () => {
  it('replaces the selection with the fresh list', async () => {
    const { pane } = answering([1, 2, 3]);
    pane.add([3, 99]);
    await pane.selectAll();
    expect(members(pane)).toEqual([1, 2, 3]);
    expect(pane.active.value).toBe(true);
  });

  it('writes nothing when the selection already holds exactly the list', async () => {
    const { pane } = answering([1, 2, 3]);
    pane.set([3, 2, 1]);
    const before = pane.ids.value;
    await pane.selectAll();
    expect(pane.ids.value).toBe(before);
  });

  it('selects an empty folder as an empty selection', async () => {
    const { pane } = answering([]);
    pane.add([1]);
    await pane.selectAll();
    expect(pane.count.value).toBe(0);
  });
});

describe('PaneSelection: the ids provider', () => {
  it('does nothing without a provider', async () => {
    const pane = new PaneSelection();
    pane.toggle(2);
    await pane.rangeTo(5);
    await pane.selectAll();
    await pane.reconcile();
    expect(members(pane)).toEqual([2]);
    expect(pane.busy.value).toBe(false);
    expect(toastMessages()).toEqual([]);
  });

  it('does nothing after the provider is removed', async () => {
    const { pane, signals } = answering([1, 2, 3]);
    pane.setIdsProvider(null);
    await pane.selectAll();
    expect(signals).toHaveLength(0);
    expect(pane.count.value).toBe(0);
  });

  it('is called as soon as an operation is requested, with a live signal', () => {
    const { pane, calls } = waiting();
    void pane.selectAll();
    expect(calls).toHaveLength(1);
    expect(calls[0]?.signal.aborted).toBe(false);
  });

  it('is called once for concurrent calls, which all apply in the one answer', async () => {
    const { pane, calls } = waiting();
    pane.toggle(1);
    const first = pane.rangeTo(3);
    const second = pane.rangeTo(4);
    const third = pane.reconcile();
    expect(calls).toHaveLength(1);
    expect(first).toBe(second);
    expect(second).toBe(third);
    calls[0]?.answer.resolve(range(1, 10));
    await Promise.all([first, second, third]);
    expect(members(pane)).toEqual([1, 2, 3, 4]);
    expect(calls).toHaveLength(1);
  });

  it('applies concurrent operations in call order: select all, then a range', async () => {
    const { pane, calls } = waiting();
    const all = pane.selectAll();
    const lone = pane.rangeTo(99); // 99 is not in the list: it is added on top
    calls[0]?.answer.resolve([1, 2, 3]);
    await Promise.all([all, lone]);
    expect(members(pane)).toEqual([1, 2, 3, 99]);
  });

  it('applies concurrent operations in call order: a range, then select all', async () => {
    const { pane, calls } = waiting();
    const lone = pane.rangeTo(99);
    const all = pane.selectAll(); // replaces everything, 99 included
    calls[0]?.answer.resolve([1, 2, 3]);
    await Promise.all([lone, all]);
    expect(members(pane)).toEqual([1, 2, 3]);
  });

  it('applies concurrent operations in one write', async () => {
    const { pane, calls } = waiting();
    pane.add([9]);
    const runs = countRuns(() => [pane.ids.value, pane.busy.value]);
    const before = runs();
    const done = Promise.all([pane.selectAll(), pane.rangeTo(2), pane.reconcile()]);
    expect(runs() - before).toBe(1); // busy turned true
    calls[0]?.answer.resolve([1, 2, 3]);
    await done;
    expect(runs() - before).toBe(2); // ids and busy changed together
    expect(members(pane)).toEqual([1, 2, 3]);
  });

  it('is called again for the next operation: nothing is cached', async () => {
    const { pane, calls } = waiting();
    const first = pane.selectAll();
    calls[0]?.answer.resolve([1, 2]);
    await first;
    const second = pane.selectAll();
    expect(calls).toHaveLength(2);
    calls[1]?.answer.resolve([1, 2, 3]);
    await second;
    expect(members(pane)).toEqual([1, 2, 3]);
  });

  it('a provider replaced while a request is in flight does not touch that request', async () => {
    const { pane, calls } = waiting();
    const first = pane.selectAll();
    pane.setIdsProvider(async () => [7, 8]);
    calls[0]?.answer.resolve([1, 2]);
    await first;
    expect(members(pane)).toEqual([1, 2]);
    await pane.selectAll();
    expect(members(pane)).toEqual([7, 8]);
  });
});

describe('PaneSelection: discarding pending work', () => {
  it('clear() drops the answer, aborts the request and releases the waiters', async () => {
    const { pane, calls } = waiting();
    const done = pane.selectAll();
    expect(pane.busy.value).toBe(true);
    pane.clear();
    expect(calls[0]?.signal.aborted).toBe(true);
    expect(pane.busy.value).toBe(false);
    await done;
    calls[0]?.answer.resolve([1, 2, 3]);
    await flush();
    expect(pane.count.value).toBe(0);
    expect(toastMessages()).toEqual([]);
  });

  it('clear() of an empty selection still discards a pending select-all', async () => {
    const { pane, calls } = waiting();
    const done = pane.selectAll();
    pane.clear();
    await done;
    calls[0]?.answer.resolve([1, 2, 3]);
    await flush();
    expect(pane.count.value).toBe(0);
  });

  it('a context change drops the answer and aborts the request', async () => {
    const { pane, calls } = waiting();
    pane.setContext('folder-a');
    pane.toggle(1);
    const done = pane.rangeTo(3);
    pane.setContext('folder-b');
    expect(calls[0]?.signal.aborted).toBe(true);
    expect(pane.busy.value).toBe(false);
    await done;
    calls[0]?.answer.resolve(range(1, 10));
    await flush();
    expect(pane.count.value).toBe(0);
  });

  it('the same context key keeps the request and the selection', async () => {
    const { pane, calls } = waiting();
    pane.setContext('folder-a');
    pane.toggle(1);
    const done = pane.rangeTo(3);
    pane.setContext('folder-a');
    expect(calls[0]?.signal.aborted).toBe(false);
    calls[0]?.answer.resolve(range(1, 10));
    await done;
    expect(members(pane)).toEqual([1, 2, 3]);
  });

  it('set() drops the answer too: in call order it would have been overwritten', async () => {
    const { pane, calls } = waiting();
    const done = pane.selectAll();
    pane.set([5]);
    expect(calls[0]?.signal.aborted).toBe(true);
    await done;
    calls[0]?.answer.resolve([1, 2, 3]);
    await flush();
    expect(members(pane)).toEqual([5]);
  });

  it('a late answer of a discarded request does not touch the next request', async () => {
    const { pane, calls } = waiting();
    const first = pane.rangeTo(3);
    pane.clear();
    const second = pane.rangeTo(5);
    expect(calls).toHaveLength(2);
    expect(pane.busy.value).toBe(true);
    calls[0]?.answer.resolve(range(1, 10));
    await first;
    await flush();
    expect(pane.count.value).toBe(0);
    expect(pane.busy.value).toBe(true);
    calls[1]?.answer.resolve(range(1, 10));
    await second;
    expect(members(pane)).toEqual([5]);
    expect(pane.busy.value).toBe(false);
  });

  it('a late failure of a discarded request is silent', async () => {
    const { pane, calls } = waiting();
    const done = pane.selectAll();
    pane.clear();
    await done;
    calls[0]?.answer.reject(new Error('boom'));
    await flush();
    expect(toastMessages()).toEqual([]);
  });
});

describe('PaneSelection: failures', () => {
  it('rangeTo shows the toast, keeps the selection and the anchor, and does not reject', async () => {
    const { pane, calls } = waiting();
    pane.toggle(2);
    const failed = pane.rangeTo(5);
    calls[0]?.answer.reject(new Error('boom'));
    await expect(failed).resolves.toBeUndefined();
    expect(toastMessages()).toEqual([LOAD_FAILED]);
    expect(members(pane)).toEqual([2]);
    expect(pane.busy.value).toBe(false);

    const retry = pane.rangeTo(5);
    calls[1]?.answer.resolve(range(1, 10));
    await retry;
    expect(members(pane)).toEqual([2, 3, 4, 5]);
  });

  it('selectAll shows the toast and keeps the selection', async () => {
    const { pane, calls } = waiting();
    pane.toggle(7);
    const failed = pane.selectAll();
    calls[0]?.answer.reject(new Error('boom'));
    await failed;
    expect(toastMessages()).toEqual([LOAD_FAILED]);
    expect(members(pane)).toEqual([7]);
  });

  it('an answer that is not a list counts as a failure', async () => {
    const pane = new PaneSelection();
    pane.setIdsProvider(async () => ({ ids: [1] }) as unknown as number[]);
    pane.toggle(4);
    await pane.selectAll();
    expect(toastMessages()).toEqual([LOAD_FAILED]);
    expect(members(pane)).toEqual([4]);
  });

  it('a provider that throws at once counts as a failure', async () => {
    const pane = new PaneSelection();
    pane.setIdsProvider(() => {
      throw new Error('sync boom');
    });
    await pane.selectAll();
    expect(toastMessages()).toEqual([LOAD_FAILED]);
    expect(pane.busy.value).toBe(false);
  });

  it('reconcile fails silently and keeps the selection', async () => {
    const { pane, calls } = waiting();
    pane.add([1, 2]);
    const failed = pane.reconcile();
    calls[0]?.answer.reject(new Error('boom'));
    await failed;
    expect(toastMessages()).toEqual([]);
    expect(members(pane)).toEqual([1, 2]);
    expect(pane.busy.value).toBe(false);
  });

  it('a batch that holds a range shows one toast even with a reconcile in it', async () => {
    const { pane, calls } = waiting();
    pane.add([1, 2]);
    const done = Promise.all([pane.reconcile(), pane.rangeTo(5)]);
    calls[0]?.answer.reject(new Error('boom'));
    await done;
    expect(toastMessages()).toEqual([LOAD_FAILED]);
  });

  it('a cancelled request is not reported', async () => {
    const { pane, calls } = waiting();
    const done = pane.selectAll();
    calls[0]?.answer.reject(new DOMException('Aborted', 'AbortError'));
    await done;
    expect(toastMessages()).toEqual([]);
    expect(pane.busy.value).toBe(false);
  });
});

describe('PaneSelection: busy', () => {
  it('is true while a request is pending and false once it settles', async () => {
    const { pane, calls } = waiting();
    const seen: boolean[] = [];
    stops.push(
      effect(() => {
        seen.push(pane.busy.value);
      }),
    );
    const done = pane.selectAll();
    expect(pane.busy.value).toBe(true);
    calls[0]?.answer.resolve([1]);
    await done;
    expect(pane.busy.value).toBe(false);
    expect(seen).toEqual([false, true, false]);
  });

  it('stays true until the last of the joined calls has been answered', async () => {
    const { pane, calls } = waiting();
    const first = pane.rangeTo(2);
    const second = pane.selectAll();
    expect(pane.busy.value).toBe(true);
    calls[0]?.answer.resolve([1, 2, 3]);
    await Promise.all([first, second]);
    expect(pane.busy.value).toBe(false);
    expect(calls).toHaveLength(1);
  });

  it('is false after a failure', async () => {
    const { pane, calls } = waiting();
    const done = pane.selectAll();
    calls[0]?.answer.reject(new Error('boom'));
    await done;
    expect(pane.busy.value).toBe(false);
  });

  it('turns on again for the next request', async () => {
    const { pane, calls } = waiting();
    const first = pane.selectAll();
    calls[0]?.answer.resolve([1]);
    await first;
    const second = pane.selectAll();
    expect(pane.busy.value).toBe(true);
    calls[1]?.answer.resolve([1]);
    await second;
    expect(pane.busy.value).toBe(false);
  });
});

describe('PaneSelection: reconcile', () => {
  it('does not call the provider while nothing is selected', async () => {
    const { pane, signals } = answering([1, 2, 3]);
    await pane.reconcile();
    expect(signals).toHaveLength(0);
    expect(pane.busy.value).toBe(false);
  });

  it('drops the ids that no longer exist, in one write', async () => {
    const { pane } = answering([1, 3, 5]);
    pane.add([1, 2, 3, 4]);
    const runs = countRuns(() => pane.ids.value);
    await pane.reconcile();
    expect(members(pane)).toEqual([1, 3]);
    expect(runs()).toBe(2);
  });

  it('writes nothing when no selected id is stale', async () => {
    const { pane } = answering(range(1, 10));
    pane.add([1, 2, 3]);
    const before = pane.ids.value;
    const runs = countRuns(() => pane.ids.value);
    await pane.reconcile();
    expect(pane.ids.value).toBe(before);
    expect(runs()).toBe(1);
  });

  it('empties the selection when every selected id is gone', async () => {
    const { pane } = answering([100, 101]);
    pane.add([1, 2]);
    await pane.reconcile();
    expect(pane.count.value).toBe(0);
    expect(pane.active.value).toBe(false);
  });

  it('shares the request of a pending select-all', async () => {
    const { pane, calls } = waiting();
    pane.add([1, 9]);
    const all = pane.selectAll();
    const reconcile = pane.reconcile();
    expect(calls).toHaveLength(1);
    calls[0]?.answer.resolve([1, 2, 3]);
    await Promise.all([all, reconcile]);
    expect(members(pane)).toEqual([1, 2, 3]);
  });

  it('is skipped when the selection is empty even if a request is pending', async () => {
    const { pane, calls } = waiting();
    const all = pane.selectAll();
    await pane.reconcile();
    expect(calls).toHaveLength(1);
    calls[0]?.answer.resolve([1]);
    await all;
  });
});

describe('PaneSelection: context', () => {
  it('the first call only stores the key', () => {
    const pane = new PaneSelection();
    pane.add([1, 2]);
    pane.setContext('folder-a');
    expect(members(pane)).toEqual([1, 2]);
  });

  it('a different key clears the selection', () => {
    const pane = new PaneSelection();
    pane.setContext('folder-a');
    pane.add([1, 2]);
    pane.setContext('folder-b');
    expect(pane.count.value).toBe(0);
    expect(pane.active.value).toBe(false);
  });

  it('the same key keeps the selection', () => {
    const pane = new PaneSelection();
    pane.setContext('folder-a');
    pane.add([1, 2]);
    pane.setContext('folder-a');
    expect(members(pane)).toEqual([1, 2]);
  });

  it('a change of key resets the anchor', async () => {
    const { pane } = answering(range(1, 10));
    pane.setContext('folder-a');
    pane.toggle(2);
    pane.setContext('folder-b');
    await pane.rangeTo(4);
    expect(members(pane)).toEqual([4]);
  });
});

describe('selectionContextKey', () => {
  const request: BrowseRequest = {
    volume_id: 1,
    dir: '2024/Trip',
    filter: { cameras: ['Canon R5'], ratings: [4, 5], flags: [], tags: [], selection_id: null },
    plan: null,
  };

  it('is the same for the same volume, directory and filter', () => {
    expect(selectionContextKey(request)).toBe(selectionContextKey({ ...request }));
  });

  it('does not depend on the order of the properties', () => {
    const reordered = {
      dir: request.dir,
      filter: {
        selection_id: null,
        tags: [],
        flags: [],
        ratings: [4, 5],
        cameras: ['Canon R5'],
      },
      volume_id: request.volume_id,
    };
    expect(selectionContextKey(reordered)).toBe(selectionContextKey(request));
  });

  it('ignores the plan', () => {
    const planned: BrowseRequest = { ...request, plan: { plan_id: 3, side: 'source' } };
    expect(selectionContextKey(planned)).toBe(selectionContextKey(request));
  });

  it('changes with the volume, the directory and each part of the filter', () => {
    const keys = new Set([
      selectionContextKey(request),
      selectionContextKey({ ...request, volume_id: 2 }),
      selectionContextKey({ ...request, volume_id: null }),
      selectionContextKey({ ...request, dir: '2024' }),
      selectionContextKey({ ...request, filter: { ...request.filter, cameras: [] } }),
      selectionContextKey({ ...request, filter: { ...request.filter, ratings: [5] } }),
      selectionContextKey({ ...request, filter: { ...request.filter, flags: ['selected'] } }),
      selectionContextKey({ ...request, filter: { ...request.filter, tags: ['trip'] } }),
      selectionContextKey({ ...request, filter: { ...request.filter, selection_id: 9 } }),
    ]);
    expect(keys.size).toBe(9);
  });
});

describe('the registry', () => {
  it('has one independent selection per pane', async () => {
    expect(Object.keys(selection).sort()).toEqual(['comparison', 'source']);
    expect(selection.source).not.toBe(selection.comparison);
    selection.source.add([1, 2]);
    expect(members(selection.source)).toEqual([1, 2]);
    expect(selection.comparison.count.value).toBe(0);

    selection.comparison.setContext('a');
    selection.source.setContext('b');
    selection.comparison.setContext('c');
    expect(members(selection.source)).toEqual([1, 2]);

    const { provider, calls } = controlled();
    selection.comparison.setIdsProvider(provider);
    await selection.source.selectAll();
    expect(calls).toHaveLength(0);
  });

  it('starts with the source pane active', () => {
    expect(activePane.value).toBe('source');
  });

  it('clearAllSelections empties both panes and discards what is pending', async () => {
    const { provider, calls } = controlled();
    selection.source.add([1]);
    selection.comparison.add([2]);
    selection.comparison.setIdsProvider(provider);
    const done = selection.comparison.selectAll();
    clearAllSelections();
    await done;
    expect(selection.source.count.value).toBe(0);
    expect(selection.comparison.count.value).toBe(0);
    expect(calls[0]?.signal.aborted).toBe(true);
  });

  it('resetSelectionForTests restores the initial state', async () => {
    selection.source.setContext('a');
    selection.source.add([1]);
    selection.source.setIdsProvider(async () => [1, 2]);
    activePane.value = 'comparison';
    resetSelectionForTests();
    expect(selection.source.count.value).toBe(0);
    expect(activePane.value).toBe('source');
    await selection.source.selectAll();
    expect(selection.source.count.value).toBe(0); // the provider is gone
    selection.source.add([5]);
    selection.source.setContext('b'); // the first call after a reset only stores the key
    expect(members(selection.source)).toEqual([5]);
  });
});
