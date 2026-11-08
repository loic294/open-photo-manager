import { effect, untracked } from '@preact/signals-core';
import { locations, setLocation } from '@/stores/nav';
import type { ToolContext } from '@/tools/framework/tool-context';

export interface FollowSourceOptions {
  /** Asked on every move, never subscribed to: it gates the follow but does not trigger it. */
  enabled?: (ctx: ToolContext) => boolean;
}

/**
 * Shared wiring for a tool's `setup`: the comparison pane starts on, and then follows, the folder
 * of the source pane (Folder formatting, Empty folder cleanup).
 *
 *   setup: (ctx) => followSource(ctx, { enabled: (c) => c.baseIsDefault('comparison') }),
 *
 * - Called, it moves the comparison location to the source location at once, and afterwards every
 *   CHANGE of the source location moves it again, while `enabled(ctx)` (default: always) is true.
 *   This includes a deep link: a `cmp` in the address is replaced by the source folder.
 * - A change of the comparison location alone never moves the source and never moves the
 *   comparison back: the user browses each side on its own. Because `enabled` is not subscribed
 *   to, a follow that was off (the user pinned a base) resumes at the next move of the source,
 *   not at once.
 * - Nothing moves while the source has no volume (the volumes root).
 * - The follow never adds a history entry: it REPLACES the current one, so Back steps through the
 *   moves the user made and not through the follow's own. For that, the first move is immediate
 *   but a later one lands one microtask AFTER the move of the source (read the comparison after
 *   an `await`, not on the next line): the source signal changes while the move's own
 *   `setLocation` is still running, and a write from the follow at that moment would take the
 *   place of the entry the move is about to push. Moves of one turn are followed once, with the
 *   last source folder.
 *
 * Returns the cleanup, which `setup` may return as it is. A follow that has not landed yet when
 * it is called is dropped.
 */
export function followSource(ctx: ToolContext, options: FollowSourceOptions = {}): () => void {
  let stopped = false;
  let queued = false;
  let started = false;

  const follow = () => {
    const source = locations.source.peek();
    if (source.volume === null || (options.enabled && !options.enabled(ctx))) return;
    setLocation('comparison', source, { replace: true });
  };

  const stop = effect(() => {
    const { volume } = locations.source.value;
    const first = !started;
    started = true;
    if (volume === null) return;
    if (first) {
      untracked(follow);
    } else if (!queued) {
      queued = true;
      queueMicrotask(() => {
        queued = false;
        if (!stopped) follow();
      });
    }
  });

  return () => {
    stopped = true;
    stop();
  };
}
