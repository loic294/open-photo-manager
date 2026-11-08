import { afterEach, beforeEach, describe, expect, it, type MockInstance, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { IndexStatusDto } from '@/api/generated/IndexStatusDto';
import type { ApmIndexNotices } from '@/components/apm-index-notices';
import { refreshIndexStatus, resetIndexStatusForTests } from '@/stores/index-status';
import { clearToasts, toasts } from '@/stores/toasts';
import { pressConfirm, shownConfirm } from '@/test/confirm';
import { mount } from '@/test/helpers';
import { makeTask } from '@/test/task-fixtures';
import { formatCount } from '@/util/format-count';
import '@/components/apm-index-notices';

const photos = { volume_id: 1, volume_name: 'photos', files: 12_345 };
const archive = { volume_id: 2, volume_name: 'archive', files: 80 };

const statusOf = (...held: IndexStatusDto['held_removals']): IndexStatusDto => ({
  held_removals: held,
  active_task_id: null,
  watcher: null,
});

let post: MockInstance<typeof api.post>;

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

const alerts = (host: ApmIndexNotices) => [...host.querySelectorAll('[role="alert"]')];
const buttons = (scope: ParentNode) => [...scope.querySelectorAll<HTMLButtonElement>('button')];

function button(scope: ParentNode, label: string): HTMLButtonElement {
  const found = buttons(scope).find((candidate) => text(candidate) === label);
  if (!found) throw new Error(`No "${label}" button`);
  return found;
}

const messages = () => toasts.value.map((t) => [t.kind, t.message]);

const rescanBody = (volumeId: number, confirm: boolean) =>
  [
    '/api/index/rescan',
    {
      body: {
        targets: [{ volume_id: volumeId, dir: '', recursive: true }],
        force: false,
        confirm_removals: confirm,
      },
      signal: undefined,
    },
  ] as const;

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

/** Puts the store into `status` and mounts the notices on it. */
async function mountWith(status: IndexStatusDto): Promise<ApmIndexNotices> {
  vi.spyOn(api, 'get').mockResolvedValue(status);
  await refreshIndexStatus();
  return mount('apm-index-notices');
}

beforeEach(() => {
  post = vi.spyOn(api, 'post').mockResolvedValue(makeTask({ kind: 'index', state: 'queued' }));
  clearToasts();
});

afterEach(() => {
  resetIndexStatusForTests();
  clearToasts();
  vi.restoreAllMocks();
});

describe('apm-index-notices', () => {
  it('renders nothing while no removal is held back', async () => {
    const host = await mountWith(statusOf());

    expect(alerts(host)).toEqual([]);
    expect(host.textContent?.trim()).toBe('');
  });

  it('is a block that does not shrink in the shell column', async () => {
    const host = await mountWith(statusOf());

    expect(host.classList.contains('block')).toBe(true);
    expect(host.classList.contains('flex-none')).toBe(true);
  });

  it('warns once per volume, in words that name the volume and the count', async () => {
    const host = await mountWith(statusOf(photos, archive));

    expect(alerts(host).map(text)).toEqual([
      `Volume photos: ${formatCount(12_345)} files could not be found on disk, so their entries ` +
        'were kept in the index. If the share is mounted correctly you can remove them from the ' +
        'index. Rescan Remove from the index',
      'Volume archive: 80 files could not be found on disk, so their entries were kept in the ' +
        'index. If the share is mounted correctly you can remove them from the index. Rescan ' +
        'Remove from the index',
    ]);
  });

  it('uses the DaisyUI warning alert, full width like the connection banner', async () => {
    const host = await mountWith(statusOf(photos));

    const [alert] = alerts(host);
    expect(alert?.classList.contains('alert')).toBe(true);
    expect(alert?.classList.contains('alert-warning')).toBe(true);
    expect(alert?.classList.contains('rounded-none')).toBe(true);
    expect(alert?.classList.contains('flex-none')).toBe(true);
    expect(alert?.querySelector('apm-icon')?.getAttribute('name')).toBe('triangle-alert');
  });

  it('labels the two actions of a notice with the volume they act on', async () => {
    const host = await mountWith(statusOf(photos, archive));

    const groups = [...host.querySelectorAll('[role="group"]')];
    expect(groups.map((group) => group.getAttribute('aria-label'))).toEqual([
      'Volume photos',
      'Volume archive',
    ]);
    expect(groups.map((group) => buttons(group).map(text))).toEqual([
      ['Rescan', 'Remove from the index'],
      ['Rescan', 'Remove from the index'],
    ]);
    for (const each of buttons(host)) {
      expect(each.classList.contains('btn')).toBe(true);
      expect(each.classList.contains('btn-sm')).toBe(true);
      expect(each.type).toBe('button');
    }
  });

  it('speaks of "1 file" correctly', async () => {
    const host = await mountWith(statusOf({ volume_id: 3, volume_name: 'nas', files: 1 }));

    expect(text(alerts(host)[0])).toContain(
      'Volume nas: 1 file could not be found on disk, so its entry was kept in the index. ' +
        'If the share is mounted correctly you can remove it from the index.',
    );
  });

  it('follows the status: a notice appears and goes away with the held removals', async () => {
    const host = await mountWith(statusOf());
    expect(alerts(host)).toHaveLength(0);

    vi.spyOn(api, 'get').mockResolvedValue(statusOf(photos));
    await refreshIndexStatus();
    await host.updateComplete;
    expect(alerts(host)).toHaveLength(1);

    vi.spyOn(api, 'get').mockResolvedValue(statusOf());
    await refreshIndexStatus();
    await host.updateComplete;
    expect(alerts(host)).toHaveLength(0);
  });

  describe('Rescan', () => {
    it('asks for a normal scan of that volume, without forcing or confirming anything', async () => {
      const host = await mountWith(statusOf(photos, archive));
      const [, second] = alerts(host);

      button(second as Element, 'Rescan').click();
      await vi.waitFor(() => expect(post).toHaveBeenCalledTimes(1));

      expect(post.mock.calls[0]).toEqual(rescanBody(2, false));
      await vi.waitFor(() => expect(messages()).toEqual([['success', 'Rescanning archive']]));
    });

    it('does not ask for a confirmation', async () => {
      const host = await mountWith(statusOf(photos));

      button(host, 'Rescan').click();
      await vi.waitFor(() => expect(post).toHaveBeenCalledTimes(1));

      expect(document.querySelector('apm-confirm-dialog')).toBeNull();
    });
  });

  describe('Remove from the index', () => {
    it('asks first, naming the volume and the exact count, and promising that no file is touched', async () => {
      const host = await mountWith(statusOf(photos));

      button(host, 'Remove from the index').click();
      const dialog = await shownConfirm();

      expect(dialog.title).toBe(`Remove ${formatCount(12_345)} files from the index?`);
      expect(dialog.message).toContain(`${formatCount(12_345)} files of volume photos`);
      expect(dialog.message).toContain('no file on disk is touched');
      expect(dialog.message).toContain('mounted wrongly');
      expect(dialog.buttons).toEqual(['Cancel', 'Remove from the index']);
      // It forgets entries: shown as the destructive action it is.
      expect(document.querySelector('apm-confirm-dialog .btn-error')).not.toBeNull();
      expect(post).not.toHaveBeenCalled();

      await pressConfirm('Cancel');
    });

    it('sends nothing when the user cancels', async () => {
      const host = await mountWith(statusOf(photos));

      button(host, 'Remove from the index').click();
      await pressConfirm('Cancel');
      await vi.waitFor(() => expect(buttons(host).every((each) => !each.disabled)).toBe(true));

      expect(post).not.toHaveBeenCalled();
      expect(messages()).toEqual([]);
    });

    it('applies the removals of that volume once confirmed, without forcing a re-process', async () => {
      const host = await mountWith(statusOf(photos, archive));
      const [first] = alerts(host);

      button(first as Element, 'Remove from the index').click();
      await pressConfirm('Remove from the index');
      await vi.waitFor(() => expect(post).toHaveBeenCalledTimes(1));

      expect(post.mock.calls[0]).toEqual(rescanBody(1, true));
      await vi.waitFor(() =>
        expect(messages()).toEqual([
          ['success', 'Removing the missing files of photos from the index'],
        ]),
      );
    });

    it('names the volume it is about when there are several', async () => {
      const host = await mountWith(statusOf(photos, archive));
      const [, second] = alerts(host);

      button(second as Element, 'Remove from the index').click();
      const dialog = await shownConfirm();

      expect(dialog.title).toBe('Remove 80 files from the index?');
      expect(dialog.message).toContain('80 files of volume archive');
      await pressConfirm('Cancel');
    });
  });

  describe('while a request runs', () => {
    it('disables every button and spins on the one that was pressed', async () => {
      const host = await mountWith(statusOf(photos, archive));
      const answer = deferred<ReturnType<typeof makeTask>>();
      post.mockReturnValueOnce(answer.promise);
      const [first, second] = alerts(host);

      button(second as Element, 'Rescan').click();
      await host.updateComplete;

      expect(buttons(host).every((each) => each.disabled)).toBe(true);
      expect(host.querySelectorAll('.loading')).toHaveLength(1);
      expect(button(second as Element, 'Rescan').querySelector('.loading')).not.toBeNull();
      expect(button(first as Element, 'Rescan').querySelector('.loading')).toBeNull();

      // A disabled button swallows the click: still exactly one request.
      button(first as Element, 'Rescan').click();
      expect(post).toHaveBeenCalledTimes(1);

      answer.resolve(makeTask({ kind: 'index', state: 'queued' }));
      await vi.waitFor(() => expect(messages()).toHaveLength(1));
      await host.updateComplete;

      expect(buttons(host).every((each) => !each.disabled)).toBe(true);
      expect(host.querySelector('.loading')).toBeNull();
    });

    it('also waits while the confirmation is open', async () => {
      const host = await mountWith(statusOf(photos));

      button(host, 'Remove from the index').click();
      await shownConfirm();
      await host.updateComplete;

      expect(buttons(host).every((each) => each.disabled)).toBe(true);
      expect(button(host, 'Remove from the index').querySelector('.loading')).not.toBeNull();

      await pressConfirm('Cancel');
      await vi.waitFor(() => expect(buttons(host).every((each) => !each.disabled)).toBe(true));
    });
  });

  describe('when the request fails', () => {
    it('shows the message of the server and lets the user try again', async () => {
      const host = await mountWith(statusOf(photos));
      post.mockRejectedValueOnce(
        new ApiError({ status: 409, code: 'volume_offline', message: 'Volume photos is offline' }),
      );

      button(host, 'Rescan').click();
      await vi.waitFor(() => expect(messages()).toEqual([['error', 'Volume photos is offline']]));
      await host.updateComplete;

      expect(buttons(host).every((each) => !each.disabled)).toBe(true);
      expect(host.querySelector('.loading')).toBeNull();

      button(host, 'Rescan').click();
      await vi.waitFor(() => expect(post).toHaveBeenCalledTimes(2));
    });

    it('also reports a failed removal', async () => {
      const host = await mountWith(statusOf(photos));
      post.mockRejectedValueOnce(new ApiError({ status: 500, code: 'internal', message: 'boom' }));

      button(host, 'Remove from the index').click();
      await pressConfirm('Remove from the index');

      await vi.waitFor(() => expect(messages()).toEqual([['error', 'boom']]));
    });

    it('falls back to a generic message for a failure without one', async () => {
      const host = await mountWith(statusOf(photos));
      post.mockRejectedValueOnce(undefined);

      button(host, 'Rescan').click();

      await vi.waitFor(() => expect(messages()).toEqual([['error', 'Could not start the scan']]));
    });
  });
});
