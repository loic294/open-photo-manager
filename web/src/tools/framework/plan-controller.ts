import { batch, computed, effect, signal, untracked } from '@preact/signals-core';
import { errorMessage, isAbortError, isApiError } from '@/api/errors';
import type { PlanRef } from '@/api/generated/PlanRef';
import type { PlanSide } from '@/api/generated/PlanSide';
import type { Scope } from '@/api/generated/Scope';
import { createPlan, deletePlan } from '@/api/plans';
import { events } from '@/api/sse';
import { isEnded, onTaskFinished, tasks } from '@/stores/tasks';
import type { PlanBinding, PlanStatus, PlanWarningView } from '@/tools/framework/plan-binding';
import type { PlanParamsResult } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import { fsChangeAffects } from '@/util/fs-change';

export const PLAN_DEBOUNCE_MS = 300;
export const PLAN_CHANGE_QUIET_MS = 1_500;
export const PLAN_CHANGE_MAX_WAIT_MS = 5_000;
export const PLAN_AFTER_RUN_MS = 1_000;
export const PLAN_STALE_WINDOW_MS = 10_000;
export const PLAN_STALE_LIMIT = 3;
export const INDEX_DEPENDENT_WARNINGS = [
  'unhashed_files',
  'metadata_pending',
  'index_running',
] as const;

/** Stable JSON: object key order is irrelevant; array order and exact path bytes are not. */
export function paramsKey(params: Record<string, unknown>): string {
  return JSON.stringify(params, (_key, value: unknown) =>
    value !== null && typeof value === 'object' && !Array.isArray(value)
      ? Object.fromEntries(Object.entries(value).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
      : value,
  );
}

/** Only concrete Scope objects inside the planner's parameters are invalidation scopes. */
function scopesIn(value: unknown): Scope[] {
  if (value === null || typeof value !== 'object') return [];
  if ('volume_id' in value && 'dir' in value && 'recursive' in value) {
    if (
      (value.volume_id === null || typeof value.volume_id === 'number') &&
      typeof value.dir === 'string' &&
      typeof value.recursive === 'boolean'
    ) {
      return [{ volume_id: value.volume_id, dir: value.dir, recursive: value.recursive }];
    }
  }
  return Object.values(value).flatMap(scopesIn);
}

/** Best effort only for cleanup; unexpected failures remain visible in the diagnostic log. */
function dropPlan(id: number | null): void {
  if (id === null) return;
  void deletePlan(id).catch((error: unknown) => {
    if (isApiError(error) && (error.status === 404 || error.status === 409)) return;
    if (!isAbortError(error)) console.warn('[plans] could not delete preview', id, error);
  });
}

export class PlanController implements PlanBinding {
  private readonly statusValue = signal<PlanStatus>('idle');
  private readonly errorValue = signal<string | null>(null);
  private readonly idValue = signal<number | null>(null);
  private readonly statsValue = signal<Readonly<Record<string, number>>>({});
  private readonly warningsValue = signal<readonly PlanWarningView[]>([]);
  private readonly currentKey = signal<string | null>(null);
  private readonly shownKey = signal<string | null>(null);
  readonly ready = computed(
    () => this.statusValue.value === 'ready' && this.shownKey.value === this.currentKey.value,
  );

  private input: PlanParamsResult = { ready: false, reason: null };
  private generation = 0;
  private request: AbortController | null = null;
  private buildTimer: number | undefined;
  private quietTimer: number | undefined;
  private maxTimer: number | undefined;
  private afterRunTimer: number | undefined;
  private taskId: number | null = null;
  private disposed = false;
  private staleTimes: number[] = [];
  private readonly stops: (() => void)[] = [];

  constructor(private readonly ctx: ToolContext) {
    this.stops.push(
      events.on('fs_change', (event) => {
        if (
          this.input.ready &&
          scopesIn(this.input.params).some((scope) =>
            fsChangeAffects(scope, event, { subtree: true }),
          )
        ) {
          this.invalidate();
        }
      }),
      events.onResync(() => this.invalidate()),
      events.on('task', ({ task }) => {
        this.finished(task);
        if (
          task.kind === 'index' &&
          isEnded(task) &&
          this.warnings.some(
            (warning) =>
              warning.code === 'index_running' ||
              (task.state === 'done' &&
                INDEX_DEPENDENT_WARNINGS.some((code) => code === warning.code)),
          )
        ) {
          this.invalidate();
        }
      }),
      // The task store also reports terminal snapshots after reconnects.
      onTaskFinished((task) => this.finished(task)),
      effect(() => {
        try {
          const input = ctx.config.plan?.params(ctx) ?? { ready: false, reason: null };
          const key = input.ready ? paramsKey(input.params) : null;
          untracked(() => this.changed(input, key));
        } catch (error) {
          untracked(() => {
            this.cancelBuild();
            this.input = { ready: false, reason: null };
            this.currentKey.value = null;
            this.setError(errorMessage(error, 'The plan parameters could not be read.'));
          });
        }
      }),
    );
  }

  get status(): PlanStatus {
    return this.statusValue.value;
  }
  get error(): string | null {
    return this.errorValue.value;
  }
  get planId(): number | null {
    return this.idValue.value;
  }
  get stats(): Readonly<Record<string, number>> {
    return this.statsValue.value;
  }
  get warnings(): readonly PlanWarningView[] {
    return this.warningsValue.value;
  }

  ref(side: PlanSide): PlanRef | null {
    const id = this.planId;
    return id === null || this.status === 'executing' ? null : { plan_id: id, side };
  }

  private changed(input: PlanParamsResult, key: string | null): void {
    this.input = input;
    if (key === this.currentKey.peek() && (input.ready || this.idValue.peek() === null)) return;
    this.currentKey.value = key;
    if (this.taskId !== null || this.afterRunTimer !== undefined) return;
    this.cancelBuild();
    this.clearInvalidation();
    if (!input.ready) {
      const old = this.idValue.peek();
      batch(() => {
        this.idValue.value = null;
        this.shownKey.value = null;
        this.statsValue.value = {};
        this.warningsValue.value = [];
        this.errorValue.value = null;
        this.statusValue.value = 'idle';
      });
      dropPlan(old);
      return;
    }
    this.statusValue.value = 'building';
    this.errorValue.value = null;
    this.buildTimer = window.setTimeout(() => {
      this.buildTimer = undefined;
      void this.build();
    }, PLAN_DEBOUNCE_MS);
  }

  private cancelBuild(): void {
    this.generation += 1;
    this.request?.abort();
    this.request = null;
    window.clearTimeout(this.buildTimer);
    this.buildTimer = undefined;
  }

  private clearInvalidation(): void {
    window.clearTimeout(this.quietTimer);
    window.clearTimeout(this.maxTimer);
    this.quietTimer = undefined;
    this.maxTimer = undefined;
  }

  private invalidate(): void {
    if (
      this.disposed ||
      !this.input.ready ||
      this.taskId !== null ||
      this.afterRunTimer !== undefined ||
      this.request !== null ||
      this.buildTimer !== undefined
    )
      return;
    this.statusValue.value = 'building';
    window.clearTimeout(this.quietTimer);
    const rebuild = () => {
      this.clearInvalidation();
      void this.build();
    };
    this.quietTimer = window.setTimeout(rebuild, PLAN_CHANGE_QUIET_MS);
    this.maxTimer ??= window.setTimeout(rebuild, PLAN_CHANGE_MAX_WAIT_MS);
  }

  private async build(): Promise<void> {
    if (this.disposed || !this.input.ready || this.request || this.taskId !== null) return;
    const spec = this.ctx.config.plan;
    if (!spec) return;
    const key = this.currentKey.peek();
    const generation = ++this.generation;
    const request = new AbortController();
    this.request = request;
    batch(() => {
      this.statusValue.value = 'building';
      this.errorValue.value = null;
    });
    try {
      const plan = await createPlan(spec.kind, this.input.params, {
        replaces: this.idValue.peek(),
        signal: request.signal,
      });
      if (
        this.disposed ||
        request.signal.aborted ||
        generation !== this.generation ||
        key !== this.currentKey.peek()
      ) {
        dropPlan(plan.id);
        return;
      }
      batch(() => {
        this.idValue.value = plan.id;
        this.shownKey.value = key;
        this.statsValue.value = plan.stats;
        this.warningsValue.value = plan.warnings;
        this.statusValue.value = 'ready';
      });
    } catch (error) {
      if (!request.signal.aborted && generation === this.generation && !isAbortError(error)) {
        this.setError(errorMessage(error, 'The plan could not be built.'));
      }
    } finally {
      if (this.request === request) this.request = null;
    }
  }

  private setError(message: string): void {
    batch(() => {
      this.errorValue.value = message;
      this.statusValue.value = 'error';
    });
  }

  retry(): void {
    if (this.disposed || this.request || this.taskId !== null || this.afterRunTimer !== undefined)
      return;
    window.clearTimeout(this.buildTimer);
    this.buildTimer = undefined;
    this.clearInvalidation();
    this.staleTimes = [];
    void this.build();
  }

  stale(ref: PlanRef): void {
    if (
      this.disposed ||
      ref.plan_id !== this.idValue.peek() ||
      this.taskId !== null ||
      this.request ||
      this.afterRunTimer !== undefined
    )
      return;
    window.clearTimeout(this.buildTimer);
    this.buildTimer = undefined;
    const now = Date.now();
    this.staleTimes = this.staleTimes.filter((time) => now - time < PLAN_STALE_WINDOW_MS);
    this.staleTimes.push(now);
    this.clearInvalidation();
    if (this.staleTimes.length > PLAN_STALE_LIMIT) {
      this.setError('The plan keeps expiring. Retry.');
      return;
    }
    void this.build();
  }

  executing(taskId: number): void {
    if (this.disposed) return;
    this.cancelBuild();
    this.clearInvalidation();
    this.taskId = taskId;
    this.statusValue.value = 'executing';
    const task = tasks.peek().find((item) => item.id === taskId);
    if (task) this.finished(task);
  }

  private finished(task: Parameters<typeof isEnded>[0]): void {
    if (this.disposed || task.id !== this.taskId || !isEnded(task)) return;
    this.taskId = null;
    // Executed plans have no overlay and may never be executed again.
    batch(() => {
      this.idValue.value = null;
      this.shownKey.value = null;
    });
    this.afterRunTimer = window.setTimeout(() => {
      this.afterRunTimer = undefined;
      if (this.input.ready) void this.build();
      else {
        batch(() => {
          this.statsValue.value = {};
          this.warningsValue.value = [];
          this.statusValue.value = 'idle';
        });
      }
    }, PLAN_AFTER_RUN_MS);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.cancelBuild();
    this.clearInvalidation();
    window.clearTimeout(this.afterRunTimer);
    this.afterRunTimer = undefined;
    for (const stop of this.stops.splice(0)) stop();
    if (this.taskId === null) dropPlan(this.idValue.peek());
  }
}
