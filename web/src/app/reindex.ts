import { computed, type ReadonlySignal, signal } from '@preact/signals-core';
import { errorMessage } from '@/api/errors';
import type { Scope } from '@/api/generated/Scope';
import { rescan } from '@/api/indexing';
import { type ConnectionState, events } from '@/api/sse';
import { onReindex, type ReindexScope } from '@/app/app-actions';
import { confirmDialog } from '@/components/confirm-dialog';
import { toast } from '@/components/toast';
import { locations, PANES } from '@/stores/nav';
import { volumeIdByName } from '@/stores/volumes';

const DOWN: ReadonlySet<ConnectionState> = new Set(['reconnecting', 'closed']);

const pending = signal<ReindexScope | null>(null);

/**
 * The re-index being started, from the click until the server accepted the request (a
 * confirmation the user has not answered yet counts), else null. The menu shows a spinner on it.
 */
export const reindexPending: ReadonlySignal<ReindexScope | null> = pending;

/**
 * True while a re-index cannot start: one is already being started, or the event stream is down
 * (the server is out of reach, and a task started now could not be followed). The menu entries
 * are disabled, and re-index requests do nothing.
 */
export const reindexDisabled: ReadonlySignal<boolean> = computed(
  () => pending.value !== null || DOWN.has(events.connection.value),
);

const plural = (count: number, noun: string) => `${count} ${noun}${count === 1 ? '' : 's'}`;

/**
 * The folders the panes show, each with its subfolders. Every tool shows both panes, so both
 * count; a pane at the volumes root, or on a volume this client does not know, shows no folder,
 * and the same folder in both panes is sent once. `dir` is the exact on-disk name.
 */
export function currentFolderTargets(): Scope[] {
  const targets = new Map<string, Scope>();
  for (const pane of PANES) {
    const { volume, dir } = locations[pane].peek();
    const volumeId = volumeIdByName(volume);
    if (volumeId === undefined) continue;
    const key = `${volumeId}\0${dir}`;
    if (!targets.has(key)) targets.set(key, { volume_id: volumeId, dir, recursive: true });
  }
  return [...targets.values()];
}

/** Runs one re-index while holding `pending`; a failure becomes an error toast. */
async function start(scope: ReindexScope, run: () => Promise<void>): Promise<void> {
  if (reindexDisabled.peek()) return;
  pending.value = scope;
  try {
    await run();
  } catch (error) {
    toast.error(errorMessage(error, 'Could not start the re-index'));
  } finally {
    pending.value = null;
  }
}

/**
 * Re-indexes the folders shown in the panes, subfolders included. Full mode re-processes every
 * file; changed mode discovers additions/removals and only re-processes new or changed files.
 * With no folder open it only tells the user so and sends nothing.
 */
export function reindexCurrentFolders(mode: 'full' | 'changed' = 'full'): Promise<void> {
  const changedOnly = mode === 'changed';
  return start(changedOnly ? 'current-changed' : 'current', async () => {
    const targets = currentFolderTargets();
    if (targets.length === 0) {
      toast.info('Open a folder first');
      return;
    }
    await rescan({ targets, force: !changedOnly, confirm_removals: false });
    toast.success(
      changedOnly
        ? `Scanning ${plural(targets.length, 'folder')} for changes`
        : `Re-indexing ${plural(targets.length, 'folder')}`,
    );
  });
}

/** Scans every volume, fully or for changes, after the user agreed it may take long. */
export function reindexAllAssets(mode: 'full' | 'changed' = 'full'): Promise<void> {
  const changedOnly = mode === 'changed';
  return start(changedOnly ? 'all-changed' : 'all', async () => {
    const confirmed = await confirmDialog({
      title: changedOnly ? 'Scan all assets for changes?' : 'Re-index all assets?',
      message:
        (changedOnly
          ? 'This looks for new and removed files in every volume, and re-processes files whose ' +
            'size or modification time changed. Unchanged files are not re-processed. '
          : 'This re-reads the metadata, checksums and thumbnails of every file in every volume. ') +
        'It can take a long time on a large library, and you can keep using the app meanwhile.',
      confirmLabel: changedOnly ? 'Scan for changes' : 'Re-index all assets',
    });
    if (!confirmed) return;
    await rescan({ targets: [], force: !changedOnly, confirm_removals: false });
    toast.success(changedOnly ? 'Scanning all assets for changes' : 'Re-indexing all assets');
  });
}

const HANDLERS: Readonly<Record<ReindexScope, () => Promise<void>>> = {
  current: reindexCurrentFolders,
  all: reindexAllAssets,
  'current-changed': () => reindexCurrentFolders('changed'),
  'all-changed': () => reindexAllAssets('changed'),
};

/**
 * Makes the shell's scan and re-index menu entries work, by registering the functions above as the
 * handlers of `app/app-actions` (so the "not available yet" toast never shows). The shell calls
 * this once; the result unregisters.
 */
export function startReindex(): () => void {
  return onReindex((scope) => void HANDLERS[scope]());
}
