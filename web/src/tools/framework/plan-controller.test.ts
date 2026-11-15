import { signal } from '@preact/signals-core';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import type { PlanDto } from '@/api/generated/PlanDto';
import { events } from '@/api/sse';
import { saveSettings } from '@/stores/settings';
import { FakeEventSource } from '@/test/fake-event-source';
import { sampleSettings } from '@/test/settings-fixtures';
import { taskEvent } from '@/test/task-fixtures';
import { plainPane, toolConfig } from '@/test/tool-config';
import {
  INDEX_DEPENDENT_WARNINGS,
  PlanController,
  paramsKey,
} from '@/tools/framework/plan-controller';
import type { PlanParamsResult } from '@/tools/framework/tool-config';
import { createToolContext } from '@/tools/framework/tool-context';

const controllers: PlanController[] = [];
const plan = (id = 1, patch: Partial<PlanDto> = {}): PlanDto => ({
  id,
  kind: 'duplicates',
  state: 'ready',
  stats: { matches: 12 },
  warnings: [],
  result: null,
  task_id: null,
  created_at: 0,
  expires_at: 7200,
  ...patch,
});

function setup() {
  vi.useFakeTimers();
  const params = signal<PlanParamsResult>({
    ready: true,
    params: {
      source: { volume_id: 1, dir: 'a', recursive: true },
      comparison: { volume_id: 2, dir: 'b', recursive: true },
    },
  });
  const post = vi.spyOn(api, 'post').mockResolvedValue(plan());
  const remove = vi.spyOn(api, 'delete').mockResolvedValue(undefined);
  const ctx = createToolContext(
    toolConfig({
      id: 'duplicates',
      plan: { kind: 'duplicates', params: () => params.value },
      panes: {
        source: { ...plainPane('Source'), planSide: 'source' },
        comparison: { ...plainPane('Comparison'), planSide: 'comparison' },
      },
    }),
    document.createElement('div'),
  );
  if (!(ctx.plan instanceof PlanController)) throw new Error('Missing controller');
  controllers.push(ctx.plan);
  return { controller: ctx.plan, ctx, params, post, remove };
}

afterEach(() => {
  for (const controller of controllers.splice(0)) controller.dispose();
  events.stop();
  FakeEventSource.reset();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('paramsKey', () => {
  it('sorts nested keys without sorting arrays or normalizing path bytes', () => {
    expect(paramsKey({ b: 2, filter: { tags: ['b', 'a'], camera: null }, a: 1 })).toBe(
      paramsKey({ a: 1, filter: { camera: null, tags: ['b', 'a'] }, b: 2 }),
    );
    expect(paramsKey({ ids: [2, 1] })).not.toBe(paramsKey({ ids: [1, 2] }));
    expect(paramsKey({ dir: 'e\u0301' })).not.toBe(paramsKey({ dir: '\u00e9' }));
  });
});

describe('build lifecycle', () => {
  it('rebuilds when shared soft exclusions change, but not for unrelated settings', async () => {
    const { controller, post } = setup();
    await vi.advanceTimersByTimeAsync(300);
    expect(controller.ready.value).toBe(true);
    const put = vi.spyOn(api, 'put').mockResolvedValue(
      sampleSettings({
        soft_excluded_folders: [{ volume_id: 1, path: 'a/private' }],
      }),
    );
    await saveSettings(sampleSettings());
    expect(controller.ready.value).toBe(false);
    await vi.advanceTimersByTimeAsync(300);
    expect(post).toHaveBeenCalledTimes(2);
    put.mockResolvedValue(
      sampleSettings({
        soft_excluded_folders: [{ volume_id: 1, path: 'a/private' }],
        name_max_len: 143,
      }),
    );
    await saveSettings(sampleSettings());
    await vi.advanceTimersByTimeAsync(300);
    expect(post).toHaveBeenCalledTimes(2);
    controller.dispose();
    put.mockResolvedValue(sampleSettings());
    await saveSettings(sampleSettings());
  });
  it('contains parameter errors and drops an obsolete preview when inputs become not-ready', async () => {
    const { ctx, controller, params } = setup();
    await vi.advanceTimersByTimeAsync(300);
    const original = ctx.config.plan?.params;
    if (!ctx.config.plan || !original) throw new Error('Missing plan spec');
    ctx.config.plan.params = () => {
      params.value;
      throw new Error('Invalid parameters');
    };
    expect(() => {
      params.value = { ready: true, params: { version: 2 } };
    }).not.toThrow();
    expect(controller.error).toBe('Invalid parameters');
    ctx.config.plan.params = original;
    params.value = { ready: false, reason: 'Choose bases' };
    expect(controller.status).toBe('idle');
    expect(controller.planId).toBeNull();
  });

  it('disables immediately, builds after 300ms, and supplies both pane refs', async () => {
    const { controller, post } = setup();
    expect(controller.status).toBe('building');
    expect(controller.ready.value).toBe(false);
    await vi.advanceTimersByTimeAsync(299);
    expect(post).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(controller.status).toBe('ready');
    expect(controller.ready.value).toBe(true);
    expect(controller.stats).toEqual({ matches: 12 });
    expect(controller.ref('source')).toEqual({ plan_id: 1, side: 'source' });
    expect(controller.ref('comparison')).toEqual({ plan_id: 1, side: 'comparison' });
  });

  it('debounces bursts, keeps old pane refs, and sends replaces', async () => {
    const { controller, post, params } = setup();
    await vi.advanceTimersByTimeAsync(300);
    post.mockResolvedValue(plan(2));
    for (const value of [1, 2, 3]) {
      params.value = { ready: true, params: { value } };
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(controller.ready.value).toBe(false);
    expect(controller.planId).toBe(1);
    await vi.advanceTimersByTimeAsync(200);
    expect(post).toHaveBeenCalledTimes(2);
    expect(post.mock.calls[1]?.[1]?.body).toEqual({
      kind: 'duplicates',
      params: { value: 3 },
      replaces: 1,
    });
    expect(controller.planId).toBe(2);
  });

  it('aborts superseded requests and deletes their late completed response', async () => {
    const { controller, post, params, remove } = setup();
    let resolve: (value: PlanDto) => void = () => {};
    post.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    await vi.advanceTimersByTimeAsync(300);
    const signal = post.mock.calls[0]?.[1]?.signal;
    params.value = { ready: true, params: { version: 2 } };
    expect(signal?.aborted).toBe(true);
    post.mockResolvedValue(plan(3));
    await vi.advanceTimersByTimeAsync(300);
    resolve(plan(2));
    await vi.advanceTimersByTimeAsync(0);
    expect(controller.planId).toBe(3);
    expect(remove).toHaveBeenCalledWith('/api/plans/2', expect.anything());
  });

  it('not-ready aborts, drops the plan and clears stats and warnings', async () => {
    const { controller, params, remove } = setup();
    await vi.advanceTimersByTimeAsync(300);
    params.value = { ready: false, reason: 'Choose both bases' };
    expect(controller.status).toBe('idle');
    expect(controller.planId).toBeNull();
    expect(controller.stats).toEqual({});
    expect(controller.warnings).toEqual([]);
    expect(remove).toHaveBeenCalledWith('/api/plans/1', expect.anything());
  });

  it.each(['plan_too_large', 'plan_build_timeout', 'invalid_plan_params'])(
    'maps %s and retries immediately',
    async (code) => {
      const { controller, post } = setup();
      post.mockRejectedValueOnce(new ApiError({ status: 400, code, message: 'Narrow the filter' }));
      await vi.advanceTimersByTimeAsync(300);
      expect(controller.status).toBe('error');
      expect(controller.error).toBe('Narrow the filter');
      controller.retry();
      await vi.advanceTimersByTimeAsync(0);
      expect(controller.status).toBe('ready');
      expect(controller.error).toBeNull();
    },
  );

  it('an abort is not an error, and retry from ready builds now but not twice in flight', async () => {
    const { controller, post } = setup();
    await vi.advanceTimersByTimeAsync(300);
    let resolve: (value: PlanDto) => void = () => {};
    post.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    controller.retry();
    controller.retry();
    expect(post).toHaveBeenCalledTimes(2);
    resolve(plan(2));
    await vi.advanceTimersByTimeAsync(0);
    post.mockRejectedValueOnce(new DOMException('Aborted', 'AbortError'));
    controller.retry();
    await vi.advanceTimersByTimeAsync(0);
    expect(controller.error).toBeNull();
  });

  it('only rebuilds a current stale ref and stops expiration loops until manual retry', async () => {
    const { controller, post } = setup();
    await vi.advanceTimersByTimeAsync(300);
    controller.stale({ plan_id: 9, side: 'source' });
    expect(post).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 4; i++) {
      controller.stale({ plan_id: 1, side: 'source' });
      await vi.advanceTimersByTimeAsync(0);
    }
    expect(post).toHaveBeenCalledTimes(4);
    expect(controller.error).toBe('The plan keeps expiring. Retry.');
    controller.retry();
    await vi.advanceTimersByTimeAsync(0);
    expect(controller.status).toBe('ready');
  });

  it('stale skips a pending parameter debounce and stale counts age out after ten seconds', async () => {
    const { controller, params, post } = setup();
    await vi.advanceTimersByTimeAsync(300);
    params.value = { ready: true, params: { changed: true } };
    controller.stale({ plan_id: 1, side: 'source' });
    await vi.advanceTimersByTimeAsync(0);
    expect(post).toHaveBeenCalledTimes(2);
    for (let i = 0; i < 4; i++) {
      await vi.advanceTimersByTimeAsync(10_000);
      controller.stale({ plan_id: 1, side: 'source' });
      await vi.advanceTimersByTimeAsync(0);
    }
    expect(controller.status).toBe('ready');
  });
});

describe('invalidations', () => {
  it('invalidates destination FolderRef subtrees, not siblings or other volumes', async () => {
    const { controller, post, params } = setup();
    params.value = {
      ready: true,
      params: {
        source: { scope: { volume_id: 1, dir: 'source', recursive: false } },
        destination: { volume_id: 2, dir: 'a/b' },
      },
    };
    await vi.advanceTimersByTimeAsync(300);
    events.emit({ type: 'fs_change', volume_id: 2, dirs: ['a/bc'] });
    events.emit({ type: 'fs_change', volume_id: 3, dirs: ['a/b/child'] });
    await vi.advanceTimersByTimeAsync(1500);
    expect(post).toHaveBeenCalledTimes(1);
    events.emit({ type: 'fs_change', volume_id: 2, dirs: ['a/b/child'] });
    expect(controller.status).toBe('building');
    await vi.advanceTimersByTimeAsync(1500);
    expect(post).toHaveBeenCalledTimes(2);
  });
  it('affects either scope but not a sibling or another volume, with a quiet debounce', async () => {
    const { controller, post } = setup();
    await vi.advanceTimersByTimeAsync(300);
    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['ab'] });
    events.emit({ type: 'fs_change', volume_id: 3, dirs: ['a'] });
    await vi.advanceTimersByTimeAsync(1500);
    expect(post).toHaveBeenCalledTimes(1);
    events.emit({ type: 'fs_change', volume_id: 2, dirs: ['b/deep'] });
    expect(controller.status).toBe('building');
    await vi.advanceTimersByTimeAsync(1000);
    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['a'] });
    await vi.advanceTimersByTimeAsync(1499);
    expect(post).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(post).toHaveBeenCalledTimes(2);
  });

  it('a constant stream of changes cannot postpone a rebuild past five seconds', async () => {
    const { post } = setup();
    await vi.advanceTimersByTimeAsync(300);
    for (let i = 0; i < 5; i++) {
      events.emit({ type: 'fs_change', volume_id: 1, dirs: ['a'] });
      await vi.advanceTimersByTimeAsync(1000);
    }
    expect(post).toHaveBeenCalledTimes(2);
  });

  it('resync rebuilds and does not add work during a pending build', async () => {
    const { post } = setup();
    vi.stubGlobal('EventSource', FakeEventSource);
    events.start();
    FakeEventSource.last().open();
    events.stop();
    events.start();
    FakeEventSource.last().open();
    await vi.advanceTimersByTimeAsync(300);
    expect(post).toHaveBeenCalledTimes(1);
    events.stop();
    events.start();
    FakeEventSource.last().open();
    await vi.advanceTimersByTimeAsync(1500);
    expect(post).toHaveBeenCalledTimes(2);
  });

  it.each(['unhashed_files', 'metadata_pending', 'index_running', 'unrelated'])(
    'index completion rebuilds only for %s when index dependent',
    async (code) => {
      const { post } = setup();
      expect(INDEX_DEPENDENT_WARNINGS).toEqual([
        'unhashed_files',
        'metadata_pending',
        'index_running',
      ]);
      post.mockResolvedValue(plan(1, { warnings: [{ code, count: 2, message: 'Pending' }] }));
      await vi.advanceTimersByTimeAsync(300);
      events.emit(taskEvent({ kind: 'immich_sync', state: 'done' }));
      events.emit(taskEvent({ kind: 'index', state: 'running' }));
      await vi.advanceTimersByTimeAsync(1500);
      expect(post).toHaveBeenCalledTimes(1);
      events.emit(taskEvent({ kind: 'index', state: 'done' }));
      await vi.advanceTimersByTimeAsync(1500);
      expect(post).toHaveBeenCalledTimes(code === 'unrelated' ? 1 : 2);
    },
  );

  it.each(['done', 'failed', 'cancelled', 'interrupted'] as const)(
    'an ended index task (%s) clears index_running only when that warning is present',
    async (state) => {
      const { post, controller } = setup();
      post.mockResolvedValue(
        plan(1, {
          warnings: [{ code: 'index_running', count: 1, message: 'Indexing' }],
        }),
      );
      await vi.advanceTimersByTimeAsync(300);
      events.emit(taskEvent({ kind: 'index', state }));
      await vi.advanceTimersByTimeAsync(1500);
      expect(post).toHaveBeenCalledTimes(2);
      controller.dispose();
      post.mockClear();
      const second = setup();
      second.post.mockResolvedValue(
        plan(2, {
          warnings: [{ code: 'metadata_pending', count: 1, message: 'Metadata pending' }],
        }),
      );
      await vi.advanceTimersByTimeAsync(300);
      events.emit(taskEvent({ kind: 'index', state }));
      await vi.advanceTimersByTimeAsync(1500);
      expect(second.post).toHaveBeenCalledTimes(state === 'done' ? 2 : 1);
    },
  );

  it.each(['done', 'failed', 'cancelled', 'interrupted'] as const)(
    'holds changes during execution and rebuilds once a second after %s',
    async (state) => {
      const { controller, post, params } = setup();
      await vi.advanceTimersByTimeAsync(300);
      controller.executing(8);
      expect(controller.ref('source')).toBeNull();
      params.value = { ready: true, params: { changed: 2 } };
      controller.retry();
      events.emit({ type: 'fs_change', volume_id: 1, dirs: ['a'] });
      await vi.advanceTimersByTimeAsync(6000);
      expect(post).toHaveBeenCalledTimes(1);
      events.emit(taskEvent({ id: 7, state }));
      events.emit(taskEvent({ id: 8, state }));
      params.value = { ready: true, params: { changed: 3 } };
      events.emit(taskEvent({ id: 8, state }));
      await vi.advanceTimersByTimeAsync(999);
      expect(post).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(1);
      expect(post).toHaveBeenCalledTimes(2);
      expect(post.mock.calls[1]?.[1]?.body).toMatchObject({
        params: { changed: 3 },
        replaces: null,
      });
    },
  );

  it('disposes idempotently, cancels work, removes listeners and leaves no timers', async () => {
    const { controller, post, remove } = setup();
    await vi.advanceTimersByTimeAsync(300);
    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['a'] });
    controller.dispose();
    controller.dispose();
    expect(vi.getTimerCount()).toBe(0);
    expect(remove).toHaveBeenCalledTimes(1);
    events.emit({ type: 'fs_change', volume_id: 1, dirs: ['a'] });
    events.emit(taskEvent({ kind: 'index', state: 'done' }));
    controller.retry();
    await vi.advanceTimersByTimeAsync(6000);
    expect(post).toHaveBeenCalledTimes(1);
  });

  it('does not delete the executing plan when the view disconnects', async () => {
    const { controller, remove } = setup();
    await vi.advanceTimersByTimeAsync(300);
    controller.executing(8);
    controller.dispose();
    expect(remove).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('aborts an in-flight build on disposal and cleans up its late answer', async () => {
    const { controller, post, remove } = setup();
    let resolve: (value: PlanDto) => void = () => {};
    post.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    await vi.advanceTimersByTimeAsync(300);
    const signal = post.mock.calls[0]?.[1]?.signal;
    controller.dispose();
    expect(signal?.aborted).toBe(true);
    resolve(plan(99));
    await vi.advanceTimersByTimeAsync(0);
    expect(controller.planId).toBeNull();
    expect(remove).toHaveBeenCalledWith('/api/plans/99', expect.anything());
    expect(vi.getTimerCount()).toBe(0);
  });
});
