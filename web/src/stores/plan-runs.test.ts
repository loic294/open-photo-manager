import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import type { PlanDto } from '@/api/generated/PlanDto';
import type { PlanResult } from '@/api/generated/PlanResult';
import * as plans from '@/api/plans';
import { events } from '@/api/sse';
import {
  dismissLastRun,
  lastRun,
  type PlanRun,
  resetPlanRunsForTests,
  trackPlanRun,
} from '@/stores/plan-runs';
import { clearToasts, toasts } from '@/stores/toasts';
import { FakeEventSource } from '@/test/fake-event-source';
import { taskEvent } from '@/test/task-fixtures';

const run: PlanRun = { tool: 'duplicates', planId: 5, taskId: 8, title: 'Delete files' };
const result: PlanResult = {
  done: 10,
  failed: 0,
  skipped: 0,
  bytes: 123,
  failures: [],
  cancelled: false,
  note: null,
};
const plan = (patch: Partial<PlanResult> = {}): PlanDto => ({
  id: 5,
  kind: 'duplicates',
  state: 'executed',
  stats: {},
  warnings: [],
  result: { ...result, ...patch },
  task_id: 8,
  created_at: 0,
  expires_at: 7200,
});
const flush = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve();
};

beforeEach(() => {
  vi.spyOn(api, 'get').mockResolvedValue([]);
});
afterEach(() => {
  resetPlanRunsForTests();
  clearToasts();
  events.stop();
  FakeEventSource.reset();
  document.querySelector('opm-plan-result')?.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('runs outlive the view', () => {
  it('fetches the result once, retains it and announces a clean success', async () => {
    const get = vi.spyOn(plans, 'getPlan').mockResolvedValue(plan());
    trackPlanRun(run);
    events.emit(taskEvent({ id: 8, state: 'done', title: 'Deleted 10 files' }));
    await flush();
    expect(get).toHaveBeenCalledWith(5);
    expect(lastRun('duplicates').value).toMatchObject({
      ...run,
      title: 'Deleted 10 files',
      result,
    });
    expect(toasts.value[0]).toMatchObject({ kind: 'success', message: 'Deleted 10 files' });
    expect(toasts.value[0]?.action).toBeUndefined();
    events.emit(taskEvent({ id: 8, state: 'done' }));
    await flush();
    expect(get).toHaveBeenCalledOnce();
    dismissLastRun('duplicates');
    expect(lastRun('duplicates').value).toBeNull();
  });

  it.each([{ failed: 2 }, { skipped: 3 }])(
    'offers Details for %j, opening one singleton result dialog',
    async (patch) => {
      vi.spyOn(plans, 'getPlan').mockResolvedValue(plan(patch));
      trackPlanRun(run);
      events.emit(taskEvent({ id: 8, state: 'done', title: 'Completed with omissions' }));
      await flush();
      expect(toasts.value[0]).toMatchObject({
        kind: 'warning',
        action: { label: 'Details' },
        timeout: 10000,
      });
      toasts.value[0]?.action?.run();
      toasts.value[0]?.action?.run();
      expect(document.querySelectorAll('opm-plan-result')).toHaveLength(1);
      const host = document.querySelector('opm-plan-result');
      expect(host?.run?.result).toMatchObject(patch);
    },
  );

  it.each([
    ['failed', 'error', 'Disk offline'],
    ['cancelled', 'info', 'Cancelled deletion'],
    ['interrupted', 'warning', 'Interrupted deletion'],
  ] as const)('announces a %s task and stores its last run', async (state, kind, message) => {
    const get = vi.spyOn(plans, 'getPlan').mockResolvedValue(
      plan({
        failed: state === 'failed' ? 1 : 0,
        cancelled: state === 'cancelled',
        note: state === 'interrupted' ? 'Interrupted by a restart' : null,
      }),
    );
    trackPlanRun(run);
    events.emit(
      taskEvent({ id: 8, state, title: message, error: state === 'failed' ? message : null }),
    );
    await flush();
    expect(toasts.value[0]).toMatchObject({ kind, message });
    expect(lastRun('duplicates').value?.taskId).toBe(8);
    expect(get).toHaveBeenCalledOnce();
    expect(lastRun('duplicates').value?.result).toMatchObject({
      cancelled: state === 'cancelled',
    });
  });

  it('retries a failed result fetch once', async () => {
    const get = vi
      .spyOn(plans, 'getPlan')
      .mockRejectedValueOnce(new Error('Retry me'))
      .mockResolvedValueOnce(plan());
    trackPlanRun(run);
    events.emit(taskEvent({ id: 8, state: 'done' }));
    await flush();
    expect(get).toHaveBeenCalledTimes(2);
    expect(lastRun('duplicates').value?.result).toEqual(result);
  });

  it('also retries a result that is not yet available', async () => {
    const get = vi
      .spyOn(plans, 'getPlan')
      .mockResolvedValueOnce({ ...plan(), result: null })
      .mockResolvedValueOnce(plan());
    trackPlanRun(run);
    events.emit(taskEvent({ id: 8, state: 'done' }));
    await flush();
    expect(get).toHaveBeenCalledTimes(2);
    expect(lastRun('duplicates').value?.result).toEqual(result);
  });

  it('reports a second result-fetch failure without claiming success', async () => {
    vi.spyOn(plans, 'getPlan').mockRejectedValue(new Error('Could not read result'));
    trackPlanRun(run);
    events.emit(taskEvent({ id: 8, state: 'done' }));
    await flush();
    expect(toasts.value[0]).toMatchObject({ kind: 'error', message: 'Could not read result' });
    expect(lastRun('duplicates').value).toBeNull();
  });

  it('still reports the original failed task when its partial result cannot be fetched', async () => {
    vi.spyOn(plans, 'getPlan').mockRejectedValue(new Error('Could not read result'));
    trackPlanRun(run);
    events.emit(taskEvent({ id: 8, state: 'failed', error: 'Disk became offline' }));
    await flush();
    expect(toasts.value.map((toast) => toast.message)).toEqual([
      'Could not read result',
      'Disk became offline',
    ]);
    expect(lastRun('duplicates').value?.result).toBeNull();
  });

  it('subscribes once, ignores unrelated/nonterminal tasks and replaces lastRun on new execution', async () => {
    const on = vi.spyOn(events, 'on');
    vi.spyOn(plans, 'getPlan').mockResolvedValue(plan());
    trackPlanRun(run);
    events.emit(taskEvent({ id: 8, state: 'done' }));
    await flush();
    trackPlanRun({ ...run, taskId: 9 });
    expect(lastRun('duplicates').value).toBeNull();
    trackPlanRun({ ...run, tool: 'move', taskId: 10 });
    expect(on.mock.calls.filter(([type]) => type === 'task')).toHaveLength(1);
    events.emit(taskEvent({ id: 9, state: 'running' }));
    events.emit(taskEvent({ id: 99, state: 'done' }));
    await flush();
    expect(lastRun('duplicates').value).toBeNull();
    events.emit(taskEvent({ id: 9, state: 'done' }));
    await flush();
    expect(lastRun('duplicates').value?.taskId).toBe(9);
    expect(lastRun('move').value).toBeNull();
  });

  it('an older in-flight completion cannot overwrite a newer run', async () => {
    let resolve: (value: PlanDto) => void = () => {};
    vi.spyOn(plans, 'getPlan').mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    trackPlanRun(run);
    events.emit(taskEvent({ id: 8, state: 'done' }));
    trackPlanRun({ ...run, taskId: 9 });
    resolve(plan());
    await flush();
    expect(lastRun('duplicates').value).toBeNull();
  });

  it('recovers terminal events missed on reconnect', async () => {
    vi.stubGlobal('EventSource', FakeEventSource);
    events.start();
    FakeEventSource.last().open();
    trackPlanRun(run);
    await flush();
    vi.spyOn(api, 'get').mockResolvedValue([taskEvent({ id: 8, state: 'done' }).task]);
    vi.spyOn(plans, 'getPlan').mockResolvedValue(plan());
    events.stop();
    events.start();
    FakeEventSource.last().open();
    await flush();
    expect(lastRun('duplicates').value?.result).toEqual(result);
  });

  it('reset removes listeners and ignores pending completions', async () => {
    let resolve: (value: PlanDto) => void = () => {};
    const get = vi.spyOn(plans, 'getPlan').mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    trackPlanRun(run);
    events.emit(taskEvent({ id: 8, state: 'done' }));
    resetPlanRunsForTests();
    resolve(plan());
    await flush();
    events.emit(taskEvent({ id: 8, state: 'done' }));
    expect(get).toHaveBeenCalledOnce();
    expect(lastRun('duplicates').value).toBeNull();
    expect(toasts.value).toEqual([]);
  });
});
