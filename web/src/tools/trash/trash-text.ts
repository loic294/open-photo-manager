import type { ConflictPolicy } from '@/api/generated/ConflictPolicy';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TrashBatchDto } from '@/api/generated/TrashBatchDto';
import type { TrashSummary } from '@/api/generated/TrashSummary';
import type { TrashVolumeSummary } from '@/api/generated/TrashVolumeSummary';
import { EM_DASH, formatFileSize, formatFileTime } from '@/util/format';
import { formatCount } from '@/util/format-count';

/** Everything `confirmDialog` needs to ask one question. */
export interface ConfirmText {
  title: string;
  message: string;
  confirmLabel: string;
}

/** "1 item", "2 items", "1,250 items". */
export function itemsLabel(count: number): string {
  return `${formatCount(count)} ${count === 1 ? 'item' : 'items'}`;
}

/** The retention rule, as the Trash page states it. `null` is the setting being off. */
export function retentionText(days: number | null): string {
  if (days === null) return 'Items stay in the trash until you empty it.';
  const unit = days === 1 ? 'day' : 'days';
  return `Items are deleted automatically ${formatCount(days)} ${unit} after they were trashed.`;
}

/** What a restore does when a name is already taken, for each conflict policy of the settings. */
const POLICY_TEXT: Readonly<Record<ConflictPolicy, string>> = {
  smart: 'A name that is already taken gets a number, and an identical file stays in the trash.',
  rename: 'A name that is already taken gets a number.',
  skip: 'An item whose name is already taken stays in the trash.',
  replace: 'A file that is already there goes to the trash and is replaced.',
};

export function restorePolicyText(policy: ConflictPolicy): string {
  return POLICY_TEXT[policy] ?? POLICY_TEXT.smart;
}

interface ReasonStyle {
  label: string;
  /** Complete class names, so Tailwind finds them. */
  badge: string;
  hint: string;
}

/** The `reason` values `fsops` writes; an item that sync found on disk has none (`null`). */
const REASONS: Readonly<Record<string, ReasonStyle>> = {
  user: { label: 'Deleted', badge: 'badge-neutral', hint: 'Deleted by you' },
  replaced: {
    label: 'Replaced',
    badge: 'badge-warning',
    hint: 'Trashed when a file with the same name replaced it',
  },
  junk: {
    label: 'Junk',
    badge: 'badge-info',
    hint: 'A junk file that was removed with its folder',
  },
  moved: {
    label: 'Moved',
    badge: 'badge-accent',
    hint: 'The original of a file that was moved to another volume',
  },
};

const FOUND_ON_DISK: ReasonStyle = {
  label: 'Found on disk',
  badge: 'badge-ghost',
  hint: 'Found in the trash folder on disk: it was not trashed through this app',
};

/** How the reason of an item is written and drawn. An unknown reason shows as it is. */
export function reasonStyle(reason: string | null): ReasonStyle {
  if (reason === null) return FOUND_ON_DISK;
  return REASONS[reason] ?? { label: reason, badge: 'badge-ghost', hint: reason };
}

const RELATIVE = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });

const RELATIVE_UNITS: readonly (readonly [Intl.RelativeTimeFormatUnit, number])[] = [
  ['year', 365 * 86_400],
  ['month', 30 * 86_400],
  ['day', 86_400],
  ['hour', 3_600],
  ['minute', 60],
];

/**
 * An instant as "3 days ago" or "yesterday" (the exact time goes in a `title`). Under a minute,
 * and any time in the future (a clock that runs ahead), is "just now".
 */
export function relativeTime(epochSecs: number, nowSecs: number = Date.now() / 1000): string {
  const age = nowSecs - epochSecs;
  if (!Number.isFinite(age)) return EM_DASH;
  for (const [unit, seconds] of RELATIVE_UNITS) {
    if (age >= seconds) return RELATIVE.format(-Math.floor(age / seconds), unit);
  }
  return 'just now';
}

export type VolumeAction = 'restore' | 'delete';

/**
 * Why the items of a volume cannot be restored or deleted right now, `null` when they can (or
 * when the volume is not known). Shown as the tooltip of the disabled button.
 */
export function blockedReason(
  volume: Pick<TrashVolumeSummary, 'name' | 'online' | 'writable'> | undefined,
  action: VolumeAction,
): string | null {
  if (!volume) return null;
  const done = action === 'restore' ? 'restored' : 'deleted';
  if (!volume.online) {
    return `Volume ${volume.name} is offline: its items cannot be ${done} until it is back.`;
  }
  if (!volume.writable) {
    return `Volume ${volume.name} is read-only: its items cannot be ${done}.`;
  }
  return null;
}

/** The question asked before a whole deletion is restored. */
export function restoreBatchConfirmation(
  batch: TrashBatchDto,
  policy: ConflictPolicy,
): ConfirmText {
  const items = itemsLabel(batch.items);
  return {
    title: `Restore ${items}?`,
    message:
      `${items} (${formatFileSize(batch.bytes)}) deleted on ${formatFileTime(batch.trashed_at)} ` +
      `go back to their original folders on volume ${batch.volume}. ${restorePolicyText(policy)}`,
    confirmLabel: `Restore ${items}`,
  };
}

/** What "Empty trash" is about to delete: the numbers the user is looking at. */
export interface EmptyScope {
  /** The volume to empty, `null` for all of them. */
  volumeId: number | null;
  /** "all volumes" or the name of the volume. */
  where: string;
  items: number;
  bytes: number;
  /** The volumes of the scope whose items cannot be deleted now (offline or read-only). */
  blocked: readonly TrashVolumeSummary[];
  /** The items of the scope that are not on a blocked volume. */
  deletable: number;
}

/** The scope of "Empty trash" for the summary on screen and the volume filter (`null`: none). */
export function emptyScope(summary: TrashSummary, volumeId: number | null): EmptyScope {
  const all = volumeId === null;
  const volumes = all
    ? summary.volumes
    : summary.volumes.filter((volume) => volume.volume_id === volumeId);
  const blocked = volumes.filter((volume) => blockedReason(volume, 'delete') !== null);
  const sum = (pick: (volume: TrashVolumeSummary) => number) =>
    volumes.reduce((total, volume) => total + pick(volume), 0);
  return {
    volumeId,
    where: all ? 'all volumes' : (volumes[0]?.name ?? 'this volume'),
    items: all ? summary.items : sum((volume) => volume.items),
    bytes: all ? summary.bytes : sum((volume) => volume.bytes),
    blocked,
    deletable: Math.max(
      0,
      sum((volume) => volume.items) - blocked.reduce((total, volume) => total + volume.items, 0),
    ),
  };
}

/** Why "Empty trash" cannot be used now, `null` when it can. `null` for the scope: not loaded yet. */
export function emptyBlockedReason(scope: EmptyScope | null): string | null {
  if (scope === null) return 'The trash is still loading.';
  if (scope.deletable > 0) return null;
  const [first] = scope.blocked;
  if (first && scope.items > 0) return blockedReason(first, 'delete');
  return scope.volumeId === null ? 'The trash is empty.' : `The trash on ${scope.where} is empty.`;
}

/** The question asked before the trash is emptied: the one place where files are lost for good. */
export function emptyConfirmation(scope: EmptyScope): ConfirmText {
  const everything = scope.volumeId === null;
  const lead =
    `Permanently delete ${itemsLabel(scope.items)} (${formatFileSize(scope.bytes)}) ` +
    `from ${scope.where}? This cannot be undone.`;
  const names = scope.blocked.map((volume) => volume.name).join(', ');
  return {
    title: everything ? 'Empty the trash?' : `Empty the trash on ${scope.where}?`,
    message:
      scope.blocked.length > 0
        ? `${lead} Items on offline or read-only volumes (${names}) cannot be deleted: they stay in the trash.`
        : lead,
    confirmLabel: everything ? 'Empty trash' : `Empty trash on ${scope.where}`,
  };
}

/** "51–100 of 250", or "No items" for an empty list. `count` is how many rows are on the page. */
export function rangeText(offset: number, count: number, total: number): string {
  if (total === 0 || count === 0) return 'No items';
  return `${formatCount(offset + 1)}\u2013${formatCount(offset + count)} of ${formatCount(total)}`;
}

/** The offset of the last page, so a page that vanished can step back to the one before. */
export function lastPageOffset(total: number, limit: number): number {
  return total > 0 ? Math.floor((total - 1) / limit) * limit : 0;
}

export type OutcomeKind = 'success' | 'info' | 'warning' | 'error';

/** A sync that held something back says so: "kept 3 items whose file is missing", "2 volumes offline". */
const SYNC_CAUTION = /\b(?:kept|skipped)\b|offline/;

/**
 * The toast that tells how a task the user started ended. `done` with part of the items left
 * behind ("Restored 118 of 120 items, 2 left in the trash", or a check of the trash that kept
 * something back) is a warning, a failure shows the error text of the task.
 */
export function taskOutcome(task: TaskDto): { kind: OutcomeKind; message: string } {
  switch (task.state) {
    case 'done': {
      const partial =
        task.title.includes('left in the trash') ||
        /\d of \d/.test(task.title) ||
        (task.kind === 'fs_trash' && SYNC_CAUTION.test(task.title));
      return { kind: partial ? 'warning' : 'success', message: task.title };
    }
    case 'failed':
      return { kind: 'error', message: task.error ?? `${task.title} failed` };
    case 'interrupted':
      return { kind: 'error', message: task.error ?? `${task.title} was interrupted` };
    case 'cancelled':
      return { kind: 'info', message: `${task.title} was cancelled` };
    default:
      return { kind: 'info', message: task.title };
  }
}
