import type { ServerEvent } from '@/api/generated/ServerEvent';
import type { TaskDto } from '@/api/generated/TaskDto';

/** A running file move that has not reported any progress yet; override what a test is about. */
export function makeTask(overrides: Partial<TaskDto> = {}): TaskDto {
  return {
    id: 1,
    kind: 'fs_move',
    title: 'Move 3 files',
    state: 'running',
    items_done: 0,
    items_total: null,
    bytes_done: 0,
    bytes_total: null,
    speed: null,
    eta_secs: null,
    error: null,
    started_at: 1_000,
    finished_at: null,
    ...overrides,
  };
}

/** The `task` event the server sends when `makeTask(overrides)` changes. */
export function taskEvent(
  overrides: Partial<TaskDto> = {},
): Extract<ServerEvent, { type: 'task' }> {
  return { type: 'task', task: makeTask(overrides) };
}
