import { html, nothing } from 'lit';
import { live } from 'lit/directives/live.js';
import { isAbortError, isApiError } from '@/api/errors';
import type { OutboxItemDto } from '@/api/generated/OutboxItemDto';
import type { OutboxPage } from '@/api/generated/OutboxPage';
import type { OutboxState } from '@/api/generated/OutboxState';
import { dismissImmichOutbox, fetchImmichOutbox, retryImmichOutbox } from '@/api/immich';
import { ApmElement } from '@/base/apm-element';
import { confirmDialog } from '@/components/confirm-dialog';
import { immichStatus, loadImmichStatus } from '@/stores/immich';
import { describeImmichError } from '@/tools/settings/immich/immich-errors';
import { relativeTime } from '@/tools/trash/trash-text';

const LABELS = { moved: 'Moved', removed: 'Removed', added: 'Added' };
const PAGE_SIZE = 50;

export class ApmImmichOutbox extends ApmElement {
  static override properties = {
    pages: { state: true },
    selected: { state: true },
    busy: { state: true },
    loading: { state: true },
    failure: { state: true },
    pendingOpen: { state: true },
  };
  private pages: Record<OutboxState, OutboxPage> = {
    attention: { items: [], total: 0 },
    pending: { items: [], total: 0 },
  };
  private selected: ReadonlySet<number> = new Set();
  private busy = false;
  private loading: ReadonlySet<OutboxState> = new Set();
  private failure: string | null = null;
  private pendingOpen = false;
  private lastStatus: unknown;
  private requests = new Map<OutboxState, AbortController>();
  private actionRefreshing = false;

  protected override updated(): void {
    if (!this.isConnected) return;
    const status = immichStatus.peek();
    if (this.lastStatus !== status) {
      this.lastStatus = status;
      if (!this.actionRefreshing) void this.reload();
    }
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    for (const request of this.requests.values()) request.abort();
    this.requests.clear();
    this.loading = new Set();
    this.lastStatus = undefined;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.requestUpdate();
  }

  private async load(state: OutboxState, more = false): Promise<void> {
    this.requests.get(state)?.abort();
    const controller = new AbortController();
    this.requests.set(state, controller);
    this.loading = new Set(this.requests.keys());
    const offset = more ? this.pages[state].items.length : 0;
    try {
      const page = await fetchImmichOutbox({
        state,
        offset,
        limit: PAGE_SIZE,
        signal: controller.signal,
      });
      if (this.requests.get(state) === controller && !controller.signal.aborted) {
        const items = more ? [...this.pages[state].items, ...page.items] : page.items;
        this.pages = { ...this.pages, [state]: { ...page, items } };
      }
    } catch (error) {
      if (!isAbortError(error) && this.requests.get(state) === controller) {
        this.failure = describeImmichError(error).message;
      }
    } finally {
      if (this.requests.get(state) === controller) {
        this.requests.delete(state);
        this.loading = new Set(this.requests.keys());
      }
    }
  }

  private async reload(): Promise<void> {
    await Promise.all([
      this.load('attention'),
      ...(this.pendingOpen ? [this.load('pending')] : []),
    ]);
  }

  private toggle(id: number, checked: boolean): void {
    const selected = new Set(this.selected);
    if (checked) selected.add(id);
    else selected.delete(id);
    this.selected = selected;
  }

  private async act(kind: 'retry' | 'dismiss', all = false): Promise<void> {
    if (this.busy || immichStatus.peek()?.needs_reconnect || (!all && !this.selected.size)) return;
    const ids = [...this.selected];
    this.busy = true;
    this.failure = null;
    try {
      if (
        kind === 'dismiss' &&
        !(await confirmDialog({
          title: `Dismiss ${ids.length} changes?`,
          danger: true,
          confirmLabel: 'Dismiss selected',
          message: `Immich is not updated for these ${ids.length} files. Their old entries stay in Immich until removed by hand.`,
        }))
      )
        return;
      if (kind === 'dismiss') await dismissImmichOutbox(ids);
      else await retryImmichOutbox(all ? undefined : ids);
      this.selected = new Set();
      this.actionRefreshing = true;
      await loadImmichStatus();
      this.lastStatus = immichStatus.peek();
      await this.reload();
    } catch (error) {
      this.failure = describeImmichError(error).message;
      if (isApiError(error) && error.code === 'immich_not_connected') await loadImmichStatus();
    } finally {
      this.actionRefreshing = false;
      this.busy = false;
    }
  }

  private renderItem(item: OutboxItemDto) {
    return html`<tr>
      <td><input class="checkbox checkbox-sm" type="checkbox" aria-label=${`Select change ${item.id}`}
        .checked=${live(this.selected.has(item.id))} ?disabled=${this.busy}
        @change=${(event: Event) => this.toggle(item.id, (event.target as HTMLInputElement).checked)} /></td>
      <td><span class="badge badge-outline badge-sm">${LABELS[item.kind]}</span></td>
      <td><div class="max-w-56 truncate" title=${item.from ?? ''}>${item.from ?? '—'}</div></td>
      <td><div class="max-w-56 truncate" title=${item.to ?? ''}>${item.to ?? '—'}</div></td>
      <td>${item.attempts}</td>
      <td class="min-w-48 whitespace-normal">${item.last_error ? describeImmichError(item.last_error).message : 'None'}</td>
      <td class="whitespace-nowrap" title=${new Date(item.created_at * 1000).toLocaleString()}>${relativeTime(item.created_at)}</td>
    </tr>`;
  }

  private list(state: OutboxState) {
    const page = this.pages[state];
    const checked = page.items.length > 0 && page.items.every((item) => this.selected.has(item.id));
    return html`<div class="max-h-96 overflow-auto"><table class="table table-sm">
      <thead><tr>
        <th><input class="checkbox checkbox-sm" type="checkbox" aria-label=${`Select loaded ${state} changes`}
          .checked=${live(checked)} ?disabled=${this.busy || !page.items.length}
          @change=${(event: Event) => {
            const selected = new Set(this.selected);
            for (const item of page.items) {
              if ((event.target as HTMLInputElement).checked) selected.add(item.id);
              else selected.delete(item.id);
            }
            this.selected = selected;
          }} /></th>
        <th>Change</th><th>From</th><th>To</th><th>Attempts</th><th>Last error</th><th>Age</th>
      </tr></thead>
      <tbody>${page.items.map((item) => this.renderItem(item))}</tbody>
    </table></div>
    ${
      this.loading.has(state)
        ? html`<p role="status"><span class="loading loading-spinner loading-sm"></span>Loading changes…</p>`
        : page.items.length === 0
          ? html`<p>No ${state === 'attention' ? 'changes needing attention' : 'waiting changes'}.</p>`
          : nothing
    }
    ${
      page.items.length < page.total
        ? html`<button class="btn btn-sm" ?disabled=${this.loading.has(state)}
      @click=${() => this.load(state, true)}>Load more</button>`
        : nothing
    }`;
  }

  protected override render() {
    const status = immichStatus.value;
    const summary = status?.outbox;
    const pending = summary?.pending ?? status?.outbox_pending ?? 0;
    const attention = summary?.attention ?? status?.needs_attention ?? 0;
    const blocked = !!status?.needs_reconnect;
    return html`<section class="card border border-base-300 bg-base-200">
      <div class="card-body gap-4 min-w-0">
        <h2 class="card-title">Changes waiting for Immich</h2>
        ${attention ? html`<div class="alert alert-warning" role="alert">${attention} changes need your decision</div>` : nothing}
        ${
          pending
            ? html`<p>${pending} changes will be sent to Immich soon. ${summary?.waiting_for_immich ?? 0} wait for Immich's own rescan.</p>`
            : !attention
              ? html`<p>All caught up. Moves and deletes are mirrored to Immich about 15 seconds after file operations stop.</p>`
              : nothing
        }
        <p>Last run ${summary?.last_run_at == null ? 'not yet' : relativeTime(summary.last_run_at)}
          ${summary?.oldest_pending_at != null ? html`· Oldest pending ${relativeTime(summary.oldest_pending_at)}` : nothing}</p>
        ${
          summary?.scans_on_demand === false
            ? html`<div class="alert alert-info">
          This account cannot trigger a library scan. Changes reach Immich on its own schedule and need attention after three days if still waiting.
        </div>`
            : nothing
        }
        ${summary?.last_error ? html`<div class="alert alert-warning" role="alert">${describeImmichError(summary.last_error).message}</div>` : nothing}
        ${
          this.failure
            ? html`<div class="alert alert-error" role="alert"><span>${this.failure}</span>
          <button class="btn btn-sm" @click=${() => {
            this.failure = null;
            void this.reload();
          }}>Reload changes</button>
        </div>`
            : nothing
        }
        ${blocked ? html`<p>Connect again first</p>` : nothing}
        ${this.list('attention')}
        <details class="collapse collapse-arrow border border-base-300"
          @toggle=${(event: Event) => {
            const open = (event.target as HTMLDetailsElement).open;
            if (open === this.pendingOpen) return;
            this.pendingOpen = open;
            if (open) void this.load('pending');
            else this.requests.get('pending')?.abort();
          }}>
          <summary class="collapse-title font-medium">Waiting changes</summary>
          <div class="collapse-content">${this.pendingOpen ? this.list('pending') : nothing}</div>
        </details>
        <div class="flex flex-wrap gap-2">
          <button class="btn btn-sm" ?disabled=${this.busy || blocked || !this.selected.size} @click=${() => this.act('retry')}>Retry selected</button>
          <button class="btn btn-sm" ?disabled=${this.busy || blocked} @click=${() => this.act('retry', true)}>Retry all</button>
          <button class="btn btn-error btn-outline btn-sm" ?disabled=${this.busy || blocked || !this.selected.size} @click=${() => this.act('dismiss')}>Dismiss selected</button>
        </div>
      </div>
    </section>`;
  }
}

customElements.define('apm-immich-outbox', ApmImmichOutbox);
declare global {
  interface HTMLElementTagNameMap {
    'apm-immich-outbox': ApmImmichOutbox;
  }
}
