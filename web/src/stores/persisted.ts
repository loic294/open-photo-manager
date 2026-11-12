import { type Signal, signal } from '@preact/signals-core';

/** Every persisted key is stored under this prefix. Bump the version if a stored shape breaks. */
export const STORAGE_PREFIX = 'opm.v1.';

/** A type guard over parsed JSON: stored data is untrusted (older version, hand edits). */
export type Validator<T> = (value: unknown) => value is T;

export type PersistedSignal<T> = Signal<T> & {
  /** Stops syncing with storage. Module-level stores never need it; tests do. */
  dispose(): void;
};

export function isBoolean(value: unknown): value is boolean {
  return typeof value === 'boolean';
}

export function isString(value: unknown): value is string {
  return typeof value === 'string';
}

export function oneOf<const T extends readonly string[]>(options: T): Validator<T[number]> {
  return (value): value is T[number] => typeof value === 'string' && options.includes(value);
}

export function numberBetween(min: number, max: number): Validator<number> {
  return (value): value is number =>
    typeof value === 'number' && Number.isFinite(value) && value >= min && value <= max;
}

/** An object with exactly the keys `keys`, each value accepted by `guard`. */
export function recordOf<const K extends string, V>(
  keys: readonly K[],
  guard: Validator<V>,
): Validator<Record<K, V>> {
  return (value): value is Record<K, V> => {
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
    const record = value as Record<string, unknown>;
    return Object.keys(record).length === keys.length && keys.every((key) => guard(record[key]));
  };
}

/** An object of arbitrary string keys whose values are accepted by `guard`. */
export function dictionaryOf<V>(guard: Validator<V>): Validator<Record<string, V>> {
  return (value): value is Record<string, V> =>
    typeof value === 'object' &&
    value !== null &&
    !Array.isArray(value) &&
    Object.values(value).every(guard);
}

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null; // Blocked by browser settings (SecurityError).
  }
}

/**
 * A signal backed by `localStorage['opm.v1.' + key]` as JSON.
 *
 * - Starts from the stored value if it passes `validate`, otherwise from `fallback`.
 * - Every assignment is written back; other tabs' changes arrive through the `storage` event.
 * - Never throws: unavailable storage, a full quota or corrupt JSON just leave it in-memory.
 *
 * Treat object values as immutable: assign a new object (`s.value = { ...s.value, a: 1 }`).
 */
export function persistedSignal<T>(
  key: string,
  fallback: T,
  validate: Validator<T>,
): PersistedSignal<T> {
  const storageKey = STORAGE_PREFIX + key;

  function decode(raw: string | null | undefined): T {
    if (raw === null || raw === undefined) return fallback;
    try {
      const parsed: unknown = JSON.parse(raw);
      return validate(parsed) ? parsed : fallback;
    } catch {
      return fallback;
    }
  }

  function load(): T {
    try {
      return decode(storage()?.getItem(storageKey));
    } catch {
      return fallback;
    }
  }

  const state = signal<T>(load());
  let first = true;
  let applyingRemote = false;

  // `subscribe` runs immediately with the current value: nothing to write for that one.
  const stopWriting = state.subscribe((value) => {
    if (first) {
      first = false;
      return;
    }
    if (applyingRemote) return;
    try {
      const serialised = JSON.stringify(value);
      if (serialised !== undefined) storage()?.setItem(storageKey, serialised);
    } catch {
      // Quota exceeded or storage disabled: the in-memory value still works.
    }
  });

  function onStorage(event: StorageEvent) {
    const cleared = event.key === null;
    if (!cleared && event.key !== storageKey) return;
    if (event.storageArea && event.storageArea !== storage()) return;
    const next = cleared ? fallback : decode(event.newValue);
    if (JSON.stringify(next) === JSON.stringify(state.peek())) return;
    applyingRemote = true;
    try {
      state.value = next;
    } finally {
      applyingRemote = false;
    }
  }
  window.addEventListener('storage', onStorage);

  return Object.assign(state, {
    dispose() {
      window.removeEventListener('storage', onStorage);
      stopWriting();
    },
  });
}
