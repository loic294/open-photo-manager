import type { TrashBatchDto } from '@/api/generated/TrashBatchDto';
import type { TrashItemDto } from '@/api/generated/TrashItemDto';
import type { TrashSummary } from '@/api/generated/TrashSummary';
import type { TrashVolumeSummary } from '@/api/generated/TrashVolumeSummary';

/** The instant the fixtures were trashed (epoch seconds); a test that reads times fakes the clock. */
export const TRASHED_AT = 1_735_732_800;

/** A volume that works and holds 10 items; override what a test is about. */
export function makeTrashVolume(overrides: Partial<TrashVolumeSummary> = {}): TrashVolumeSummary {
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

/** The trash of the given volumes (one healthy volume by default); the totals follow the volumes. */
export function makeTrashSummary(
  overrides: Partial<TrashSummary> & { volumes?: TrashVolumeSummary[] } = {},
): TrashSummary {
  const volumes = overrides.volumes ?? [makeTrashVolume()];
  return {
    items: volumes.reduce((total, volume) => total + volume.items, 0),
    bytes: volumes.reduce((total, volume) => total + volume.bytes, 0),
    max_id: 99,
    retention_days: null,
    conflict_policy: 'smart',
    ...overrides,
    volumes,
  };
}

/** A file a user deleted. */
export function makeTrashItem(overrides: Partial<TrashItemDto> = {}): TrashItemDto {
  return {
    id: 1,
    volume_id: 1,
    volume: 'photos',
    name: 'IMG_0001.jpg',
    dir: '2024/Trip',
    is_dir: false,
    size: 2_048,
    reason: 'user',
    batch: '20250101-120000-t7',
    task_id: 7,
    trashed_at: TRASHED_AT,
    has_snapshot: true,
    ...overrides,
  };
}

/** One deletion of 3 items. */
export function makeTrashBatch(overrides: Partial<TrashBatchDto> = {}): TrashBatchDto {
  return {
    volume_id: 1,
    volume: 'photos',
    batch: '20250101-120000-t7',
    task_id: 7,
    trashed_at: TRASHED_AT,
    items: 3,
    bytes: 6_144,
    reasons: ['user'],
    ...overrides,
  };
}
