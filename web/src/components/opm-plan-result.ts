import { html, nothing } from 'lit';
import { OpmElement } from '@/base/opm-element';
import { tooltip } from '@/components/tooltip';
import '@/components/opm-modal';
import type { PlanRun } from '@/stores/plan-runs';
import { describeFailure } from '@/util/failure-codes';
import { formatFileSize } from '@/util/format';
import { formatCount } from '@/util/format-count';

let host: OpmPlanResult | undefined;

/** One reusable result dialog. Only an explicit Details action opens it. */
export function openPlanResult(run: PlanRun): void {
  if (!host?.isConnected) {
    host = document.createElement('opm-plan-result');
    document.body.append(host);
  }
  host.run = run;
  host.open = true;
}

export class OpmPlanResult extends OpmElement {
  static override properties = { run: { attribute: false }, open: { type: Boolean } };
  declare run: PlanRun | undefined;
  open = false;

  protected override render() {
    const result = this.run?.result;
    return html`
      <opm-modal size="lg" .open=${this.open}
        .heading=${this.run?.title ?? 'Execution result'}
        .body=${result ? this.renderResult(result) : html`<p>The result is not available.</p>`}
        .actions=${html`<button type="button" class="btn" @click=${() => {
          this.open = false;
        }}>Close</button>`}
        @close=${() => {
          if (!this.querySelector('opm-modal')?.open) this.open = false;
        }}
      ></opm-modal>
    `;
  }

  private renderResult(result: NonNullable<PlanRun['result']>) {
    const groups = new Map<string, number>();
    for (const failure of result.failures) {
      const reason = describeFailure(failure.code, failure.message);
      groups.set(reason, (groups.get(reason) ?? 0) + 1);
    }
    const total = result.failed + result.skipped;
    return html`
      <div class="flex min-w-0 flex-col gap-4">
        ${
          result.note || result.cancelled
            ? html`<div class="alert alert-warning" role="status">
          <span>${result.note ?? 'Execution was cancelled before all items were processed.'}</span>
        </div>`
            : nothing
        }
        <div class="stats stats-vertical w-full sm:stats-horizontal">
          ${[
            ['Done', formatCount(result.done)],
            ['Skipped', formatCount(result.skipped)],
            ['Failed', formatCount(result.failed)],
            ['Bytes', formatFileSize(result.bytes)],
          ].map(
            ([label, value]) => html`<div class="stat min-w-0 px-3">
            <div class="stat-title">${label}</div>
            <div class="stat-value text-xl">${value}</div>
          </div>`,
          )}
        </div>
        ${
          groups.size > 0
            ? html`<div>
          <h4 class="font-semibold">Reasons in the displayed items</h4>
          <ul class="mt-2 flex flex-col gap-1 text-sm">
            ${[...groups].map(
              ([reason, count]) => html`<li class="flex justify-between gap-4">
              <span>${reason}</span><span class="badge badge-neutral badge-sm">${formatCount(count)}</span>
            </li>`,
            )}
          </ul>
        </div>`
            : nothing
        }
        ${
          total > result.failures.length
            ? html`<p class="text-sm text-base-content/70">
          Showing the first ${formatCount(result.failures.length)} of ${formatCount(total)}
        </p>`
            : nothing
        }
        ${
          result.failures.length > 0
            ? html`<div class="max-h-96 overflow-y-auto">
          <table class="table table-sm table-zebra w-full">
            <thead><tr><th scope="col">Location</th><th scope="col">Reason</th></tr></thead>
            <tbody>${result.failures.map(
              (failure) => html`<tr>
              <td class="break-all">${failure.path}</td>
              <td class="whitespace-normal" ${tooltip(failure.message)}>
                ${describeFailure(failure.code, failure.message)}
              </td>
            </tr>`,
            )}</tbody>
          </table>
        </div>`
            : html`<p class="text-sm">No item failures were reported.</p>`
        }
      </div>
    `;
  }
}

customElements.define('opm-plan-result', OpmPlanResult);

declare global {
  interface HTMLElementTagNameMap {
    'opm-plan-result': OpmPlanResult;
  }
}
