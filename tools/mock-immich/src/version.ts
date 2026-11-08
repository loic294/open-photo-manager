import type { ServerVersion } from './types.ts';

/**
 * The first release that takes the typed search body (`filter`, `orderBy`, `cursor`): the rule of
 * `ImmichVersion::supports_search_filter` in the Rust client.
 */
const TYPED_SEARCH_SINCE = { major: 3, minor: 2, patch: 0 };

/** Parses `v3.4.0-rc.0`, `3.4.0` or a short `2.1` (missing parts are 0). */
export function parseServerVersion(text: string): ServerVersion | null {
  const match = /^v?(\d+)(?:\.(\d+))?(?:\.(\d+))?(-[0-9A-Za-z.-]+)?$/.exec(text.trim());
  if (match === null) {
    return null;
  }
  const major = Number(match[1]);
  const minor = Number(match[2] ?? 0);
  const patch = Number(match[3] ?? 0);
  return { major, minor, patch, text: `v${major}.${minor}.${patch}${match[4] ?? ''}` };
}

export function supportsTypedSearch(version: ServerVersion): boolean {
  const { major, minor, patch } = version;
  const since = TYPED_SEARCH_SINCE;
  if (major !== since.major) {
    return major > since.major;
  }
  if (minor !== since.minor) {
    return minor > since.minor;
  }
  return patch >= since.patch;
}
