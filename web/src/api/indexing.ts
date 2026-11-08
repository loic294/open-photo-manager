import { api } from '@/api/client';
import type { IndexStatusDto } from '@/api/generated/IndexStatusDto';
import type { RescanRequest } from '@/api/generated/RescanRequest';
import type { TaskDto } from '@/api/generated/TaskDto';

/**
 * `POST /api/index/rescan`: queues an `index` task for the scopes of `request`, or joins one that
 * is still waiting, and answers 202 with that task before any file is read: follow it through the
 * `task` events. A scope with a null `volume_id` and an empty `dir` stands for every online
 * volume, and an empty `targets` list for every volume. Rejects with an `ApiError`: 400
 * `invalid_path` or `bad_request`, 404 `not_found` (an unknown volume), 409 `volume_offline`.
 */
export function rescan(request: RescanRequest, signal?: AbortSignal): Promise<TaskDto> {
  return api.post<TaskDto>('/api/index/rescan', { body: request, signal });
}

/** `GET /api/index/status`: the removals the scanner's safety guard holds back, per volume. */
export function getIndexStatus(signal?: AbortSignal): Promise<IndexStatusDto> {
  return api.get<IndexStatusDto>('/api/index/status', { signal });
}
