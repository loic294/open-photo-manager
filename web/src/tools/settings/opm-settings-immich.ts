import { html, nothing } from 'lit';
import { OpmElement } from '@/base/opm-element';
import { immichStatus, immichStatusError, loadImmichStatus } from '@/stores/immich';
import '@/tools/settings/immich/opm-immich-connection';
import '@/tools/settings/immich/opm-immich-mappings';
import '@/tools/settings/immich/opm-immich-outbox';
import '@/tools/settings/immich/opm-immich-sync';

export class OpmSettingsImmich extends OpmElement {
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
        <opm-immich-connection></opm-immich-connection>
        ${
          status.connected
            ? html`
          <opm-immich-mappings></opm-immich-mappings>
          <section class="card border border-base-300 bg-base-200">
            <div class="card-body gap-4 min-w-0">
              <h2 class="card-title">Direct database path updates</h2>
              <p>${status.direct_database.reason}</p>
              <p>Last executor check: ${
                status.direct_database.checked_at == null
                  ? 'not yet'
                  : new Date(status.direct_database.checked_at * 1000).toLocaleString()
              }.
                Every tracked move checks again before moving a file.</p>
              <div class="alert alert-warning" role="alert"><span>${status.direct_database.warning}</span></div>
              <p>Mount a read-only secret file and set <code>OPM_IMMICH_DATABASE_URL_FILE</code>
                on the server. The PostgreSQL URL must use <code>sslmode=verify-full</code>
                and a trusted CA. No database password is stored in these settings.</p>
              <p>If database access is missing, unavailable or incompatible, tracked in-library
                moves are refused: no file is moved. Fix the reported problem and retry.
                Untracked files can continue. If access fails after a file moves, its path update
                stays in the durable outbox with a warning; it is not confirmed as successful.</p>
            </div>
          </section>
          <opm-immich-sync></opm-immich-sync>
          <opm-immich-outbox></opm-immich-outbox>
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

customElements.define('opm-settings-immich', OpmSettingsImmich);
declare global {
  interface HTMLElementTagNameMap {
    'opm-settings-immich': OpmSettingsImmich;
  }
}
