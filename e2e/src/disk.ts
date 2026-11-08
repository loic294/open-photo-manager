import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import {
  lstat,
  mkdir,
  open,
  readdir,
  readlink,
  rm,
  utimes,
  writeFile as write,
} from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { expect } from '@playwright/test';
import type { AppInstance } from './app-instance';

export interface TreeEntry {
  path: string;
  type: 'file' | 'directory' | 'symlink';
  size: number;
  mtime: number;
  hash?: string;
  link?: string;
  mode?: number;
  unreadable?: boolean;
}
export async function snapshotTree(
  dir: string,
  options: { hash?: boolean } = {},
): Promise<TreeEntry[]> {
  const entries: TreeEntry[] = [];
  async function visit(relative: string) {
    const path = join(dir, relative);
    const info = await lstat(path);
    const type = info.isSymbolicLink() ? 'symlink' : info.isDirectory() ? 'directory' : 'file';
    const entry: TreeEntry = {
      path: relative,
      type,
      size: type === 'directory' ? 0 : info.size,
      mtime: type === 'directory' ? 0 : info.mtimeMs,
    };
    entry.mode = info.mode & 0o777;
    const unreadable = (entry.mode & 0o400) === 0 && type !== 'symlink';
    if (unreadable) entry.unreadable = true;
    if (type === 'symlink') entry.link = await readlink(path);
    if (options.hash && type === 'file' && !unreadable) {
      const hash = createHash('sha1');
      for await (const chunk of createReadStream(path)) hash.update(chunk);
      entry.hash = hash.digest('hex');
    }
    if (relative) entries.push(entry);
    if (type === 'directory' && !unreadable)
      for (const child of (await readdir(path)).sort())
        await visit(relative ? `${relative}/${child}` : child);
  }
  await visit('');
  return entries.sort((a, b) => a.path.localeCompare(b.path));
}
export function diffTrees(before: TreeEntry[], after: TreeEntry[]) {
  const old = new Map(before.map((entry) => [entry.path, entry]));
  const next = new Map(after.map((entry) => [entry.path, entry]));
  const added = after.filter((entry) => !old.has(entry.path));
  const removed = before.filter((entry) => !next.has(entry.path));
  const changed = after.filter(
    (entry) => old.has(entry.path) && JSON.stringify(old.get(entry.path)) !== JSON.stringify(entry),
  );
  const moved: { from: string; to: string }[] = [];
  const available = [...added];
  for (const entry of removed) {
    const match =
      entry.hash &&
      available.find((other) => other.hash === entry.hash && other.type === entry.type);
    if (match) {
      moved.push({ from: entry.path, to: match.path });
      available.splice(available.indexOf(match), 1);
    }
  }
  return { added, removed, changed, moved };
}
export async function expectTree(dir: string, expected: TreeEntry[]) {
  const actual = await snapshotTree(dir, { hash: expected.some((entry) => entry.hash) });
  expect(actual, JSON.stringify(diffTrees(expected, actual), null, 2)).toEqual(expected);
}
export async function trashEntries(app: AppInstance) {
  return (await snapshotTree(app.volumesDir)).filter(
    (entry) => entry.path.includes('/.apm-trash/') && entry.type !== 'directory',
  );
}
export async function writeFile(
  app: AppInstance,
  volume: string,
  relPath: string,
  content: string | Uint8Array,
) {
  const path = app.volumePath(`${volume}/${relPath}`);
  await mkdir(dirname(path), { recursive: true });
  await write(path, content);
}
export async function touch(app: AppInstance, volume: string, relPath: string, time = new Date()) {
  await utimes(app.volumePath(`${volume}/${relPath}`), time, time);
}
export async function removeFile(app: AppInstance, volume: string, relPath: string) {
  await rm(app.volumePath(`${volume}/${relPath}`));
}
export async function makeDir(app: AppInstance, volume: string, relPath: string) {
  await mkdir(app.volumePath(`${volume}/${relPath}`), { recursive: true });
}
export async function makeSparseFile(
  app: AppInstance,
  volume: string,
  relPath: string,
  bytes: number,
) {
  if (!Number.isSafeInteger(bytes) || bytes < 0 || bytes > 4 * 1024 ** 3)
    throw new Error('Sparse file must be between 0 and 4 GiB');
  await makeDir(app, volume, dirname(relPath));
  const file = await open(app.volumePath(`${volume}/${relPath}`), 'w');
  try {
    await file.truncate(bytes);
  } finally {
    await file.close();
  }
}
