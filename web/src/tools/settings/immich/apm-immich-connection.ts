import { html, nothing } from 'lit';
import type { ImmichDisconnectResult } from '@/api/generated/ImmichDisconnectResult';
import { disconnectImmich } from '@/api/immich';
import { ApmElement } from '@/base/apm-element';
import { confirmDialog } from '@/components/confirm-dialog';
import {
  applyImmichStatus,
  immichStatus,
  immichStatusError,
  loadImmichStatus,
} from '@/stores/immich';
import { describeImmichError } from '@/tools/settings/immich/immich-errors';
import '@/tools/settings/immich/apm-immich-connect-form';
import { relativeTime } from '@/tools/trash/trash-text';

export class ApmImmichConnection extends ApmElement {
  static override properties = {
    reconnect: { state: true },
    checking: { state: true },
    busy: { state: true },
    failure: { state: true },
    result: { state: true },
  };
  private reconnect = false;
  private checking = false;
  private busy = false;
  private failure: string | null = null;
  private result: (ImmichDisconnectResult & { keyName: string }) | null = null;

  private async check(): Promise<void> {
    if (this.checking) return;
    this.checking = true;
    await loadImmichStatus({ check: true });
    this.failure = immichStatusError.peek();
    this.checking = false;
  }

  private async disconnect(): Promise<void> {
    const status = immichStatus.peek();
    if (!status?.connection || this.busy) return;
    const keyName = status.connection.api_key_name;
    const count = status.outbox_pending + status.needs_attention;
    this.busy = true;
    try {
      if (
        !(await confirmDialog({
          title: 'Disconnect from Immich?',
          message: `The key this app created in Immich is revoked. Volume mappings are kept. ${count} changes that have not reached Immich yet are forgotten.`,
          confirmLabel: 'Disconnect',
          danger: true,
        }))
      )
        return;
      const result = await disconnectImmich();
      this.result = { ...result, keyName };
      this.failure = null;
      this.reconnect = false;
      applyImmichStatus({
        ...status,
        connected: false,
        mode: 'disconnected',
        connection: null,
        reachable: null,
        needs_reconnect: false,
        server_version: null,
        version_warning: null,
        last_sync_at: null,
        sync: null,
        outbox: null,
        outbox_pending: 0,
        needs_attention: 0,
      });
    } catch (error) {
      this.failure = describeImmichError(error).message;
    } finally {
      this.busy = false;
    }
  }

  protected override render() {
    const status = immichStatus.value;
    const connection = status?.connection;
    const reconnecting = status?.needs_reconnect || this.reconnect;
    return html`<section class="card border border-base-300 bg-base-200">
      <div class="card-body gap-4">
        <h2 class="card-title">Connection</h2>
        ${
          this.result
            ? html`<div class="alert ${this.result.revoked ? 'alert-success' : 'alert-warning'}" role="alert">
          <span>${
            this.result.revoked
              ? 'Disconnected. The API key was revoked.'
              : html`${this.result.message}
            Remove the key named ${this.result.keyName} in Immich: Account settings, API keys.`
          }</span>
          <button class="btn btn-ghost btn-sm" aria-label="Dismiss disconnect result" @click=${() => {
            this.result = null;
          }}>Dismiss</button>
        </div>`
            : nothing
        }
        ${this.failure ? html`<div class="alert alert-error" role="alert">${this.failure}</div>` : nothing}
        ${status?.needs_reconnect ? html`<div class="alert alert-warning" role="alert">Immich no longer accepts the stored key. Connect again first.</div>` : nothing}
        ${
          connection
            ? html`
          <div class="flex flex-wrap items-center gap-2">
            <strong>${connection.user_name}</strong><span>${connection.email}</span>
            <span class="badge badge-outline">${connection.is_admin ? 'Administrator' : 'Regular account'}</span>
          </div>
          <a class="link break-all" href=${connection.server_url} target="_blank" rel="noopener noreferrer">Open Immich: ${connection.server_url}</a>
          <p>Immich ${status?.server_version ?? 'version unknown'} · Connected ${relativeTime(connection.connected_at)}</p>
          ${status?.version_warning ? html`<div class="alert alert-warning" role="alert">${status.version_warning}</div>` : nothing}
          <div><span class="badge ${status?.reachable === false ? 'badge-warning' : 'badge-success'}">
            ${status?.reachable === false ? 'Not answering' : 'Reachable'}</span></div>
          ${
            status?.reachable === false
              ? html`<p>Immich is not answering. Syncing and updates resume by themselves when it is back.</p>
            ${connection.last_error ? html`<p>${describeImmichError(connection.last_error).message}</p>` : nothing}
            <p>Last reached ${connection.last_ok_at === null ? 'never' : relativeTime(connection.last_ok_at)}</p>`
              : nothing
          }
          <p>${
            status?.mode === 'full'
              ? 'Full mode: Libraries are listed and the trash folder can be excluded.'
              : 'Limited mode: This account is not an Immich administrator: libraries cannot be listed, enter the path prefix by hand, and Immich picks up changes on its own library scan schedule.'
          }</p>
          <div class="flex flex-wrap gap-2">
            <button class="btn btn-sm" ?disabled=${this.checking || this.busy || status?.needs_reconnect} @click=${this.check}>
              ${this.checking ? html`<span class="loading loading-spinner loading-xs"></span>` : nothing}Check connection
            </button>
            <button class="btn btn-sm" ?disabled=${this.busy} @click=${() => {
              this.reconnect = !this.reconnect;
            }}>
              ${this.reconnect ? 'Cancel reconnect' : 'Connect again'}
            </button>
            <button class="btn btn-outline btn-sm" ?disabled=${this.busy} @click=${this.disconnect}>Disconnect</button>
          </div>`
            : nothing
        }
        ${
          !connection || reconnecting
            ? html`<apm-immich-connect-form .connection=${connection}
          @connected=${() => {
            this.reconnect = false;
            this.failure = null;
            this.result = null;
          }}></apm-immich-connect-form>`
            : nothing
        }
      </div>
    </section>`;
  }
}

customElements.define('apm-immich-connection', ApmImmichConnection);
declare global {
  interface HTMLElementTagNameMap {
    'apm-immich-connection': ApmImmichConnection;
  }
}
