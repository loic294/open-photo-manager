import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { events } from '@/api/sse';
import { applyImmichStatus } from '@/stores/immich';
import { resetTasksForTests, startTasks } from '@/stores/tasks';
import { mount } from '@/test/helpers';
import { sampleImmichStatus } from '@/test/immich-fixtures';
import { makeTask, taskEvent } from '@/test/task-fixtures';
import '@/tools/settings/immich/apm-immich-sync';

let stop: (() => void) | undefined;
beforeEach(() => {
  resetTasksForTests();
  applyImmichStatus(sampleImmichStatus());
  vi.spyOn(api, 'get').mockResolvedValue([]);
  vi.spyOn(api, 'post').mockResolvedValue(makeTask({ kind: 'immich_sync' }));
});
afterEach(() => {
  stop?.();
  resetTasksForTests();
  vi.restoreAllMocks();
});

it('shows never synced and posts the two exact nonblocking task bodies', async () => {
  const el = await mount('apm-immich-sync');
  expect(el.textContent).toContain('Not synced yet');
  const buttons = el.querySelectorAll('button');
  buttons[0]?.click();
  await vi.waitFor(() =>
    expect(api.post).toHaveBeenCalledWith('/api/immich/sync', { body: { full: false } }),
  );
  await vi.waitFor(() => expect(buttons[1]?.disabled).toBe(false));
  buttons[1]?.click();
  await vi.waitFor(() =>
    expect(api.post).toHaveBeenCalledWith('/api/immich/sync', { body: { full: true } }),
  );
});

it('uses the task store progress and disables a second sync', async () => {
  stop = startTasks();
  events.emit(
    taskEvent(
      makeTask({
        id: 70,
        kind: 'immich_sync',
        state: 'running',
        title: 'Reading Immich',
        items_done: 5,
        items_total: 10,
      }),
    ),
  );
  const el = await mount('apm-immich-sync');
  expect(el.querySelector('progress')?.value).toBe(0.5);
  expect(el.textContent).toContain('Reading Immich');
  expect([...el.querySelectorAll('button')].every((button) => button.disabled)).toBe(true);
});

it('makes failed scopes identifiable and warns about live assets with zero matches', async () => {
  applyImmichStatus(
    sampleImmichStatus({
      last_sync_at: 1700000000,
      sync: {
        assets_total: 20,
        assets_live: 15,
        assets_matched: 0,
        assets_unmapped: 20,
        last_sync_at: 1700000000,
        last_full_at: 1700000000,
        last_error: 'immich_unreachable',
        scopes: [
          {
            scope: 'missing',
            library_id: 'missing',
            library_name: 'Missing library',
            path_prefix: null,
            assets_total: 20,
            assets_live: 15,
            assets_matched: 0,
            assets_unmapped: 20,
            last_sync_at: 1700000000,
            last_full_at: 1700000000,
            last_error: 'immich_unreachable',
            consecutive_failures: 3,
          },
        ],
      },
    }),
  );
  const el = await mount('apm-immich-sync');
  expect(el.textContent).toContain('Last synced');
  expect(el.textContent).toContain('No Immich asset matched');
  expect(el.textContent).toContain('Missing library');
  expect(el.textContent).toContain('could not be reached');
  expect(el.textContent).toContain('Failed');
});
