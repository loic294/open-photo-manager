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
 * are disabled, and both functions do nothing.
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
 * Re-indexes the folders shown in the panes, subfolders included: the server re-processes every
 * file there (`force`: metadata, checksums and thumbnails, even when size and modification time
 * are unchanged), and the progress shows in the task indicator like any other task. With no folder
 * open (both panes at the volumes root) it only tells the user so and sends nothing.
 */
export function reindexCurrentFolders(): Promise<void> {
  return start('current', async () => {
    const targets = currentFolderTargets();
    if (targets.length === 0) {
      toast.info('Open a folder first');
      return;
    }
    await rescan({ targets, force: true, confirm_removals: false });
    toast.success(`Re-indexing ${plural(targets.length, 'folder')}`);
  });
}

/** Re-indexes every file of every volume, after the user agreed it may take long. */
export function reindexAllAssets(): Promise<void> {
  return start('all', async () => {
    const confirmed = await confirmDialog({
      title: 'Re-index all assets?',
      message:
        'This re-reads the metadata, checksums and thumbnails of every file in every volume. ' +
        'It can take a long time on a large library, and you can keep using the app meanwhile.',
      confirmLabel: 'Re-index all assets',
    });
    if (!confirmed) return;
    await rescan({ targets: [], force: true, confirm_removals: false });
    toast.success('Re-indexing all assets');
  });
}

const HANDLERS: Readonly<Record<ReindexScope, () => Promise<void>>> = {
  current: reindexCurrentFolders,
  all: reindexAllAssets,
};

/**
 * Makes the shell's "Re-index" menu entries work, by registering the two functions above as the
 * handlers of `app/app-actions` (so the "not available yet" toast never shows). The shell calls
 * this once; the result unregisters.
 */
export function startReindex(): () => void {
  return onReindex((scope) => void HANDLERS[scope]());
}
