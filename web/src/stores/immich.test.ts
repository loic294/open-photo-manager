import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { events } from '@/api/sse';
import {
  applyImmichStatus,
  immichAttention,
  immichStatus,
  immichStatusError,
  loadImmichStatus,
  resetImmichForTests,
  startImmich,
} from '@/stores/immich';
import { resetTasksForTests, startTasks } from '@/stores/tasks';
import { FakeEventSource } from '@/test/fake-event-source';
import { sampleImmichStatus } from '@/test/immich-fixtures';
import { makeTask, taskEvent } from '@/test/task-fixtures';

const stops: (() => void)[] = [];
beforeEach(() => {
  resetTasksForTests();
  vi.stubGlobal('EventSource', FakeEventSource);
  FakeEventSource.reset();
  applyImmichStatus(sampleImmichStatus());
  vi.spyOn(api, 'get').mockImplementation(async (path) =>
    path === '/api/tasks' ? [] : sampleImmichStatus(),
  );
});
afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  events.stop();
  resetTasksForTests();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it('shares plain loads but never merges an explicit check into one', async () => {
  let resolve!: (value: unknown) => void;
  const promise = new Promise((res) => {
    resolve = res;
  });

  const get = vi.mocked(api.get).mockReturnValue(promise);
  const first = loadImmichStatus();
  expect(loadImmichStatus()).toBe(first);
  const check = loadImmichStatus({ check: true });
  expect(check).not.toBe(first);
  expect(get).toHaveBeenCalledTimes(2);
  resolve(sampleImmichStatus({ needs_attention: 3 }));
  await Promise.all([first, check]);
  expect(immichStatus.value?.needs_attention).toBe(3);
});

it('loads the first snapshot from null and makes immediate applied results visible', async () => {
  resetImmichForTests();
  expect(immichStatus.value).toBeNull();
  await loadImmichStatus();
  expect(immichStatus.value?.connected).toBe(true);
  expect(immichStatusError.value).toBeNull();
  const saved = sampleImmichStatus({ needs_attention: 5 });
  applyImmichStatus(saved);
  expect(immichStatus.value).toBe(saved);
  expect(immichAttention.value).toBe(true);
});

it('keeps the previous status on failure and resolves instead of rejecting', async () => {
  const previous = immichStatus.value;
  vi.mocked(api.get).mockRejectedValue(new Error('offline'));
  await expect(loadImmichStatus()).resolves.toBeUndefined();
  expect(immichStatus.value).toBe(previous);
  expect(immichStatusError.value).toBe('offline');
});

it('does not overwrite a connect or mapping save with an older response', async () => {
  let resolve!: (value: unknown) => void;
  vi.mocked(api.get).mockReturnValue(
    new Promise((res) => {
      resolve = res;
    }),
  );
  const load = loadImmichStatus();
  const current = sampleImmichStatus({ needs_attention: 9 });
  applyImmichStatus(current);
  resolve(sampleImmichStatus({ needs_attention: 1 }));
  await load;
  expect(immichStatus.value).toBe(current);
});

it.each([
  [false, false, 0, true, false],
  [false, true, 4, false, false],
  [true, false, 0, true, false],
  [true, false, 0, false, false],
  [true, true, 0, true, true],
  [true, false, 1, true, true],
])(
  'attention truth table: connected=%s reconnect=%s count=%s reachable=%s',
  (connected, needs_reconnect, needs_attention, reachable, expected) => {
    applyImmichStatus(
      sampleImmichStatus({ connected, needs_reconnect, needs_attention, reachable }),
    );
    expect(immichAttention.value).toBe(expected);
  },
);

it('loads initially and on resync, never polls during ten idle minutes', async () => {
  vi.useFakeTimers();
  stops.push(startImmich());
  await vi.advanceTimersByTimeAsync(0);
  const get = vi.mocked(api.get);
  expect(get).toHaveBeenCalledTimes(1);
  events.start();
  FakeEventSource.last().open();
  FakeEventSource.last().error();
  window.dispatchEvent(new Event('online'));
  FakeEventSource.last().open();
  await vi.advanceTimersByTimeAsync(0);
  expect(get).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(600000);
  expect(get).toHaveBeenCalledTimes(2);
});

it.each([
  'immich_sync',
  'immich_cleanup',
  'fs_move',
  'fs_trash',
  'fs_rmdir',
  'plan_execute',
  'trash_empty',
] as const)('debounces finished %s tasks and unsubscribes cleanly', async (kind) => {
  vi.useFakeTimers();
  stops.push(startTasks());
  const stop = startImmich();
  await vi.advanceTimersByTimeAsync(0);
  vi.mocked(api.get).mockClear();
  events.emit(taskEvent(makeTask({ id: 10, kind, state: 'running' })));
  events.emit(taskEvent(makeTask({ id: 10, kind, state: 'done' })));
  await vi.advanceTimersByTimeAsync(800);
  events.emit(taskEvent(makeTask({ id: 11, kind, state: 'failed' })));
  await vi.advanceTimersByTimeAsync(999);
  expect(api.get).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  expect(api.get).toHaveBeenCalledOnce();
  stop();
  events.emit(taskEvent(makeTask({ id: 12, kind, state: 'done' })));
  await vi.advanceTimersByTimeAsync(1000);
  expect(api.get).toHaveBeenCalledOnce();
});

it('ignores other finished kinds and progress updates', async () => {
  vi.useFakeTimers();
  stops.push(startTasks(), startImmich());
  await vi.advanceTimersByTimeAsync(0);
  vi.mocked(api.get).mockClear();
  events.emit(taskEvent(makeTask({ id: 30, kind: 'index', state: 'done' })));
  events.emit(taskEvent(makeTask({ id: 31, kind: 'immich_sync', state: 'running' })));
  await vi.advanceTimersByTimeAsync(2000);
  expect(api.get).not.toHaveBeenCalled();
});
