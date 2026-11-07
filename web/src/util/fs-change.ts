/** What a listing shows. A `BrowseRequest` fits as is, so callers pass their request unchanged. */
export interface FsChangeScope {
  /** `null` means every volume. */
  volume_id: number | null;
  /** `/`-separated, relative to the volume root; `''` is the root. */
  dir: string;
  /** An active plan: its overlay can change when any file on any volume changes. */
  plan?: unknown;
}

/** The part of the `fs_change` server event this check needs. */
export interface FsChangeEvent {
  volume_id: number;
  dirs: readonly string[];
}

export interface FsChangeOptions {
  /**
   * Also match a changed dir that equals `dir` or lies below it. For views that depend on the
   * subfolders too (folder counts); a listing of direct children leaves it off.
   */
  subtree?: boolean;
}

/**
 * True when an `fs_change` event can change what `scope` shows, so the view should refetch.
 *
 * - An active plan matches every event (the overlay depends on files everywhere).
 * - A scope with a volume ignores other volumes' events; `volume_id: null` listens to all of them.
 * - Otherwise one of the event's dirs must be `scope.dir` exactly (a listing shows direct children
 *   only), or, with `subtree`, that dir or any dir below it (`startsWith(dir + '/')`, so `a/bc`
 *   is not below `a/b`; every dir is below the root `''`).
 *
 * Dirs are compared as plain strings: never normalise them (the index keeps the on-disk bytes).
 */
export function fsChangeAffects(
  scope: FsChangeScope,
  event: FsChangeEvent,
  options: FsChangeOptions = {},
): boolean {
  if (scope.plan !== null && scope.plan !== undefined) return true;
  if (scope.volume_id !== null && scope.volume_id !== event.volume_id) return false;
  const { dir } = scope;
  const below = `${dir}/`;
  return event.dirs.some(
    (changed) =>
      changed === dir || (options.subtree === true && (dir === '' || changed.startsWith(below))),
  );
}
