import { describe, expect, it } from 'vitest';
import type { TrashBatchDto } from '@/api/generated/TrashBatchDto';
import type { TrashSummary } from '@/api/generated/TrashSummary';
import type { TrashVolumeSummary } from '@/api/generated/TrashVolumeSummary';
import { makeTask } from '@/test/task-fixtures';
import {
  blockedReason,
  emptyBlockedReason,
  emptyConfirmation,
  emptyScope,
  itemsLabel,
  lastPageOffset,
  rangeText,
  reasonStyle,
  relativeTime,
  restoreBatchConfirmation,
  restorePolicyText,
  retentionText,
  taskOutcome,
} from '@/tools/trash/trash-text';
import { EM_DASH, formatFileTime } from '@/util/format';

function volume(overrides: Partial<TrashVolumeSummary> = {}): TrashVolumeSummary {
  return {
    volume_id: 1,
    name: 'photos',
    items: 10,
    bytes: 10_240,
    online: true,
    writable: true,
    ...overrides,
  };
}

function summary(volumes: TrashVolumeSummary[]): TrashSummary {
  return {
    items: volumes.reduce((total, v) => total + v.items, 0),
    bytes: volumes.reduce((total, v) => total + v.bytes, 0),
    max_id: 99,
    volumes,
    retention_days: null,
    conflict_policy: 'smart',
  };
}

describe('itemsLabel', () => {
  it('pluralises and groups thousands', () => {
    expect(itemsLabel(0)).toBe('0 items');
    expect(itemsLabel(1)).toBe('1 item');
    expect(itemsLabel(2)).toBe('2 items');
    expect(itemsLabel(1250)).toBe(`${(1250).toLocaleString()} items`);
  });
});

describe('retentionText', () => {
  it('says the trash is kept until it is emptied when the setting is off', () => {
    expect(retentionText(null)).toBe('Items stay in the trash until you empty it.');
  });

  it('names the number of days', () => {
    expect(retentionText(30)).toBe(
      'Items are deleted automatically 30 days after they were trashed.',
    );
    expect(retentionText(1)).toBe('Items are deleted automatically 1 day after they were trashed.');
  });
});

describe('restorePolicyText', () => {
  it('describes each conflict policy', () => {
    expect(restorePolicyText('smart')).toContain('gets a number');
    expect(restorePolicyText('smart')).toContain('identical file stays in the trash');
    expect(restorePolicyText('rename')).toBe('A name that is already taken gets a number.');
    expect(restorePolicyText('skip')).toBe(
      'An item whose name is already taken stays in the trash.',
    );
    expect(restorePolicyText('replace')).toContain('goes to the trash and is replaced');
  });
});

describe('reasonStyle', () => {
  it('words and colours the reasons fsops writes', () => {
    expect(reasonStyle('user')).toMatchObject({ label: 'Deleted', badge: 'badge-neutral' });
    expect(reasonStyle('replaced')).toMatchObject({ label: 'Replaced', badge: 'badge-warning' });
    expect(reasonStyle('junk')).toMatchObject({ label: 'Junk', badge: 'badge-info' });
    expect(reasonStyle('moved')).toMatchObject({ label: 'Moved', badge: 'badge-accent' });
  });

  it('calls an item without a reason "Found on disk"', () => {
    expect(reasonStyle(null).label).toBe('Found on disk');
  });

  it('shows a reason it does not know as it is', () => {
    expect(reasonStyle('cleanup')).toMatchObject({ label: 'cleanup', hint: 'cleanup' });
  });
});

describe('relativeTime', () => {
  const now = 1_700_000_000;

  it.each([
    [0, 'just now'],
    [59, 'just now'],
    [60, '1 minute ago'],
    [300, '5 minutes ago'],
    [3_600, '1 hour ago'],
    [3 * 3_600 + 10, '3 hours ago'],
    [86_400, 'yesterday'],
    [3 * 86_400, '3 days ago'],
    [45 * 86_400, 'last month'],
    [100 * 86_400, '3 months ago'],
    [400 * 86_400, 'last year'],
    [800 * 86_400, '2 years ago'],
  ])('reads %i seconds as "%s"', (age, expected) => {
    expect(relativeTime(now - age, now)).toBe(expected);
  });

  it('reads the future (a clock that runs ahead) as "just now"', () => {
    expect(relativeTime(now + 500, now)).toBe('just now');
  });

  it('shows an em dash for something that is not a time', () => {
    expect(relativeTime(Number.NaN, now)).toBe(EM_DASH);
  });
});

describe('blockedReason', () => {
  it('has no reason for a volume that works, or one it does not know', () => {
    expect(blockedReason(volume(), 'restore')).toBeNull();
    expect(blockedReason(undefined, 'delete')).toBeNull();
  });

  it('says that an offline volume must come back', () => {
    expect(blockedReason(volume({ online: false }), 'restore')).toBe(
      'Volume photos is offline: its items cannot be restored until it is back.',
    );
    expect(blockedReason(volume({ online: false, writable: false }), 'delete')).toBe(
      'Volume photos is offline: its items cannot be deleted until it is back.',
    );
  });

  it('says that a read-only volume cannot be changed', () => {
    expect(blockedReason(volume({ writable: false }), 'restore')).toBe(
      'Volume photos is read-only: its items cannot be restored.',
    );
  });
});

describe('restoreBatchConfirmation', () => {
  const batch: TrashBatchDto = {
    volume_id: 1,
    volume: 'photos',
    batch: '20231114-221320-t7',
    task_id: 7,
    trashed_at: 1_700_000_000,
    items: 250,
    bytes: 1_572_864,
    reasons: ['user'],
  };

  it('states the exact count, size, volume and time, and what a taken name does', () => {
    const { title, message, confirmLabel } = restoreBatchConfirmation(batch, 'smart');

    expect(title).toBe('Restore 250 items?');
    expect(confirmLabel).toBe('Restore 250 items');
    expect(message).toBe(
      `250 items (1.5 MB) deleted on ${formatFileTime(1_700_000_000)} go back to their original ` +
        'folders on volume photos. A name that is already taken gets a number, and an identical ' +
        'file stays in the trash.',
    );
  });

  it('uses the policy of the settings', () => {
    expect(restoreBatchConfirmation(batch, 'skip').message).toContain(
      'An item whose name is already taken stays in the trash.',
    );
  });

  it('writes one item in the singular', () => {
    expect(restoreBatchConfirmation({ ...batch, items: 1 }, 'smart').title).toBe('Restore 1 item?');
  });
});

describe('emptyScope', () => {
  const photos = volume({ volume_id: 1, name: 'photos', items: 12, bytes: 1_572_864 });
  const archive = volume({ volume_id: 2, name: 'archive', items: 3, bytes: 2_048 });

  it('covers every volume with the totals of the summary', () => {
    const scope = emptyScope(summary([photos, archive]), null);

    expect(scope).toMatchObject({
      volumeId: null,
      where: 'all volumes',
      items: 15,
      bytes: 1_574_912,
      blocked: [],
      deletable: 15,
    });
  });

  it('covers one volume with its own numbers', () => {
    const scope = emptyScope(summary([photos, archive]), 2);

    expect(scope).toMatchObject({ volumeId: 2, where: 'archive', items: 3, bytes: 2_048 });
  });

  it('lists the volumes whose items cannot be deleted and leaves their items out of deletable', () => {
    const offline = { ...archive, online: false };
    const scope = emptyScope(summary([photos, offline]), null);

    expect(scope.blocked).toEqual([offline]);
    expect(scope.items).toBe(15);
    expect(scope.deletable).toBe(12);
  });

  it('has nothing to delete in a volume that is read-only', () => {
    const scope = emptyScope(summary([{ ...photos, writable: false }]), 1);

    expect(scope.deletable).toBe(0);
  });

  it('has nothing to delete in a volume the summary does not list', () => {
    expect(emptyScope(summary([photos]), 9)).toMatchObject({
      where: 'this volume',
      items: 0,
      deletable: 0,
    });
  });
});

describe('emptyBlockedReason', () => {
  const photos = volume({ volume_id: 1, name: 'photos', items: 12 });

  it('has no reason while there is something to delete', () => {
    expect(emptyBlockedReason(emptyScope(summary([photos]), null))).toBeNull();
    expect(
      emptyBlockedReason(
        emptyScope(summary([photos, { ...photos, volume_id: 2, online: false }]), null),
      ),
    ).toBeNull();
  });

  it('waits for the summary', () => {
    expect(emptyBlockedReason(null)).toBe('The trash is still loading.');
  });

  it('says that the items of an offline or read-only volume cannot be deleted', () => {
    expect(emptyBlockedReason(emptyScope(summary([{ ...photos, online: false }]), 1))).toBe(
      'Volume photos is offline: its items cannot be deleted until it is back.',
    );
    expect(emptyBlockedReason(emptyScope(summary([{ ...photos, writable: false }]), null))).toBe(
      'Volume photos is read-only: its items cannot be deleted.',
    );
  });

  it('says that the trash is empty', () => {
    expect(emptyBlockedReason(emptyScope(summary([]), null))).toBe('The trash is empty.');
    expect(emptyBlockedReason(emptyScope(summary([photos]), 9))).toBe(
      'The trash on this volume is empty.',
    );
  });
});

describe('emptyConfirmation', () => {
  it('asks about every volume with the exact count and size', () => {
    const scope = emptyScope(
      summary([volume({ items: 1_234, bytes: 5_368_709_120 }), volume({ volume_id: 2, items: 0 })]),
      null,
    );

    expect(emptyConfirmation(scope)).toEqual({
      title: 'Empty the trash?',
      message: `Permanently delete ${(1_234).toLocaleString()} items (5 GB) from all volumes? This cannot be undone.`,
      confirmLabel: 'Empty trash',
    });
  });

  it('names the volume when one is filtered', () => {
    const scope = emptyScope(
      summary([volume({ name: 'archive', volume_id: 2, items: 1, bytes: 512 })]),
      2,
    );

    expect(emptyConfirmation(scope)).toEqual({
      title: 'Empty the trash on archive?',
      message: 'Permanently delete 1 item (512 B) from archive? This cannot be undone.',
      confirmLabel: 'Empty trash on archive',
    });
  });

  it('warns that items of an offline or read-only volume stay', () => {
    const scope = emptyScope(
      summary([volume(), volume({ volume_id: 2, name: 'archive', online: false })]),
      null,
    );

    expect(emptyConfirmation(scope).message).toMatch(
      /This cannot be undone\. Items on offline or read-only volumes \(archive\) cannot be deleted: they stay in the trash\.$/,
    );
  });
});

describe('rangeText and lastPageOffset', () => {
  it('writes the range of the page', () => {
    expect(rangeText(50, 50, 250)).toBe('51\u2013100 of 250');
    expect(rangeText(0, 1, 1)).toBe('1\u20131 of 1');
    expect(rangeText(0, 0, 0)).toBe('No items');
  });

  it('finds the first row of the last page', () => {
    expect(lastPageOffset(0, 50)).toBe(0);
    expect(lastPageOffset(50, 50)).toBe(0);
    expect(lastPageOffset(51, 50)).toBe(50);
    expect(lastPageOffset(250, 50)).toBe(200);
  });
});

describe('taskOutcome', () => {
  it('reports a task that did everything as a success', () => {
    expect(taskOutcome(makeTask({ state: 'done', title: 'Restored 5 items' }))).toEqual({
      kind: 'success',
      message: 'Restored 5 items',
    });
  });

  it('warns when items were left behind', () => {
    const left = makeTask({
      state: 'done',
      title: 'Restored 118 of 120 items, 2 left in the trash',
    });
    const fewer = makeTask({ state: 'done', title: 'Deleted 4 of 5 items' });

    expect(taskOutcome(left).kind).toBe('warning');
    expect(taskOutcome(fewer).kind).toBe('warning');
  });

  it('warns when a check of the trash held something back', () => {
    const check = (title: string) =>
      taskOutcome(makeTask({ kind: 'fs_trash', state: 'done', title }));

    expect(check('The trash matches the disk')).toEqual({
      kind: 'success',
      message: 'The trash matches the disk',
    });
    expect(check('Checked the trash: added 3 items found on the disk').kind).toBe('success');
    expect(
      check('Checked the trash: kept 2 items whose file is missing (is a share unmounted?)').kind,
    ).toBe('warning');
    expect(check('Checked the trash: skipped 1 entry that could not be read').kind).toBe('warning');
    expect(
      check('Checked the trash: removed 1 item whose file is gone, 1 volume offline').kind,
    ).toBe('warning');
  });

  it('reads the cautions of a check of the trash in the title of that task only', () => {
    const task = makeTask({ kind: 'fs_move', state: 'done', title: 'Restored 1 item to offline' });

    expect(taskOutcome(task).kind).toBe('success');
  });

  it('shows the error of a failed task, or says it failed', () => {
    const error = 'Restored 1 of 2 items (1 failed): a/b.jpg (source_missing: gone)';

    expect(taskOutcome(makeTask({ state: 'failed', title: 'Restoring 2 items', error }))).toEqual({
      kind: 'error',
      message: error,
    });
    expect(taskOutcome(makeTask({ state: 'failed', title: 'Emptying the trash' })).message).toBe(
      'Emptying the trash failed',
    );
  });

  it('reports an interruption as an error and a cancellation as information', () => {
    expect(taskOutcome(makeTask({ state: 'interrupted', title: 'Emptying the trash' }))).toEqual({
      kind: 'error',
      message: 'Emptying the trash was interrupted',
    });
    expect(taskOutcome(makeTask({ state: 'cancelled', title: 'Emptying the trash' }))).toEqual({
      kind: 'info',
      message: 'Emptying the trash was cancelled',
    });
  });
});
