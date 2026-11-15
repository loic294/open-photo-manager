import { html, nothing } from 'lit';
import { errorMessage } from '@/api/errors';
import type { IsmInfoDto } from '@/api/generated/IsmInfoDto';
import type { IsmRootDto } from '@/api/generated/IsmRootDto';
import type { IsmStatusDto } from '@/api/generated/IsmStatusDto';
import type { VolumeDto } from '@/api/generated/VolumeDto';
import { connectIsm, disconnectIsm, fetchIsmStatus, stageIsmBinding, testIsm } from '@/api/ism';
import { OpmElement } from '@/base/opm-element';
import { loadVolumes, volumes, volumesStatus } from '@/stores/volumes';

/** What ISM does with a change OPM sends for a root, from its `direct_writes` state. */
export function writesLabel(root: IsmRootDto): string {
  const state = root.direct_writes;
  if (state === 'enabled') return 'changes enabled';
  if (state === 'not_linked' || (!root.immich_import_path && state !== 'blocked')) {
    return 'changes enabled (not linked to Immich)';
  }
  const reason = root.direct_writes_reason ? `: ${root.direct_writes_reason}` : '';
  return `changes refused (${state}${reason})`;
}

export class OpmSettingsIsm extends OpmElement {
  static override properties = {
    status: { state: true },
    info: { state: true },
    failure: { state: true },
    busy: { state: true },
    editing: { state: true },
    baseUrl: { state: true },
    token: { state: true },
  };

  private status: IsmStatusDto | null = null;
  private info: IsmInfoDto | null = null;
  private failure: string | null = null;
  private busy = false;
  private editing = false;
  private baseUrl = '';
  private token = '';

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
    void this.load();
  }

  private async load(): Promise<void> {
    if (volumesStatus.peek() === 'idle') void loadVolumes();
    try {
      this.status = await fetchIsmStatus();
      this.baseUrl = this.status.connection?.base_url ?? '';
      this.failure = null;
    } catch (error) {
      this.failure = errorMessage(error, 'Could not load Sync Manager settings.');
      return;
    }
    if (this.status.connection && !this.info) {
      try {
        this.info = await testIsm();
      } catch (error) {
        this.failure = errorMessage(error, 'Could not reach Sync Manager to list its roots.');
      }
    }
  }

  private async bind(volume: VolumeDto, rootId: string): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    this.failure = null;
    try {
      this.status = await stageIsmBinding({ volume_id: volume.id, root_id: rootId || null });
    } catch (error) {
      this.failure = errorMessage(error, `Could not change the binding of ${volume.name}.`);
    } finally {
      this.busy = false;
    }
  }

  private renderBindings(status: IsmStatusDto) {
    const roots = (this.info?.roots ?? []).filter((root) => root.opm_prefix);
    const list = volumes.value;
    return html`<div class="flex flex-col gap-2">
      <h3 class="font-semibold">Volume bindings</h3>
      <p class="text-sm">Changes are applied the next time Open Photo Manager starts.</p>
      ${
        list.length
          ? html`<table class="table table-sm">
            <thead><tr><th>Volume</th><th>Sync Manager root</th><th>State</th></tr></thead>
            <tbody>${list.map((volume) => {
              const bound = status.bindings.find((binding) => binding.volume_id === volume.id);
              const staged = status.staged.find((change) => change.volume_id === volume.id);
              const selected = staged ? (staged.root_id ?? '') : (bound?.root_id ?? '');
              return html`<tr>
                <td>${volume.name}</td>
                <td><select class="select select-sm" aria-label=${`Sync Manager root for ${volume.name}`}
                  ?disabled=${this.busy || !this.info}
                  @change=${(event: Event) =>
                    this.bind(volume, (event.target as HTMLSelectElement).value)}>
                  <option value="" ?selected=${selected === ''}>Not bound</option>
                  ${roots.map(
                    (root) =>
                      html`<option value=${root.root_id} ?selected=${selected === root.root_id}>
                        ${root.name} · ${writesLabel(root)}</option>`,
                  )}
                </select></td>
                <td>${
                  staged
                    ? html`<span class="badge badge-warning">${
                        staged.root_id
                          ? `Binds to ${staged.root_name} at restart`
                          : 'Unbinds at restart'
                      }</span>${
                        staged.error
                          ? html`<p class="text-error text-sm">${staged.error}</p>`
                          : nothing
                      }`
                    : bound
                      ? html`<span class="badge badge-success">Bound to ${bound.root_name}</span>`
                      : html`<span class="badge badge-ghost">Local</span>`
                }</td>
              </tr>`;
            })}</tbody>
          </table>`
          : html`<p>No volumes are mounted.</p>`
      }
    </div>`;
  }

  private async connect(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (this.busy) return;
    this.busy = true;
    this.failure = null;
    try {
      this.info = await connectIsm({ base_url: this.baseUrl, token: this.token });
      this.token = '';
      this.editing = false;
      await this.load();
    } catch (error) {
      this.failure = errorMessage(error, 'Could not connect to Sync Manager.');
    } finally {
      this.busy = false;
    }
  }

  private async check(): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    this.failure = null;
    try {
      this.info = await testIsm();
    } catch (error) {
      this.info = null;
      this.failure = errorMessage(error, 'Could not check Sync Manager.');
    } finally {
      this.busy = false;
    }
  }

  private async disconnect(): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    this.failure = null;
    try {
      this.status = await disconnectIsm();
      this.info = null;
      this.baseUrl = '';
      this.token = '';
      this.editing = false;
    } catch (error) {
      this.failure = errorMessage(error, 'Could not disconnect Sync Manager.');
    } finally {
      this.busy = false;
    }
  }

  protected override render() {
    const connection = this.status?.connection;
    return html`<section class="card border border-base-300 bg-base-200">
      <div class="card-body gap-4">
        <h2 class="card-title">Immich Sync Manager</h2>
        <p>Connect an OPM client token to discover the roots available to this application.</p>
        <div class="alert alert-info" role="status">On a bound volume Open Photo Manager never
          changes files itself: moves, renames and trash are sent to Sync Manager in batches and
          their progress is shown as the task runs. Copies onto it and replacing files are refused.
          While Sync Manager refuses changes to an Immich library, those changes fail.</div>
        ${this.failure ? html`<div class="alert alert-error" role="alert">${this.failure}</div>` : nothing}
        ${
          !this.status
            ? this.failure
              ? html`<button class="btn btn-sm" @click=${this.load}>Retry</button>`
              : html`<div class="skeleton h-32 w-full" role="status" aria-label="Loading Sync Manager settings"></div>`
            : html`
          ${
            connection
              ? html`<p><strong>${connection.instance_name}</strong> · ${connection.base_url}</p>
                <p>${this.status.bound_volumes} bound volumes</p>
                <div class="flex flex-wrap gap-2">
                  <button class="btn btn-sm" ?disabled=${this.busy} @click=${this.check}>Check connection</button>
                  <button class="btn btn-sm" ?disabled=${this.busy} @click=${() => {
                    this.editing = !this.editing;
                    this.token = '';
                  }}>${this.editing ? 'Cancel' : 'Connect again'}</button>
                  <button class="btn btn-sm" ?disabled=${this.busy || this.status.bound_volumes > 0}
                    @click=${this.disconnect}>Disconnect</button>
                </div>
                ${this.status.bound_volumes > 0 ? html`<p>Bound volumes must remain protected; the connection cannot be removed.</p>` : nothing}
                ${this.renderBindings(this.status)}`
              : nothing
          }
          ${
            !connection || this.editing
              ? html`<form class="flex flex-col gap-3" @submit=${this.connect}>
                <label class="form-control"><span class="label-text">Sync Manager URL</span>
                  <input class="input input-bordered w-full" type="url" required autocomplete="url"
                    placeholder="https://sync.example" .value=${this.baseUrl}
                    @input=${(event: Event) => {
                      this.baseUrl = (event.target as HTMLInputElement).value;
                    }} /></label>
                <label class="form-control"><span class="label-text">OPM client token</span>
                  <input class="input input-bordered w-full" type="password" required autocomplete="off"
                    .value=${this.token} @input=${(event: Event) => {
                      this.token = (event.target as HTMLInputElement).value;
                    }} /></label>
                <div><button class="btn btn-primary btn-sm" type="submit" ?disabled=${this.busy}>
                  ${this.busy ? 'Connecting…' : 'Connect'}</button></div>
              </form>`
              : nothing
          }
          ${
            this.info
              ? html`<div>
                <h3 class="font-semibold">Available roots</h3>
                ${
                  this.info.roots.length
                    ? html`<ul class="list-disc pl-5">${this.info.roots.map(
                        (
                          root,
                        ) => html`<li>${root.name} · ${root.available ? 'available' : 'unavailable'}
                          · ${writesLabel(root)}${root.opm_prefix ? html` · ${root.opm_prefix}` : nothing}</li>`,
                      )}</ul>`
                    : html`<p>No roots are available to this token.</p>`
                }
              </div>`
              : nothing
          }`
        }
      </div>
    </section>`;
  }
}

customElements.define('opm-settings-ism', OpmSettingsIsm);
declare global {
  interface HTMLElementTagNameMap {
    'opm-settings-ism': OpmSettingsIsm;
  }
}
