import { html, nothing } from 'lit';
import { live } from 'lit/directives/live.js';
import { isApiError } from '@/api/errors';
import type { ImmichConnectionDto } from '@/api/generated/ImmichConnectionDto';
import { connectImmich } from '@/api/immich';
import { ApmElement } from '@/base/apm-element';
import { applyImmichStatus, immichStatus, loadImmichStatus } from '@/stores/immich';
import { describeImmichError } from '@/tools/settings/immich/immich-errors';

type Field = 'server_url' | 'email' | 'password';
const LABELS: Record<Field, string> = {
  server_url: 'Server URL',
  email: 'Email',
  password: 'Password',
};

export class ApmImmichConnectForm extends ApmElement {
  static override properties = {
    connection: { attribute: false },
    serverUrl: { state: true },
    email: { state: true },
    password: { state: true },
    insecure: { state: true },
    busy: { state: true },
    errors: { state: true },
    failure: { state: true },
  };
  declare connection: ImmichConnectionDto | null | undefined;
  private serverUrl = '';
  private email = '';
  private password = '';
  private insecure = false;
  private busy = false;
  private errors: Partial<Record<Field, string>> = {};
  private failure: string | null = null;
  private initialised = false;

  protected override willUpdate(): void {
    if (!this.initialised) {
      this.initialised = true;
      this.serverUrl = this.connection?.server_url ?? '';
      this.email = this.connection?.email ?? '';
      this.insecure = this.connection?.allow_insecure_tls ?? false;
    }
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.password = '';
    const input = this.querySelector<HTMLInputElement>('[name="password"]');
    if (input) input.value = '';
  }

  private async submit(event: Event): Promise<void> {
    event.preventDefault();
    if (this.busy) return;
    this.busy = true;
    this.errors = {};
    this.failure = null;
    try {
      const connection = await connectImmich({
        server_url: this.serverUrl,
        email: this.email,
        password: this.password,
        allow_insecure_tls: this.insecure,
      });
      this.password = '';
      const input = this.querySelector<HTMLInputElement>('[name="password"]');
      if (input) input.value = '';
      const previous = immichStatus.peek();
      applyImmichStatus({
        connected: true,
        mode: connection.is_admin ? 'full' : 'limited',
        connection,
        reachable: true,
        needs_reconnect: false,
        server_version: connection.server_version,
        version_warning: null,
        mappings: previous?.mappings ?? [],
        last_sync_at: null,
        sync: null,
        outbox_pending: previous?.outbox_pending ?? 0,
        needs_attention: previous?.needs_attention ?? 0,
        outbox: previous?.outbox ?? null,
      });
      void loadImmichStatus();
      this.dispatchEvent(new Event('connected', { bubbles: true }));
    } catch (error) {
      const described = describeImmichError(error);
      const fields: Partial<Record<Field, string>> = {};
      const unplaced: string[] = [];
      if (isApiError(error) && error.code === 'validation_failed') {
        for (const item of error.fields) {
          if (item.field in LABELS) fields[item.field as Field] = item.message;
          else unplaced.push(item.message);
        }
      } else if (described.field) {
        fields[described.field as Field] = described.message;
      } else unplaced.push(described.message);
      this.errors = fields;
      this.failure = unplaced.join(' ') || (Object.keys(fields).length ? null : described.message);
    } finally {
      this.busy = false;
    }
  }

  private field(field: Field) {
    const value =
      field === 'server_url' ? this.serverUrl : field === 'email' ? this.email : this.password;
    const error = this.errors[field];
    return html`<label class="fieldset min-w-0">
      <span class="fieldset-legend">${LABELS[field]}</span>
      <input class="input w-full ${error ? 'input-error' : ''}" name=${field}
        type=${field === 'password' ? 'password' : field === 'email' ? 'email' : 'url'}
        autocomplete=${field === 'password' ? 'off' : field === 'email' ? 'username' : 'url'}
        required .value=${live(value)} aria-invalid=${!!error}
        aria-describedby=${`immich-connect-${field}-help`}
        @input=${(event: Event) => {
          const text = (event.target as HTMLInputElement).value;
          if (field === 'server_url') this.serverUrl = text;
          else if (field === 'email') this.email = text;
          else this.password = text;
          this.errors = { ...this.errors, [field]: undefined };
        }} />
      <span id=${`immich-connect-${field}-help`} class="text-error">${error ?? ''}</span>
    </label>`;
  }

  protected override render() {
    return html`<form aria-label="Connect to Immich" @submit=${this.submit}>
      <fieldset class="fieldset gap-4" ?disabled=${this.busy}>
        ${this.connection ? html`<p>Connect again to replace the stored key. Your mappings are kept.</p>` : nothing}
        <div class="grid gap-2 sm:grid-cols-2">
          <div class="sm:col-span-2">${this.field('server_url')}</div>
          ${this.field('email')} ${this.field('password')}
        </div>
        <label class="flex items-center gap-3">
          <input class="toggle toggle-sm" type="checkbox" .checked=${live(this.insecure)}
            @change=${(event: Event) => {
              this.insecure = (event.target as HTMLInputElement).checked;
            }} />
          Allow a self-signed certificate
        </label>
        <p>Only allow a self-signed certificate for servers on the local network.</p>
        <p>The password is used once to create an API key and is not kept. Disconnecting revokes that key.</p>
        ${this.failure ? html`<div class="alert alert-error" role="alert">${this.failure}</div>` : nothing}
        <div><button type="submit" class="btn btn-primary">
          ${this.busy ? html`<span class="loading loading-spinner loading-sm"></span>Connecting…` : 'Connect'}
        </button></div>
      </fieldset>
    </form>`;
  }
}

customElements.define('apm-immich-connect-form', ApmImmichConnectForm);
declare global {
  interface HTMLElementTagNameMap {
    'apm-immich-connect-form': ApmImmichConnectForm;
  }
}
