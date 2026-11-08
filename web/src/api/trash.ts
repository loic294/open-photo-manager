import { api } from '@/api/client';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TrashBatchesQuery } from '@/api/generated/TrashBatchesQuery';
import type { TrashBatchesResponse } from '@/api/generated/TrashBatchesResponse';
import type { TrashEmptyRequest } from '@/api/generated/TrashEmptyRequest';
import type { TrashListQuery } from '@/api/generated/TrashListQuery';
import type { TrashListResponse } from '@/api/generated/TrashListResponse';
import type { TrashRestoreRequest } from '@/api/generated/TrashRestoreRequest';

/**
 * `GET /api/trash`: one page of the trashed items (`items`), how many match (`total`) and the
 * global `summary` (it ignores the filters). Every field of `query` is sent when it is not
 * null; `order: null` keeps the natural direction of the sort. Rejects with an `ApiError`: 422
 * `validation_failed` (the fields say which values are wrong).
 */
export function listTrash(query: TrashListQuery, signal?: AbortSignal): Promise<TrashListResponse> {
  return api.get<TrashListResponse>('/api/trash', { query: { ...query }, signal });
}

/**
 * `GET /api/trash/batches`: one page of the deletions (the items trashed together by one task),
 * newest first, with the same global `summary` as `listTrash`.
 */
export function listTrashBatches(
  query: TrashBatchesQuery,
  signal?: AbortSignal,
): Promise<TrashBatchesResponse> {
  return api.get<TrashBatchesResponse>('/api/trash/batches', { query: { ...query }, signal });
}

/**
 * `POST /api/trash/restore`: queues an `fs_move` task that puts the chosen items back at their
 * original paths and answers 202 with it. The target is a list of item ids or a whole batch.
 * Send `conflict_policy: null` to use the setting. Rejects with an `ApiError`: 404 `not_found`
 * (an unknown volume), 409 `volume_offline`, 422 `validation_failed`.
 */
export function restoreTrash(request: TrashRestoreRequest, signal?: AbortSignal): Promise<TaskDto> {
  return api.post<TaskDto>('/api/trash/restore', { body: request, signal });
}

/**
 * `POST /api/trash/empty`: queues a `trash_empty` task that permanently deletes the items of
 * `volume_id` (null for every volume) whose id is at most `up_to_id`, so what was trashed after
 * the page was loaded survives. Rejects with an `ApiError`: 404 `not_found` (an unknown volume),
 * 409 `trash_already_empty` (nothing matches) or `volume_offline`.
 */
export function emptyTrash(request: TrashEmptyRequest, signal?: AbortSignal): Promise<TaskDto> {
  return api.post<TaskDto>('/api/trash/empty', { body: request, signal });
}

/**
 * `POST /api/trash/sync`: queues an `fs_trash` task that compares the trash folders on disk with
 * the list: it adopts what is on disk but unknown and forgets what is gone.
 */
export function syncTrash(signal?: AbortSignal): Promise<TaskDto> {
  return api.post<TaskDto>('/api/trash/sync', { signal });
}
