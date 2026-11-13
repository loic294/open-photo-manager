import { html, nothing, type TemplateResult } from 'lit';
import { tooltip } from '@/components/tooltip';
import '@/components/opm-icon';
import { openPlanResult } from '@/components/opm-plan-result';
import { dismissLastRun, lastRun } from '@/stores/plan-runs';
import type { ToolContext } from '@/tools/framework/tool-context';
import { formatCount } from '@/util/format-count';

/**
 * The tool bar's status area for the plan (`renderPlanStatus`): what the matching is doing, why a
 * tool has nothing to match yet and the plan's warnings. It sits at the right end of the row that
 * holds the summary, a row of a fixed height, so that the status changing never moves the panes
 * (only the error alert is taller than the row). It always renders its live region, so a screen
 * reader hears each change.
 *
 * - `building`: a spinner and "Matching...", `executing`: a spinner and "Working...".
 * - `error`: an inline alert with the message and a Retry button (`ctx.plan.retry()`).
 * - Nothing to match yet (status `none` or `idle` on a tool that has a plan): the reason from
 *   `config.plan.params(ctx)` as quiet text, never an error.
 * - Warnings: a compact "N warnings" dropdown listing each message with its count.
 */

/** DaisyUI's focus dropdown has no Escape: take the focus off the trigger, which closes it. */
function closeOnEscape(event: KeyboardEvent): void {
  if (event.key !== 'Escape') return;
  const active = document.activeElement;
  const area = event.currentTarget;
  if (active instanceof HTMLElement && area instanceof Node && area.contains(active)) {
    event.preventDefault();
    event.stopPropagation();
    active.blur();
  }
}

/** Why the tool has no plan to build yet, `null` when it is building, has one or needs nothing. */
function notReadyReason(ctx: ToolContext): string | null {
  const spec = ctx.config.plan;
  const { status } = ctx.plan;
  if (!spec || (status !== 'none' && status !== 'idle')) return null;
  const result = spec.params(ctx);
  return result.ready ? null : result.reason;
}

function renderProgress(ctx: ToolContext): TemplateResult | null {
  switch (ctx.plan.status) {
    case 'building':
    case 'executing':
      return html`<span class="loading loading-spinner loading-xs shrink-0" aria-hidden="true"></span>
        <span class="truncate">${ctx.plan.status === 'building' ? 'Matching...' : 'Working...'}</span>`;
    default: {
      const reason = notReadyReason(ctx);
      return reason === null
        ? null
        : html`<span class="truncate" ${tooltip(reason)}>${reason}</span>`;
    }
  }
}

function renderError(ctx: ToolContext) {
  if (ctx.plan.status !== 'error') return nothing;
  return html`
    <div role="alert" class="alert alert-error min-w-0 py-1 text-sm">
      <opm-icon name="circle-alert" size="sm"></opm-icon>
      <span class="min-w-0 break-words">${ctx.plan.error ?? 'The plan could not be built.'}</span>
      <button type="button" class="btn btn-xs" @click=${() => ctx.plan.retry()}>Retry</button>
    </div>
  `;
}

function renderWarnings(ctx: ToolContext) {
  const { warnings } = ctx.plan;
  if (warnings.length === 0) return nothing;
  return html`
    <div class="dropdown dropdown-end" @keydown=${closeOnEscape}>
      <div tabindex="0" role="button" class="btn btn-ghost btn-xs gap-1 text-base-content">
        <opm-icon name="triangle-alert" size="sm" class="text-warning"></opm-icon>
        <span>${warnings.length === 1 ? '1 warning' : `${warnings.length} warnings`}</span>
      </div>
      <ul
        tabindex="0"
        class="dropdown-content z-10 mt-1 w-72 max-w-[calc(100vw-1rem)] space-y-2 rounded-box border border-base-300 bg-base-100 p-3 text-sm shadow-lg"
      >
        ${warnings.map(
          (warning) => html`
            <li class="flex items-start gap-2">
              <span class="badge badge-warning badge-sm mt-0.5 shrink-0">${warning.count}</span>
              <span class="min-w-0 break-words">${warning.message}</span>
            </li>
          `,
        )}
      </ul>
    </div>
  `;
}

export function renderPlanStatus(ctx: ToolContext): TemplateResult {
  const run = lastRun(ctx.tool).value;
  const result = run?.result;
  return html`
    <div class="flex min-w-0 max-w-full items-center justify-end gap-x-3">
      <span role="status" class="flex min-w-0 items-center gap-2 text-sm text-base-content/70">
        ${renderProgress(ctx)}
      </span>
      ${renderError(ctx)} ${renderWarnings(ctx)}
      ${
        run && result && (result.failed > 0 || result.skipped > 0)
          ? html`
        <div class="flex min-w-0 items-center">
          <button type="button" class="btn btn-ghost btn-xs min-w-0"
            @click=${() => openPlanResult(run)}>
            <span class="truncate">Last run: ${formatCount(result.failed)} failed, ${formatCount(result.skipped)} skipped</span>
          </button>
          <button type="button" class="btn btn-ghost btn-xs btn-circle"
            aria-label="Dismiss last run" @click=${() => dismissLastRun(ctx.tool)}>
            <opm-icon name="x" size="xs"></opm-icon>
          </button>
        </div>`
          : nothing
      }
    </div>
  `;
}
