import { html, nothing } from 'lit';
import { ApmElement } from '@/base/apm-element';
import { immichStatus, immichStatusError, loadImmichStatus } from '@/stores/immich';
import '@/tools/settings/immich/apm-immich-connection';
import '@/tools/settings/immich/apm-immich-mappings';
import '@/tools/settings/immich/apm-immich-outbox';
import '@/tools/settings/immich/apm-immich-sync';

export class ApmSettingsImmich extends ApmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
  }

  protected override render() {
    const status = immichStatus.value;
    const error = immichStatusError.value;
    return html`<div class="flex flex-col gap-6">
      ${
        error
          ? html`<div class="alert alert-error" role="alert"><span>${error}</span>
        <button class="btn btn-sm" @click=${() => loadImmichStatus()}>Retry</button></div>`
          : nothing
      }
      ${
        status
          ? html`
        <apm-immich-connection></apm-immich-connection>
        ${
          status.connected
            ? html`
          <apm-immich-mappings></apm-immich-mappings>
          <apm-immich-sync></apm-immich-sync>
          <apm-immich-outbox></apm-immich-outbox>
        `
            : nothing
        }
      `
          : !error
            ? html`<div aria-label="Loading Immich settings" role="status" class="flex flex-col gap-4">
        <div class="skeleton h-8 w-40"></div><div class="skeleton h-64 w-full"></div>
      </div>`
            : nothing
      }
    </div>`;
  }
}

customElements.define('apm-settings-immich', ApmSettingsImmich);
declare global {
  interface HTMLElementTagNameMap {
    'apm-settings-immich': ApmSettingsImmich;
  }
}
