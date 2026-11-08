import { api } from '@/api/client';
import type { TaskDto } from '@/api/generated/TaskDto';

/**
 * `GET /api/tasks`: a bare array with the active tasks first (running, then queued) and the newest
 * 100 finished ones after them, newest first. The server forgets finished tasks after 7 days.
 */
export function listTasks(signal?: AbortSignal): Promise<TaskDto[]> {
  return api.get<TaskDto[]>('/api/tasks', { signal });
}

/**
 * `POST /api/tasks/{id}/cancel`: answers 202 with the task as it is now. A queued task is already
 * `cancelled` in that answer; a running one ends later, when its runner returns (a `task` event
 * says so). Rejects with an `ApiError`: 404 `not_found` for an unknown (or forgotten) task, 409
 * `conflict` for a task that ended in another way than a cancellation.
 */
export function cancelTask(id: number): Promise<TaskDto> {
  return api.post<TaskDto>(`/api/tasks/${id}/cancel`);
}
