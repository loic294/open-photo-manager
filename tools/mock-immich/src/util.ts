import { createHash, randomBytes } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';

export type Obj = Record<string, unknown>;

/** The shape Immich's spec asks of ids (`IdFilter`): a version 4 UUID. */
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export const isUuid = (value: unknown): value is string =>
  typeof value === 'string' && UUID_RE.test(value);

export const isRecord = (value: unknown): value is Obj =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

/**
 * A UUID derived from a name, so ids of scenario entities are the same on every start and after
 * every reset: ids the backend stored (a library id in a mapping) stay valid.
 */
export function stableUuid(kind: string, key: string): string {
  const bytes = createHash('sha1').update(`apm-mock-immich:${kind}:${key}`).digest();
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = bytes.subarray(0, 16).toString('hex');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export interface Clock {
  /** Epoch milliseconds, strictly greater than every earlier answer. */
  now(): number;
}

/**
 * A strictly increasing millisecond clock. Every change of an asset takes a new tick, so a search
 * for `updatedAfter` the highest `updatedAt` seen never misses a change made in the same
 * millisecond (the client advances its high-water mark that way).
 */
export function createClock(): Clock {
  let last = 0;
  return {
    now() {
      last = Math.max(Date.now(), last + 1);
      return last;
    },
  };
}

export const iso = (ms: number): string => new Date(ms).toISOString();

export const isoOrNull = (ms: number | null): string | null => (ms === null ? null : iso(ms));

export const sha1Base64 = (text: string): string =>
  createHash('sha1').update(text).digest('base64');

export const sha256Hex = (text: string): string => createHash('sha256').update(text).digest('hex');

export const randomToken = (bytes: number): string => randomBytes(bytes).toString('base64url');

export async function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  await delay(ms, undefined, { signal });
}

export function basename(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1);
}

export function extensionOf(path: string): string {
  const name = basename(path);
  const dot = name.lastIndexOf('.');
  return dot <= 0 ? '' : name.slice(dot + 1).toLowerCase();
}

/** The path without a trailing slash (`/` stays). */
export function trimSlash(path: string): string {
  return path.length > 1 && path.endsWith('/') ? path.replace(/\/+$/, '') || '/' : path;
}

/** Whether `path` is `root` or lies below it, compared on whole segments. */
export function isUnder(path: string, root: string): boolean {
  const base = trimSlash(root);
  return base === '/' || path === base || path.startsWith(`${base}/`);
}
