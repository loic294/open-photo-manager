import { type ReadonlySignal, type Signal, signal } from '@preact/signals-core';
import { errorMessage } from '@/api/errors';
import type { PlanDto } from '@/api/generated/PlanDto';
import type { PlanResult } from '@/api/generated/PlanResult';
import type { TaskDto } from '@/api/generated/TaskDto';
import { getPlan } from '@/api/plans';
import { events } from '@/api/sse';
import { listTasks } from '@/api/tasks';
import { openPlanResult } from '@/components/apm-plan-result';
import { toast } from '@/components/toast';
import type { ToolId } from '@/stores/nav';
import { isEnded, onTaskFinished, tasks } from '@/stores/tasks';

export interface PlanRun {
  tool: ToolId;
  planId: number;
  taskId: number;
  title: string;
  result?: PlanResult | null;
}

const active = new Map<number, PlanRun>();
const latest = new Map<ToolId, number>();
const results = new Map<ToolId, Signal<PlanRun | null>>();
let stops: (() => void)[] = [];
let epoch = 0;

function resultSignal(tool: ToolId) {
  let value = results.get(tool);
  if (!value) {
    value = signal<PlanRun | null>(null);
    results.set(tool, value);
  }
  return value;
}

export function lastRun(tool: ToolId): ReadonlySignal<PlanRun | null> {
  return resultSignal(tool);
}

export function dismissLastRun(tool: ToolId): void {
  resultSignal(tool).value = null;
}

async function finish(task: TaskDto): Promise<void> {
  const run = active.get(task.id);
  if (!run || !isEnded(task)) return;
  active.delete(task.id);
  const version = epoch;
  const completed: PlanRun = { ...run, title: task.title };
  try {
    let plan: PlanDto;
    try {
      plan = await getPlan(run.planId);
      if (!plan.result) throw new Error('The execution result is not available.');
    } catch {
      // The terminal task can arrive just before a proxy makes the result readable.
      if (version !== epoch) return;
      plan = await getPlan(run.planId);
    }
    if (version !== epoch) return;
    if (!plan.result) throw new Error('The execution result is not available.');
    completed.result = plan.result;
  } catch (error) {
    if (version !== epoch) return;
    toast.error(errorMessage(error, 'Could not load the execution result.'));
    if (task.state === 'done') return;
    completed.result = null;
  }
  if (version !== epoch) return;
  if (latest.get(run.tool) === run.taskId) resultSignal(run.tool).value = completed;
  switch (task.state) {
    case 'done': {
      const result = completed.result;
      if (result && (result.failed > 0 || result.skipped > 0 || result.cancelled || result.note)) {
        toast.warning(task.title, {
          action: { label: 'Details', run: () => openPlanResult(completed) },
        });
      } else toast.success(task.title);
      break;
    }
    case 'failed':
      toast.error(task.error || task.title);
      break;
    case 'cancelled':
      toast.info(task.title);
      break;
    case 'interrupted':
      toast.warning(task.title);
      break;
  }
}

async function reconcile(): Promise<void> {
  if (active.size === 0) return;
  const version = epoch;
  try {
    const snapshot = await listTasks();
    if (version !== epoch) return;
    for (const task of snapshot) void finish(task);
  } catch (error) {
    if (version === epoch) console.warn('[plans] could not reconcile executions', error);
  }
}

/** App-lifetime listeners, installed lazily. Runs and results survive leaving the tool view. */
export function trackPlanRun(run: PlanRun): void {
  if (stops.length === 0) {
    stops = [
      events.on('task', ({ task }) => {
        void finish(task);
      }),
      events.onResync(() => {
        void reconcile();
      }),
      onTaskFinished((task) => {
        void finish(task);
      }),
    ];
  }
  active.set(run.taskId, { ...run });
  latest.set(run.tool, run.taskId);
  dismissLastRun(run.tool);
  // A very small run may have ended before the execute response reached the view.
  const task = tasks.peek().find((item) => item.id === run.taskId);
  if (task) void finish(task);
  else void reconcile();
}

export function resetPlanRunsForTests(): void {
  epoch += 1;
  for (const stop of stops.splice(0)) stop();
  active.clear();
  latest.clear();
  for (const result of results.values()) result.value = null;
}
