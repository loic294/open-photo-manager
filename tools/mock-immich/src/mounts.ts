import { statSync } from 'node:fs';
import { join, resolve } from 'node:path';
import type { Mount } from './types.ts';
import { isUnder, trimSlash } from './util.ts';

/**
 * Checks the `{ '/mnt/photos': '/real/dir' }` object and returns the mounts, longest prefix
 * first. A mount is how the mock reaches the files behind the paths Immich reports.
 */
export function normaliseMounts(input: Record<string, string> | undefined): Mount[] {
  const mounts: Mount[] = [];
  for (const [rawPrefix, rawDir] of Object.entries(input ?? {})) {
    if (!rawPrefix.startsWith('/')) {
      throw new Error(
        `mount prefix ${JSON.stringify(rawPrefix)} must be an absolute path such as /mnt/photos`,
      );
    }
    const prefix = trimSlash(rawPrefix);
    if (prefix.split('/').includes('..')) {
      throw new Error(`mount prefix ${JSON.stringify(rawPrefix)} must not contain ".."`);
    }
    if (mounts.some((mount) => mount.prefix === prefix)) {
      throw new Error(`mount prefix ${prefix} is given twice`);
    }
    if (typeof rawDir !== 'string' || rawDir === '') {
      throw new Error(`mount ${prefix}: the directory must be a non-empty path`);
    }
    const dir = trimSlash(resolve(rawDir));
    let isDirectory = false;
    try {
      isDirectory = statSync(dir).isDirectory();
    } catch {
      throw new Error(`mount ${prefix}: directory ${dir} does not exist`);
    }
    if (!isDirectory) {
      throw new Error(`mount ${prefix}: ${dir} is not a directory`);
    }
    mounts.push({ prefix, dir });
  }
  return mounts.sort((a, b) => b.prefix.length - a.prefix.length);
}

/** The real path behind a path of the Immich server, or undefined when no mount covers it. */
export function toRealPath(mounts: readonly Mount[], immichPath: string): string | undefined {
  for (const mount of mounts) {
    if (!isUnder(immichPath, mount.prefix)) {
      continue;
    }
    const rest = mount.prefix === '/' ? immichPath : immichPath.slice(mount.prefix.length);
    if (rest.split('/').includes('..')) {
      return undefined;
    }
    return rest === '' || rest === '/' ? mount.dir : join(mount.dir, rest);
  }
  return undefined;
}

export const isMounted = (mounts: readonly Mount[], immichPath: string): boolean =>
  toRealPath(mounts, immichPath) !== undefined;
