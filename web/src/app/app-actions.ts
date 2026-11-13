import { toast } from '@/components/toast';

/**
 * App-level actions the shell menu triggers but other modules implement.
 *
 * Re-indexing: the shell's "Re-index" menu entries call `requestReindex(scope)`. `startReindex`
 * (`app/reindex.ts`, started by the shell) registers with `onReindex(handler)` and does the work;
 * with no handler registered (the shell is not mounted) the user gets a toast that it is not
 * available. The request travels as a cancelable `opm-reindex` event on `document`, so
 * `document.addEventListener('opm-reindex', ...)` works too (call `preventDefault()` to mark it
 * handled).
 */
export const REINDEX_EVENT = 'opm-reindex';

/**
 * `current`: the folders shown in the panes. `all`: every volume.
 * The `-changed` variants only re-process new files and files whose size or modification time changed.
 */
export type ReindexScope = 'current' | 'all' | 'current-changed' | 'all-changed';

export type ReindexEvent = CustomEvent<{ scope: ReindexScope }>;

declare global {
  interface GlobalEventHandlersEventMap {
    [REINDEX_EVENT]: ReindexEvent;
  }
}

/** Asks the app to re-index. Shows a toast when nothing handles it. */
export function requestReindex(scope: ReindexScope): void {
  const event: ReindexEvent = new CustomEvent(REINDEX_EVENT, {
    detail: { scope },
    cancelable: true,
  });
  if (document.dispatchEvent(event)) {
    toast.info('Re-indexing is not available yet.');
  }
}

/** Handles re-index requests (and marks them handled). Returns a function that unregisters. */
export function onReindex(handler: (scope: ReindexScope) => void): () => void {
  const listener = (event: ReindexEvent) => {
    event.preventDefault();
    handler(event.detail.scope);
  };
  document.addEventListener(REINDEX_EVENT, listener);
  return () => document.removeEventListener(REINDEX_EVENT, listener);
}
