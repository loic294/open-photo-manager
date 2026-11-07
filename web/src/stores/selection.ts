import { batch, computed, type ReadonlySignal, type Signal, signal } from '@preact/signals-core';
import { isAbortError } from '@/api/errors';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import { toast } from '@/components/toast';
import type { PaneId } from '@/stores/nav';

export type { PaneId } from '@/stores/nav';

/**
 * Fetches the ids of every file of the pane's folder, in the pane's CURRENT sort order. Real
 * files only: planned ghosts are not selectable. The pane sets it with `setIdsProvider` and it
 * must read the pane's current request (volume, directory, filter, sort) on every call; pass the
 * signal on to `fetch` so a discarded request is cancelled.
 */
export type IdsProvider = (signal: AbortSignal) => Promise<readonly number[]>;

const LOAD_FAILED = 'Could not load the list of items';

type Operation =
  | { kind: 'range'; id: number; anchor: number | null }
  | { kind: 'all' }
  | { kind: 'reconcile' };

/** One provider call, and the operations that wait for its result. */
interface InFlight {
  readonly controller: AbortController;
  readonly operations: Operation[];
  readonly done: Promise<void>;
  /** Wakes everyone awaiting `done`; safe to call twice. */
  readonly release: () => void;
}

function createDeferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve: () => void = () => undefined;
  const promise = new Promise<void>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

function sameMembers(a: ReadonlySet<number>, b: ReadonlySet<number>): boolean {
  if (a.size !== b.size) return false;
  for (const id of a) {
    if (!b.has(id)) return false;
  }
  return true;
}

/**
 * The inclusive stretch of `list` between the anchor and `id`, in either direction. Without an
 * anchor, or when the anchor or `id` is gone from the fresh list, it is just `id`.
 */
function stretchBetween(
  list: readonly number[],
  anchor: number | null,
  id: number,
): readonly number[] {
  const to = list.indexOf(id);
  const from = anchor === null ? -1 : list.indexOf(anchor);
  if (to < 0 || from < 0) return [id];
  return list.slice(Math.min(from, to), Math.max(from, to) + 1);
}

/**
 * The selection of ONE pane: the selected file ids and the anchor, the item last clicked, which
 * is where the next Shift range starts. Read the signals (`ids`, `count`, `active`, `busy`) in
 * `render()` to subscribe.
 *
 * Changes that need nothing but what is already known are synchronous (`toggle`, `add`, `set`,
 * `clear`). Each writes ONE new Set (a 50k-item range costs one copy) and writes nothing when
 * nothing would change, so an unchanged selection never re-renders.
 *
 * `rangeTo`, `selectAll` and `reconcile` need the whole folder, but the browser only holds a few
 * hundred of up to 100k items, so they ask the injected ids provider: no cache (the answer is
 * fresh at click time) and no pretending (loaded items are never used). They never reject.
 * - Calls made while a request is in flight join it (ONE provider call) and apply together, in
 *   call order, when it answers. A click made meanwhile applies at once and the answer lands on
 *   top of it.
 * - `clear()`, `set()` and a context change discard what is in flight (and abort the request): in
 *   call order they would overwrite it anyway.
 * - A `rangeTo` reads the anchor when it is CALLED; the anchor moves to its id when it APPLIES, so
 *   a failed range can be retried.
 */
export class PaneSelection {
  private readonly idsState = signal<ReadonlySet<number>>(new Set());
  private readonly busyState = signal(false);

  /** The selected ids in the order they were selected. REPLACED on change, never mutated. */
  readonly ids: ReadonlySignal<ReadonlySet<number>> = this.idsState;
  readonly count: ReadonlySignal<number> = computed(() => this.idsState.value.size);
  /** True while something is selected: a plain click then selects instead of opening. */
  readonly active: ReadonlySignal<boolean> = computed(() => this.count.value > 0);
  /** True while a range, select-all or reconcile waits for the list of ids. */
  readonly busy: ReadonlySignal<boolean> = this.busyState;

  private anchor: number | null = null;
  private provider: IdsProvider | null = null;
  private contextKey: string | null = null;
  private inFlight: InFlight | null = null;

  has(id: number): boolean {
    return this.idsState.value.has(id);
  }

  toArray(): number[] {
    return [...this.idsState.value];
  }

  /** Selects or deselects one item; it becomes the anchor either way. */
  toggle(id: number): void {
    const next = new Set(this.idsState.peek());
    if (!next.delete(id)) next.add(id);
    this.anchor = id;
    this.idsState.value = next;
  }

  /** Adds ids to the selection; the anchor stays. */
  add(ids: readonly number[]): void {
    const current = this.idsState.peek();
    if (ids.every((id) => current.has(id))) return;
    const next = new Set(current);
    for (const id of ids) next.add(id);
    this.idsState.value = next;
  }

  /** Replaces the selection; the anchor is reset. */
  set(ids: Iterable<number>): void {
    batch(() => {
      this.discardInFlight();
      this.anchor = null;
      const next = new Set(ids);
      if (!sameMembers(this.idsState.peek(), next)) this.idsState.value = next;
    });
  }

  clear(): void {
    batch(() => {
      this.discardInFlight();
      this.anchor = null;
      if (this.idsState.peek().size > 0) this.idsState.value = new Set();
    });
  }

  /**
   * Adds the stretch between the anchor and `id` (either direction), then makes `id` the anchor
   * (the Gmail model, which matches the checkbox UI). With no anchor, or when the anchor or `id`
   * is not in the fresh list, it selects just `id` and anchors there.
   */
  rangeTo(id: number): Promise<void> {
    return this.enqueue({ kind: 'range', id, anchor: this.anchor });
  }

  /** Replaces the selection with every item of the folder; the anchor is reset. */
  selectAll(): Promise<void> {
    return this.enqueue({ kind: 'all' });
  }

  /**
   * Drops the selected ids that no longer exist (call it after a change on disk). Does not even
   * ask for the list while nothing is selected, writes nothing when nothing is stale, and keeps
   * the selection when the request fails.
   */
  reconcile(): Promise<void> {
    if (this.idsState.peek().size === 0) return Promise.resolve();
    return this.enqueue({ kind: 'reconcile' });
  }

  /** Where the ordered ids come from. The pane sets it when it mounts and clears it with `null`. */
  setIdsProvider(provider: IdsProvider | null): void {
    this.provider = provider;
  }

  /**
   * Names what the selection belongs to (see `selectionContextKey`). The first call only stores
   * the key; a later call with a DIFFERENT key clears the selection and discards what is in flight.
   */
  setContext(key: string): void {
    const previous = this.contextKey;
    this.contextKey = key;
    if (previous !== null && previous !== key) this.clear();
  }

  /** Back to a fresh instance: empty, no provider, no context. For tests. */
  reset(): void {
    this.clear();
    this.provider = null;
    this.contextKey = null;
  }

  private enqueue(operation: Operation): Promise<void> {
    const provider = this.provider;
    if (!provider) return Promise.resolve();
    const current = this.inFlight;
    if (current) {
      current.operations.push(operation);
      return current.done;
    }
    const { promise, resolve } = createDeferred();
    const created: InFlight = {
      controller: new AbortController(),
      operations: [operation],
      done: promise,
      release: resolve,
    };
    this.inFlight = created;
    this.busyState.value = true;
    void this.load(created, provider);
    return promise;
  }

  private async load(current: InFlight, provider: IdsProvider): Promise<void> {
    let list: readonly number[] | undefined;
    let failure: unknown;
    try {
      const result = await provider(current.controller.signal);
      if (Array.isArray(result)) list = result;
      else failure = new TypeError('The list of items is not an array');
    } catch (error) {
      failure = error;
    }
    // Discarded meanwhile: `discardInFlight` already woke the waiters.
    if (this.inFlight !== current) return;
    try {
      batch(() => {
        this.inFlight = null;
        this.busyState.value = false;
        if (list !== undefined) this.apply(current.operations, list);
      });
      const asked = current.operations.some((operation) => operation.kind !== 'reconcile');
      if (list === undefined && asked && !isAbortError(failure)) toast.error(LOAD_FAILED);
    } finally {
      current.release();
    }
  }

  /** Runs the operations in call order against the fresh list: one write at most. */
  private apply(operations: readonly Operation[], list: readonly number[]): void {
    const original = this.idsState.peek();
    let next: Set<number> | null = null;
    let anchor = this.anchor;
    for (const operation of operations) {
      switch (operation.kind) {
        case 'all':
          next = new Set(list);
          anchor = null;
          break;
        case 'range':
          next ??= new Set(original);
          for (const id of stretchBetween(list, operation.anchor, operation.id)) next.add(id);
          anchor = operation.id;
          break;
        case 'reconcile': {
          const existing = new Set(list);
          const stale = [...(next ?? original)].filter((id) => !existing.has(id));
          if (stale.length > 0) {
            next ??= new Set(original);
            for (const id of stale) next.delete(id);
          }
          break;
        }
      }
    }
    this.anchor = anchor;
    if (next !== null && !sameMembers(original, next)) this.idsState.value = next;
  }

  private discardInFlight(): void {
    const current = this.inFlight;
    if (!current) return;
    this.inFlight = null;
    this.busyState.value = false;
    current.controller.abort();
    current.release();
  }
}

/** One independent selection per pane. */
export const selection: Record<PaneId, PaneSelection> = {
  source: new PaneSelection(),
  comparison: new PaneSelection(),
};

/**
 * The pane the user touched last (pointer or focus); the keyboard shortcuts act on it. Set by the
 * `SelectionController`.
 */
export const activePane: Signal<PaneId> = signal<PaneId>('source');

/**
 * The context key for `PaneSelection.setContext`: the volume, the directory and the FILTER. The
 * plan and the sort are left out on purpose: they change neither which files exist nor which are
 * selectable, so changing them keeps the selection.
 */
export function selectionContextKey(
  request: Pick<BrowseRequest, 'volume_id' | 'dir' | 'filter'>,
): string {
  return JSON.stringify([request.volume_id, request.dir, request.filter], withSortedKeys);
}

/** A `JSON.stringify` replacer that makes the output independent of property order. */
function withSortedKeys(_key: string, value: unknown): unknown {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return value;
  const record = value as Record<string, unknown>;
  return Object.fromEntries(
    Object.keys(record)
      .sort()
      .map((name) => [name, record[name]]),
  );
}

export function clearAllSelections(): void {
  batch(() => {
    for (const pane of Object.values(selection)) pane.clear();
  });
}

/** Back to the initial state: every pane empty with no provider or context, `source` active. */
export function resetSelectionForTests(): void {
  batch(() => {
    for (const pane of Object.values(selection)) pane.reset();
    activePane.value = 'source';
  });
}
