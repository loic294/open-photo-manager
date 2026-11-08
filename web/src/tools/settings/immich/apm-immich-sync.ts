import { html, nothing } from 'lit';
import { requestImmichSync } from '@/api/immich';
import { ApmElement } from '@/base/apm-element';
import { immichStatus } from '@/stores/immich';
import { isActive, loadTasks, taskFraction, tasks } from '@/stores/tasks';
import { describeImmichError } from '@/tools/settings/immich/immich-errors';
import { relativeTime } from '@/tools/trash/trash-text';

export class ApmImmichSync extends ApmElement {
  static override properties = { submitting: { state: true }, failure: { state: true } };
  private submitting = false;
  private failure: string | null = null;

  private async sync(full: boolean): Promise<void> {
    if (
      this.submitting ||
      immichStatus.peek()?.needs_reconnect ||
      tasks.peek().some((task) => task.kind === 'immich_sync' && isActive(task))
    )
      return;
    this.submitting = true;
    this.failure = null;
    try {
      await requestImmichSync({ full });
      await loadTasks();
    } catch (error) {
      this.failure = describeImmichError(error).message;
    } finally {
      this.submitting = false;
    }
  }

  protected override render() {
    const status = immichStatus.value;
    const summary = status?.sync;
    const active = tasks.value.find((task) => task.kind === 'immich_sync' && isActive(task));
    const fraction = active ? taskFraction(active) : null;
    const disabled = this.submitting || !!active || !!status?.needs_reconnect;
    return html`<section class="card border border-base-300 bg-base-200">
      <div class="card-body gap-4">
        <h2 class="card-title">Sync</h2>
        <p>${status?.last_sync_at == null ? 'Not synced yet' : `Last synced ${relativeTime(status.last_sync_at)}`}</p>
        ${
          summary
            ? html`<dl class="grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
          <dt>Assets in Immich (total / live)</dt><dd>${summary.assets_total} / ${summary.assets_live}</dd>
          <dt>Files matched</dt><dd>${summary.assets_matched}</dd>
          <dt>Assets without a matching file</dt><dd>${Math.max(0, summary.assets_total - summary.assets_matched)}</dd>
          <dt>Last full sync</dt><dd>${summary.last_full_at === null ? 'Not synced yet' : relativeTime(summary.last_full_at)}</dd>
        </dl>`
            : nothing
        }
        ${
          summary && summary.assets_live > 0 && summary.assets_matched === 0
            ? html`<div class="alert alert-warning" role="alert">
          No Immich asset matched a file of this app: the path prefix of a volume is probably wrong. Use Auto-detect in the table above.
        </div>`
            : nothing
        }
        ${summary?.last_error ? html`<div class="alert alert-error" role="alert">${describeImmichError(summary.last_error).message}</div>` : nothing}
        ${this.failure ? html`<div class="alert alert-error" role="alert">${this.failure}</div>` : nothing}
        ${
          summary?.scopes.length
            ? html`<details class="collapse collapse-arrow border border-base-300">
          <summary class="collapse-title font-medium">Library sync details</summary>
          <div class="collapse-content overflow-x-auto"><table class="table table-sm">
            <thead><tr><th>Library or prefix</th><th>State</th><th>Total / live / matched / unmapped</th><th>Last sync</th><th>Error</th></tr></thead>
            <tbody>${summary.scopes.map(
              (scope) => html`<tr>
              <th>${scope.library_name || scope.path_prefix || scope.scope}</th>
              <td>${scope.last_error ? 'Failed' : scope.last_sync_at === null ? 'Not synced yet' : 'Synced'}</td>
              <td>${scope.assets_total} / ${scope.assets_live} / ${scope.assets_matched} / ${scope.assets_unmapped}</td>
              <td>${scope.last_sync_at === null ? 'Never' : relativeTime(scope.last_sync_at)}</td>
              <td>${scope.last_error ? describeImmichError(scope.last_error).message : 'None'}</td>
            </tr>`,
            )}</tbody>
          </table></div>
        </details>`
            : nothing
        }
        ${
          active
            ? html`<div role="status"><p>${active.title}</p>
          <progress class="progress progress-primary w-full" max="1" value=${fraction ?? nothing} aria-label=${active.title}></progress>
        </div>`
            : nothing
        }
        ${status?.needs_reconnect ? html`<p>Connect again first</p>` : nothing}
        <div class="flex flex-wrap gap-2">
          <button class="btn btn-primary btn-sm" ?disabled=${disabled} @click=${() => this.sync(false)}>Sync now</button>
          <button class="btn btn-sm" ?disabled=${disabled} @click=${() => this.sync(true)}>Full resync</button>
        </div>
        <p>Full resync reads everything again and takes longer. Both run as background tasks without blocking the app.</p>
      </div>
    </section>`;
  }
}

customElements.define('apm-immich-sync', ApmImmichSync);
declare global {
  interface HTMLElementTagNameMap {
    'apm-immich-sync': ApmImmichSync;
  }
}
