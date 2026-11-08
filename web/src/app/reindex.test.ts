import type { Signal } from '@preact/signals-core';
import { afterEach, beforeEach, describe, expect, it, type MockInstance, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { VolumeDto } from '@/api/generated/VolumeDto';
import { type ConnectionState, events } from '@/api/sse';
import { requestReindex } from '@/app/app-actions';
import {
  currentFolderTargets,
  reindexAllAssets,
  reindexCurrentFolders,
  reindexDisabled,
  reindexPending,
  startReindex,
} from '@/app/reindex';
import { locations, PANES, ROOT_LOCATION } from '@/stores/nav';
import { clearToasts, toasts } from '@/stores/toasts';
import { loadVolumes } from '@/stores/volumes';
import { pressConfirm, shownConfirm } from '@/test/confirm';
import { makeTask } from '@/test/task-fixtures';

const photos: VolumeDto = { id: 1, name: 'photos', writable: true, online: true };
const backup: VolumeDto = { id: 2, name: 'backup', writable: false, online: true };
const nas: VolumeDto = { id: 3, name: 'nas', writable: true, online: false };

const queued = makeTask({ id: 7, kind: 'index', state: 'queued', title: 'Re-indexing 1 folder' });

let post: MockInstance<typeof api.post>;

const setConnection = (state: ConnectionState) => {
  (events.connection as Signal<ConnectionState>).value = state;
};

const show = (
  source: [string | null, string],
  comparison: [string | null, string] = [null, ''],
) => {
  locations.source.value = { volume: source[0], dir: source[1] };
  locations.comparison.value = { volume: comparison[0], dir: comparison[1] };
};

const messages = () => toasts.value.map((t) => [t.kind, t.message]);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

beforeEach(async () => {
  vi.spyOn(api, 'get').mockResolvedValue([photos, backup, nas]);
  await loadVolumes();
  post = vi.spyOn(api, 'post').mockResolvedValue(queued);
  setConnection('open');
  clearToasts();
});

afterEach(() => {
  for (const pane of PANES) locations[pane].value = ROOT_LOCATION;
  setConnection('closed');
  clearToasts();
  vi.restoreAllMocks();
});

const rescanBody = (body: object) => ['/api/index/rescan', { body, signal: undefined }] as const;

describe('currentFolderTargets', () => {
  it('lists the folder of every pane with its subfolders, volume names turned into ids', () => {
    show(['photos', 'trips/2024'], ['backup', '']);

    expect(currentFolderTargets()).toEqual([
      { volume_id: 1, dir: 'trips/2024', recursive: true },
      { volume_id: 2, dir: '', recursive: true },
    ]);
  });

  it('lists a folder shown in both panes once', () => {
    show(['photos', 'trips'], ['photos', 'trips']);

    expect(currentFolderTargets()).toEqual([{ volume_id: 1, dir: 'trips', recursive: true }]);
  });

  it('keeps two folders of the same volume, and the same folder name on two volumes', () => {
    show(['photos', 'trips'], ['photos', 'trips/2024']);
    expect(currentFolderTargets().map((t) => t.dir)).toEqual(['trips', 'trips/2024']);

    show(['photos', 'trips'], ['backup', 'trips']);
    expect(currentFolderTargets().map((t) => t.volume_id)).toEqual([1, 2]);
  });

  it('skips a pane at the volumes root', () => {
    show([null, ''], ['backup', 'old']);

    expect(currentFolderTargets()).toEqual([{ volume_id: 2, dir: 'old', recursive: true }]);
  });

  it('skips a pane on a volume this client does not know', () => {
    show(['gone', 'x'], ['photos', 'y']);

    expect(currentFolderTargets()).toEqual([{ volume_id: 1, dir: 'y', recursive: true }]);
  });

  it('passes the exact on-disk name through, never normalised or trimmed', () => {
    show(['photos', 'Été 2024/Café '], [null, '']);

    expect(currentFolderTargets()).toEqual([
      { volume_id: 1, dir: 'Été 2024/Café ', recursive: true },
    ]);
  });

  it('keeps an offline volume: the server explains why it cannot be scanned', () => {
    show(['nas', 'photos'], [null, '']);

    expect(currentFolderTargets()).toEqual([{ volume_id: 3, dir: 'photos', recursive: true }]);
  });

  it('is empty when both panes are at the root', () => {
    expect(currentFolderTargets()).toEqual([]);
  });
});

describe('reindexCurrentFolders', () => {
  it('forces a rescan of the folders of the panes and says so', async () => {
    show(['photos', 'trips/2024'], ['backup', 'old']);

    await reindexCurrentFolders();

    expect(post).toHaveBeenCalledTimes(1);
    expect(post.mock.calls[0]).toEqual(
      rescanBody({
        targets: [
          { volume_id: 1, dir: 'trips/2024', recursive: true },
          { volume_id: 2, dir: 'old', recursive: true },
        ],
        force: true,
        confirm_removals: false,
      }),
    );
    expect(messages()).toEqual([['success', 'Re-indexing 2 folders']]);
  });

  it('says "1 folder" for one folder, also when both panes show it', async () => {
    show(['photos', 'trips'], ['photos', 'trips']);

    await reindexCurrentFolders();

    expect(messages()).toEqual([['success', 'Re-indexing 1 folder']]);
  });

  it('tells the user to open a folder first, and sends nothing', async () => {
    await reindexCurrentFolders();

    expect(post).not.toHaveBeenCalled();
    expect(messages()).toEqual([['info', 'Open a folder first']]);
    expect(reindexPending.value).toBeNull();
  });

  it('shows the message of the server when the request is refused', async () => {
    show(['nas', 'photos'], [null, '']);
    post.mockRejectedValueOnce(
      new ApiError({ status: 409, code: 'volume_offline', message: 'Volume nas is offline' }),
    );

    await expect(reindexCurrentFolders()).resolves.toBeUndefined();

    expect(messages()).toEqual([['error', 'Volume nas is offline']]);
    expect(reindexPending.value).toBeNull();
  });

  it('falls back to a generic message for a failure without one', async () => {
    show(['photos', 'trips'], [null, '']);
    post.mockRejectedValueOnce(undefined);

    await reindexCurrentFolders();

    expect(messages()).toEqual([['error', 'Could not start the re-index']]);
  });

  it('can be tried again after a failure', async () => {
    show(['photos', 'trips'], [null, '']);
    post.mockRejectedValueOnce(new ApiError({ status: 500, code: 'internal', message: 'boom' }));
    await reindexCurrentFolders();

    await reindexCurrentFolders();

    expect(post).toHaveBeenCalledTimes(2);
    expect(messages()).toEqual([
      ['error', 'boom'],
      ['success', 'Re-indexing 1 folder'],
    ]);
  });
});

describe('reindexAllAssets', () => {
  it('asks first, in plain words, before touching anything', async () => {
    const run = reindexAllAssets();

    const dialog = await shownConfirm();

    expect(dialog.title).toBe('Re-index all assets?');
    expect(dialog.message).toContain('metadata, checksums and thumbnails of every file');
    expect(dialog.message).toContain('a long time on a large library');
    expect(dialog.message).toContain('keep using the app');
    expect(dialog.buttons).toEqual(['Cancel', 'Re-index all assets']);
    expect(post).not.toHaveBeenCalled();
    // A plain question, not a warning: the work only costs time.
    expect(document.querySelector('apm-confirm-dialog .alert')).toBeNull();
    expect(document.querySelector('apm-confirm-dialog .btn-error')).toBeNull();

    await pressConfirm('Cancel');
    await run;
  });

  it('sends nothing when the user cancels', async () => {
    const run = reindexAllAssets();

    await pressConfirm('Cancel');
    await run;

    expect(post).not.toHaveBeenCalled();
    expect(messages()).toEqual([]);
    expect(reindexPending.value).toBeNull();
  });

  it('forces a rescan of every volume once the user agreed', async () => {
    const run = reindexAllAssets();

    await pressConfirm('Re-index all assets');
    await run;

    expect(post).toHaveBeenCalledTimes(1);
    expect(post.mock.calls[0]).toEqual(
      rescanBody({ targets: [], force: true, confirm_removals: false }),
    );
    expect(messages()).toEqual([['success', 'Re-indexing all assets']]);
  });

  it('does not depend on what the panes show', async () => {
    show(['photos', 'trips'], ['backup', 'old']);
    const run = reindexAllAssets();

    await pressConfirm('Re-index all assets');
    await run;

    expect(post.mock.calls[0]).toEqual(
      rescanBody({ targets: [], force: true, confirm_removals: false }),
    );
  });

  it('shows the message of the server when the request is refused', async () => {
    post.mockRejectedValueOnce(
      new ApiError({ status: 409, code: 'volume_offline', message: 'No volume is online' }),
    );
    const run = reindexAllAssets();

    await pressConfirm('Re-index all assets');
    await run;

    expect(messages()).toEqual([['error', 'No volume is online']]);
    expect(reindexPending.value).toBeNull();
  });
});

describe('while a re-index is being started', () => {
  it('is busy from the click until the server answered, and ignores clicks meanwhile', async () => {
    show(['photos', 'trips'], [null, '']);
    const answer = deferred<ReturnType<typeof makeTask>>();
    post.mockReturnValueOnce(answer.promise);

    const first = reindexCurrentFolders();

    expect(reindexPending.value).toBe('current');
    expect(reindexDisabled.value).toBe(true);

    await reindexCurrentFolders();
    await reindexAllAssets();
    expect(post).toHaveBeenCalledTimes(1);
    expect(document.querySelector('apm-confirm-dialog')).toBeNull();

    answer.resolve(queued);
    await first;

    expect(reindexPending.value).toBeNull();
    expect(reindexDisabled.value).toBe(false);
    expect(messages()).toEqual([['success', 'Re-indexing 1 folder']]);
  });

  it('counts the open confirmation as being busy too', async () => {
    const run = reindexAllAssets();
    await shownConfirm();

    expect(reindexPending.value).toBe('all');
    expect(reindexDisabled.value).toBe(true);
    await reindexCurrentFolders();
    expect(post).not.toHaveBeenCalled();
    expect(messages()).toEqual([]);

    await pressConfirm('Cancel');
    await run;

    expect(reindexPending.value).toBeNull();
    expect(reindexDisabled.value).toBe(false);
  });
});

describe('while the event stream is down', () => {
  it.each(['reconnecting', 'closed'] as const)(
    'is disabled and does nothing (%s)',
    async (state) => {
      show(['photos', 'trips'], [null, '']);
      setConnection(state);

      expect(reindexDisabled.value).toBe(true);
      await reindexCurrentFolders();
      await reindexAllAssets();

      expect(post).not.toHaveBeenCalled();
      expect(messages()).toEqual([]);
      expect(document.querySelector('apm-confirm-dialog')).toBeNull();
      expect(reindexPending.value).toBeNull();
    },
  );

  it.each(['connecting', 'open'] as const)('is enabled while %s', (state) => {
    setConnection(state);

    expect(reindexDisabled.value).toBe(false);
  });

  it('follows the connection back', async () => {
    show(['photos', 'trips'], [null, '']);
    setConnection('reconnecting');
    expect(reindexDisabled.value).toBe(true);

    setConnection('open');
    await reindexCurrentFolders();

    expect(reindexDisabled.value).toBe(false);
    expect(post).toHaveBeenCalledTimes(1);
  });
});

describe('startReindex', () => {
  it('makes the shell menu entries work', async () => {
    show(['photos', 'trips'], [null, '']);
    const stop = startReindex();

    try {
      requestReindex('current');
      await vi.waitFor(() => expect(post).toHaveBeenCalledTimes(1));
      await vi.waitFor(() => expect(reindexPending.value).toBeNull());
      expect(messages()).toEqual([['success', 'Re-indexing 1 folder']]);

      requestReindex('all');
      await pressConfirm('Cancel');
      await vi.waitFor(() => expect(reindexPending.value).toBeNull());
      expect(post).toHaveBeenCalledTimes(1);
    } finally {
      stop();
    }
  });

  it('gives the request back to the "not available" fallback once stopped', () => {
    startReindex()();

    requestReindex('all');

    expect(messages()).toEqual([['info', 'Re-indexing is not available yet.']]);
    expect(post).not.toHaveBeenCalled();
  });
});
