import { type Signal, signal } from '@preact/signals-core';
import { html, nothing, type TemplateResult } from 'lit';
import { errorMessage, isAbortError } from '@/api/errors';
import '@/components/opm-icon';
import '@/components/opm-split-button';
import type { SplitActionEvent, SplitOption } from '@/components/opm-split-button';
import { toast } from '@/components/toast';
import { lastActionOf } from '@/stores/prefs';
import { checkToolActions, type ToolAction, type ToolActions } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import { formatCount } from '@/util/format-count';

/**
 * Renders and runs a tool's `actions` (`ToolConfig.actions`) in the bottom bar's far-right area.
 *
 * - No actions: nothing. One action: a primary button (red when `danger`). Several: the split
 *   button, which remembers the user's choice per tool (`lastAction`, keyed by the tool id).
 * - `count` is the badge on the button. A disabled action shows its reason as a tooltip.
 * - A click runs the action (`run` owns its confirmation) with the bar busy until it settles. A
 *   rejection becomes one error toast, except an abort, which is silent. A second click while
 *   busy is ignored.
 */

const busyByContext = new WeakMap<ToolContext, Signal<boolean>>();
const reportedProblem = new WeakMap<ToolContext, string>();

function busyOf(ctx: ToolContext): Signal<boolean> {
  let busy = busyByContext.get(ctx);
  if (!busy) {
    busy = signal(false);
    busyByContext.set(ctx, busy);
  }
  return busy;
}

/**
 * Why the action cannot run now, `null` when it can: the ONE place that decides. Reads signals
 * (through the action's own `disabled`), so it belongs in a render.
 */
export function actionReason(ctx: ToolContext, action: ToolAction): string | null {
  if (ctx.config.plan && ctx.plan.status !== 'ready') {
    switch (ctx.plan.status) {
      case 'building':
        return 'Matching...';
      case 'executing':
        return 'Working...';
      case 'error':
        return ctx.plan.error ?? 'The plan could not be built.';
      default: {
        const params = ctx.config.plan.params(ctx);
        return (!params.ready && params.reason) || 'The plan is not ready.';
      }
    }
  }
  return action.disabled?.(ctx) ?? null;
}

/** The tooltip of an action: the reason while it is disabled, its static hint otherwise. */
function tipOf(reason: string | null, action: ToolAction): string | undefined {
  return (reason === null ? action.hint : reason) || undefined;
}

async function runAction(ctx: ToolContext, action: ToolAction): Promise<void> {
  const busy = busyOf(ctx);
  if (busy.peek() || actionReason(ctx, action) !== null) return;
  busy.value = true;
  try {
    await action.run(ctx);
  } catch (error) {
    if (!isAbortError(error)) toast.error(errorMessage(error));
  } finally {
    busy.value = false;
  }
}

/** Logs a configuration mistake once per context instead of on every render. */
function reportOnce(ctx: ToolContext, problem: string): void {
  if (reportedProblem.get(ctx) === problem) return;
  reportedProblem.set(ctx, problem);
  console.error(`Tool "${ctx.tool}": ${problem}`);
}

function renderCount(count: number | undefined) {
  if (typeof count !== 'number' || !Number.isFinite(count)) return nothing;
  return html`<span class="badge badge-neutral">${formatCount(count)}</span>`;
}

/** The tooltip lives on a wrapper: a disabled button receives no pointer events. */
function renderTip(tip: string | undefined, hintId: string, content: TemplateResult) {
  return html`
    <div
      class="inline-flex ${tip ? 'tooltip tooltip-left' : ''}"
      data-tip=${tip ?? nothing}
    >
      ${content}
      ${tip ? html`<span id=${hintId} class="sr-only">${tip}</span>` : nothing}
    </div>
  `;
}

function renderSingle(
  ctx: ToolContext,
  action: ToolAction,
  count: number | undefined,
  busy: boolean,
): TemplateResult {
  const reason = actionReason(ctx, action);
  const tip = tipOf(reason, action);
  const hintId = `${ctx.tool}-action-hint`;
  const button = html`
    <button
      type="button"
      class="btn btn-sm ${action.danger ? 'btn-error' : 'btn-primary'}"
      ?disabled=${busy || reason !== null}
      aria-busy=${busy ? 'true' : nothing}
      aria-describedby=${tip ? hintId : nothing}
      @click=${() => void runAction(ctx, action)}
    >
      ${
        busy
          ? html`<span class="loading loading-spinner" aria-hidden="true"></span>`
          : action.icon
            ? html`<opm-icon name=${action.icon} size="sm"></opm-icon>`
            : nothing
      }
      <span>${action.label}</span>
      ${renderCount(count)}
    </button>
  `;
  return renderTip(tip, hintId, button);
}

/**
 * The split button disables itself when no entry is enabled, and then has no menu to carry the
 * reasons: the tooltip shows the reason of the entry it displays (the remembered one, else the
 * default, else the first).
 */
function shownReason(
  ctx: ToolContext,
  actions: ToolActions,
  reasons: (string | null)[],
): string | undefined {
  if (reasons.some((reason) => reason === null)) return undefined;
  const ids = actions.options.map((action) => action.id);
  const remembered = ids.indexOf(lastActionOf(ctx.tool) ?? '');
  const index = remembered >= 0 ? remembered : Math.max(ids.indexOf(actions.defaultId), 0);
  return reasons[index] || undefined;
}

function renderSplit(
  ctx: ToolContext,
  actions: ToolActions,
  count: number | undefined,
  busy: boolean,
): TemplateResult {
  const reasons = actions.options.map((action) => actionReason(ctx, action));
  const options: SplitOption[] = actions.options.map((action, index) => ({
    id: action.id,
    label: action.label,
    icon: action.icon,
    danger: action.danger,
    disabled: reasons[index] !== null,
    hint: tipOf(reasons[index] ?? null, action),
  }));
  const run = (event: SplitActionEvent) => {
    const action = actions.options.find((candidate) => candidate.id === event.detail.optionId);
    if (action) void runAction(ctx, action);
  };
  const hintId = `${ctx.tool}-action-hint`;
  const tip = shownReason(ctx, actions, reasons);
  const split = html`
    <opm-split-button
      persist-key=${ctx.tool}
      default-id=${actions.defaultId}
      .options=${options}
      .count=${count}
      .busy=${busy}
      @split-action=${run}
    ></opm-split-button>
  `;
  return renderTip(tip, hintId, split);
}

/**
 * The template for `<opm-bottom-bar .actions>`, `undefined` when the tool has no actions (or the
 * actions are misconfigured: `checkToolActions` logs it once and nothing is rendered). Call it in
 * the view's render: what it reads from signals re-renders the view.
 */
export function renderActions(ctx: ToolContext): TemplateResult | undefined {
  const actions = ctx.config.actions?.(ctx) ?? null;
  if (actions === null || actions.options.length === 0) return undefined;
  const problem = checkToolActions(actions);
  if (problem !== null) {
    reportOnce(ctx, problem);
    return undefined;
  }
  const busy = busyOf(ctx).value;
  const count = actions.count?.(ctx);
  const [only, ...others] = actions.options;
  return only && others.length === 0
    ? renderSingle(ctx, only, count, busy)
    : renderSplit(ctx, actions, count, busy);
}
