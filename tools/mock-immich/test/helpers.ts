import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import type { TestContext } from 'node:test';
import { fileURLToPath } from 'node:url';
import { type MockImmich, type MockImmichOptions, startMockImmich } from '../index.ts';

const PACKAGE_DIR = fileURLToPath(new URL('..', import.meta.url));

/** Everything the tests write goes here (git-ignored), never to the system temporary directory. */
export const SCRATCH_ROOT = resolve(
  process.env.MOCK_IMMICH_SCRATCH_DIR ?? join(PACKAGE_DIR, '.scratch'),
);
process.env.MOCK_IMMICH_SCRATCH_DIR = SCRATCH_ROOT;
mkdirSync(SCRATCH_ROOT, { recursive: true });

export const PACKAGE_ROOT = PACKAGE_DIR;

// The bodies are compared with JSON literals copied from the Rust client's own fixtures.
// biome-ignore lint/suspicious/noExplicitAny: parsed JSON of any shape
export type Json = any;

export interface Reply {
  status: number;
  body: Json;
  headers: Headers;
}

export interface CallOptions {
  body?: unknown;
  /** A body sent as is, to test malformed JSON. */
  raw?: string;
  /** Sent as `x-api-key`. */
  key?: string;
  /** Sent as `Authorization: Bearer`. */
  token?: string;
  headers?: Record<string, string>;
}

export async function call(
  mock: { url: string },
  method: string,
  path: string,
  options: CallOptions = {},
): Promise<Reply> {
  const headers: Record<string, string> = { ...options.headers };
  const body = options.body === undefined ? options.raw : JSON.stringify(options.body);
  if (body !== undefined) {
    headers['content-type'] = 'application/json';
  }
  if (options.key !== undefined) {
    headers['x-api-key'] = options.key;
  }
  if (options.token !== undefined) {
    headers.authorization = `Bearer ${options.token}`;
  }
  const response = await fetch(`${mock.url}${path}`, { method, headers, body });
  const text = await response.text();
  let parsed: Json = null;
  if (text !== '') {
    try {
      parsed = JSON.parse(text);
    } catch {
      parsed = text;
    }
  }
  return { status: response.status, body: parsed, headers: response.headers };
}

export const ADMIN = { email: 'admin@example.test', password: 'admin-password' } as const;
export const USER = { email: 'user@example.test', password: 'user-password' } as const;

/** The permissions the application asks for its key (`KEY_PERMISSIONS` of the Rust client). */
export const APP_PERMISSIONS = [
  'asset.read',
  'asset.update',
  'asset.delete',
  'library.read',
  'library.update',
  'album.read',
  'albumAsset.create',
  'tag.read',
  'tag.asset',
  'server.about',
  'user.read',
  'apiKey.delete',
];

export async function login(
  mock: { url: string },
  account: { email: string; password: string } = ADMIN,
): Promise<string> {
  const reply = await call(mock, 'POST', '/api/auth/login', { body: account });
  assert.equal(reply.status, 201, JSON.stringify(reply.body));
  return reply.body.accessToken;
}

/** Signs in and creates an API key like the application does; returns its secret and id. */
export async function connect(
  mock: { url: string },
  options: { account?: typeof ADMIN | typeof USER; permissions?: string[]; name?: string } = {},
): Promise<{ key: string; keyId: string; token: string }> {
  const token = await login(mock, options.account ?? ADMIN);
  const reply = await call(mock, 'POST', '/api/api-keys', {
    token,
    body: { name: options.name ?? 'opm', permissions: options.permissions ?? APP_PERMISSIONS },
  });
  assert.equal(reply.status, 201, JSON.stringify(reply.body));
  return { key: reply.body.secret, keyId: reply.body.id, token };
}

/** A mock with scans that run at once, stopped when the test ends. */
export async function startMock(
  t: TestContext,
  options: MockImmichOptions = {},
): Promise<MockImmich> {
  const mock = await startMockImmich({ scanDelayMs: 0, ...options });
  t.after(() => mock.stop());
  return mock;
}

/** A scratch directory that is removed when the test ends. */
export function scratchDir(t: TestContext, label: string): string {
  const dir = mkdtempSync(join(SCRATCH_ROOT, `${label}-`));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** Writes the files (path to content) below `root`, creating the directories. */
export function writeTree(root: string, files: Record<string, string>): void {
  for (const [relative, content] of Object.entries(files)) {
    const file = join(root, relative);
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, content);
  }
}

export function touch(file: string, whenMs: number): void {
  utimesSync(file, whenMs / 1000, whenMs / 1000);
}

/** Every page of a metadata search, following the tokens; returns the items in order. */
export async function searchAll(
  mock: { url: string },
  key: string,
  makeBody: (token: string | null) => unknown,
  tokenOf: (answer: Json) => string | null,
): Promise<{ items: Json[]; pages: number }> {
  const items: Json[] = [];
  let token: string | null = null;
  let pages = 0;
  do {
    const reply: Reply = await call(mock, 'POST', '/api/search/metadata', {
      key,
      body: makeBody(token),
    });
    assert.equal(reply.status, 200, JSON.stringify(reply.body));
    pages += 1;
    items.push(...reply.body.assets.items);
    token = tokenOf(reply.body.assets);
  } while (token !== null);
  return { items, pages };
}

/** The typed body of the Rust client: oldest first, trash left out, `cursor` after the first page. */
export function typedBody(
  cursor: string | null,
  extra: Record<string, unknown> = {},
  filter: Record<string, unknown> = {},
): Json {
  return {
    size: 250,
    orderBy: { field: 'fileCreatedAt', direction: 'asc' },
    filter: { trashedAt: { eq: null }, ...filter },
    ...(cursor === null ? {} : { cursor }),
    ...extra,
  };
}

/** The flat body of the Rust client for servers older than 3.2: numbered pages. */
export function flatBody(
  page: number | null,
  extra: Record<string, unknown> = {},
): Record<string, unknown> {
  return { size: 250, page: page ?? 1, order: 'asc', ...extra };
}

/** Waits (up to `timeoutMs`) until `check` stops throwing. */
export async function eventually(check: () => void, timeoutMs = 5000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      check();
      return;
    } catch (error) {
      if (Date.now() > deadline) {
        throw error;
      }
      await new Promise((resolveWait) => setTimeout(resolveWait, 20));
    }
  }
}
