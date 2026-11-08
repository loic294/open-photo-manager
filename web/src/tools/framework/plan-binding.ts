import type { PlanRef } from '@/api/generated/PlanRef';
import type { PlanSide } from '@/api/generated/PlanSide';
import { PlanController } from '@/tools/framework/plan-controller';
import type { ToolContext } from '@/tools/framework/tool-context';

/**
 * The seam between the tool view and the plan client: what a tool that builds a plan shows and
 * how the panes get the plan. The view, the panes and the actions only ever see `PlanBinding`;
 * the plan client implements it behind `createPlanBinding`.
 */

/** A warning of the plan: the structural twin of the server's `PlanWarning`. */
export interface PlanWarningView {
  code: string;
  message: string;
  count: number;
}

/**
 * - `none`: the tool has no plan (`NO_PLAN`).
 * - `idle`: a plan tool with no plan yet (its parameters are not ready, or nothing was asked).
 * - `building`: the server is matching, `ready`: `planId` is the plan the panes show,
 *   `executing`: a run of that plan is in progress, `error`: the last build failed (`error`).
 */
export type PlanStatus = 'none' | 'idle' | 'building' | 'ready' | 'executing' | 'error';

/**
 * Keep the state in signals (or read signals in the getters): the view renders from it, so a
 * change then re-renders the status area, the panes and the actions by itself.
 */
export interface PlanBinding {
  readonly status: PlanStatus;
  readonly error: string | null;
  /** The preview the panes show (kept while rebuilding), else `null`. */
  readonly planId: number | null;
  /** The plan's statistics, `{}` when there is none. */
  readonly stats: Readonly<Record<string, number>>;
  readonly warnings: readonly PlanWarningView[];
  /** What a pane passes as `BrowseRequest.plan`: `null` with no preview or during execution. */
  ref(side: PlanSide): PlanRef | null;
  /** A pane found its plan expired (`plan-stale`): the client builds a new one. */
  stale(ref: PlanRef): void;
  /** Builds again now (the status area's Retry button). */
  retry(): void;
  /** The execution was queued; the binding holds rebuilding until that task ends. */
  executing?(taskId: number): void;
  /** Called once when the view disconnects or gets another config: stop all work. Idempotent. */
  dispose?(): void;
}

/** The binding of a tool without a plan: every field is empty and `ref` is always `null`. */
export const NO_PLAN: PlanBinding = Object.freeze<PlanBinding>({
  status: 'none',
  error: null,
  planId: null,
  stats: Object.freeze({}),
  warnings: Object.freeze([]),
  ref: () => null,
  stale: () => undefined,
  retry: () => undefined,
});

/**
 * The binding of this context's tool. Called once, last in the context's constructor, so `ctx`
 * is complete (its getters can be read at once). The plan client replaces ONLY this body: it
 * returns `NO_PLAN` for a tool without `ctx.config.plan`.
 */
export function createPlanBinding(ctx: ToolContext): PlanBinding {
  return ctx.config.plan ? new PlanController(ctx) : NO_PLAN;
}
