import { readFileSync } from 'node:fs';
import type { ApiKey } from './types.ts';

const PERMISSIONS_FILE = new URL(
  '../../../crates/opm-server/src/immich/spec-permissions.txt',
  import.meta.url,
);

let cached: readonly string[] | undefined;

/**
 * Every permission an API key can carry (`all` included): the one list of the repository, shared
 * with the Rust client's own tests. The mock reads the file, so the two cannot drift apart.
 */
export function knownPermissions(): readonly string[] {
  if (cached === undefined) {
    let text: string;
    try {
      text = readFileSync(PERMISSIONS_FILE, 'utf8');
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      throw new Error(
        `mock-immich must run from the repository: cannot read ${PERMISSIONS_FILE.pathname} (${reason})`,
      );
    }
    cached = text
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line !== '' && !line.startsWith('#'));
  }
  return cached;
}

export const isKnownPermission = (value: string): boolean => knownPermissions().includes(value);

/** A key with `all` passes every check. */
export const keyAllows = (key: ApiKey, permission: string): boolean =>
  key.permissions.includes('all') || key.permissions.includes(permission);
