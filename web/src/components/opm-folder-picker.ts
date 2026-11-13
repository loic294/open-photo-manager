import { html, nothing, type PropertyValues } from 'lit';
import { api } from '@/api/client';
import { errorMessage, isAbortError } from '@/api/errors';
import type { FoldersResponse } from '@/api/generated/FoldersResponse';
import type { PlanRef } from '@/api/generated/PlanRef';
import type { Scope } from '@/api/generated/Scope';
import { OpmElement } from '@/base/opm-element';
import type { FolderOpenEvent } from '@/components/opm-folder-list';
import '@/components/opm-folder-list';
import '@/components/opm-modal';
import { settings } from '@/stores/settings';
import { volumes } from '@/stores/volumes';
import { EMPTY_FILTER } from '@/util/filter';

export class OpmFolderPicker extends OpmElement {
  static override properties = {
    open: { type: Boolean },
    heading: { type: String },
    initial: { attribute: false },
    plan: { attribute: false },
    location: { state: true },
    listing: { state: true },
    loading: { state: true },
    failure: { state: true },
  };

  open = false;
  heading = 'Select folder';
  initial: Scope = { volume_id: null, dir: '', recursive: true };
  plan: PlanRef | null = null;
  private location: Scope = this.initial;
  private listing: FoldersResponse | null = null;
  private loading = false;
  private failure = '';
  private request: AbortController | null = null;

  protected override updated(changed: PropertyValues<this>): void {
    if (changed.has('open') && this.open) {
      this.location = { ...this.initial };
      void this.load();
    } else if (changed.has('plan') && this.open) {
      void this.load();
    } else if (changed.has('open')) {
      this.request?.abort();
    }
  }

  override disconnectedCallback(): void {
    this.request?.abort();
    super.disconnectedCallback();
  }

  private async load(): Promise<void> {
    this.request?.abort();
    const request = new AbortController();
    this.request = request;
    this.loading = true;
    this.failure = '';
    this.listing = null;
    try {
      const listing = await api.post<FoldersResponse>('/api/browse/folders', {
        body: {
          volume_id: this.location.volume_id,
          dir: this.location.dir,
          filter: EMPTY_FILTER,
          plan: this.plan,
        },
        signal: request.signal,
      });
      if (!request.signal.aborted) this.listing = listing;
    } catch (error) {
      if (this.request === request && !request.signal.aborted && !isAbortError(error))
        this.failure = errorMessage(error, 'Could not load folders');
    } finally {
      if (this.request === request) this.loading = false;
    }
  }

  private close(): void {
    this.open = false;
    this.dispatchEvent(new Event('close'));
  }

  private move(volume_id: number | null, dir: string): void {
    this.location = { ...this.location, volume_id, dir };
    void this.load();
  }

  protected override render() {
    const volume = volumes.value.find((entry) => entry.id === this.location.volume_id);
    return html`<opm-modal .open=${this.open} .heading=${this.heading}
      @close=${() => this.close()}
      .body=${html`
        <div class="flex items-center gap-2 pb-3">
          <button class="btn btn-sm btn-ghost" ?disabled=${this.location.volume_id === null}
            @click=${() =>
              this.move(
                this.location.dir ? this.location.volume_id : null,
                this.location.dir.includes('/')
                  ? this.location.dir.slice(0, this.location.dir.lastIndexOf('/'))
                  : '',
              )}>Up</button>
          <span class="min-w-0 break-all text-sm">${volume ? `${volume.name}/${this.location.dir}` : 'All volumes'}</span>
        </div>
        ${
          this.failure
            ? html`<div class="alert alert-error" role="alert">${this.failure}
          <button class="btn btn-sm" @click=${() => this.load()}>Retry</button></div>`
            : nothing
        }
        <div class="max-h-72 overflow-auto">
          <opm-folder-list .folders=${this.listing?.folders ?? []} .loading=${this.loading}
            .softExcludedFolders=${settings.value?.soft_excluded_folders ?? []}
            .showMatches=${this.plan !== null}
            @folder-open=${(event: FolderOpenEvent) => this.move(event.detail.entry.volume_id, event.detail.entry.path)}>
          </opm-folder-list>
        </div>
        ${this.plan ? html`<p class="mt-2 text-xs text-base-content/70">Matches count files with duplicates in each folder and its subfolders, from the current search.</p>` : nothing}
        ${!this.loading && !this.failure && this.listing?.folders.length === 0 ? html`<p class="py-3 text-sm">No subfolders.</p>` : nothing}
      `}
      .actions=${html`<button class="btn btn-ghost" @click=${() => this.close()}>Cancel</button>
        <button class="btn btn-primary" ?disabled=${this.loading || !!this.failure}
          @click=${() => {
            this.dispatchEvent(
              new CustomEvent<Scope>('folder-picked', {
                detail: { ...this.location },
                bubbles: true,
              }),
            );
            this.close();
          }}>Use ${this.location.volume_id === null ? 'all volumes' : 'this folder'}</button>`}>
    </opm-modal>`;
  }
}

customElements.define('opm-folder-picker', OpmFolderPicker);

declare global {
  interface HTMLElementTagNameMap {
    'opm-folder-picker': OpmFolderPicker;
  }
}
