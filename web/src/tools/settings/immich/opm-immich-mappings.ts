import { html, nothing } from 'lit';
import { live } from 'lit/directives/live.js';
import { isAbortError, isApiError } from '@/api/errors';
import type { ImmichExcludeTrashResult } from '@/api/generated/ImmichExcludeTrashResult';
import type { ImmichLibraryDto } from '@/api/generated/ImmichLibraryDto';
import type { ImmichMappingEntry } from '@/api/generated/ImmichMappingEntry';
import type { ImmichPrefixSuggestion } from '@/api/generated/ImmichPrefixSuggestion';
import {
  detectImmichPrefix,
  excludeImmichTrash,
  fetchImmichLibraries,
  requestImmichSync,
  saveImmichMappings,
} from '@/api/immich';
import { OpmElement } from '@/base/opm-element';
import type { SelectChangeEvent, SelectOption } from '@/components/opm-select';
import '@/components/opm-select';
import '@/components/opm-save-bar';
import { toast } from '@/components/toast';
import { UnsavedGuard } from '@/controllers/unsaved-guard';
import { applyImmichStatus, immichStatus } from '@/stores/immich';
import { loadTasks } from '@/stores/tasks';
import { volumes, volumesError } from '@/stores/volumes';
import { describeImmichError } from '@/tools/settings/immich/immich-errors';

export class OpmImmichMappings extends OpmElement {
  static override properties = {
    draft: { state: true },
    libraries: { state: true },
    loading: { state: true },
    saving: { state: true },
    failure: { state: true },
    libraryError: { state: true },
    rowErrors: { state: true },
    suggestions: { state: true },
    detecting: { state: true },
    excluding: { state: true },
    excluded: { state: true },
  };
  private draft: ImmichMappingEntry[] = [];
  private baseline: ImmichMappingEntry[] = [];
  private libraries: ImmichLibraryDto[] = [];
  private loading = false;
  private saving = false;
  private failure: string | null = null;
  private libraryError: string | null = null;
  private rowErrors: Record<number, string> = {};
  private suggestions: Record<number, ImmichPrefixSuggestion[]> = {};
  private detecting: ReadonlySet<number> = new Set();
  private excluding = false;
  private excluded: ImmichExcludeTrashResult | null = null;
  private stored: unknown;
  private connectionKey = '';
  private libraryToken = 0;
  private detects = new Map<number, AbortController>();
  private readonly guard = new UnsavedGuard(this, () => this.dirty);

  private get full(): boolean {
    return immichStatus.peek()?.mode === 'full';
  }
  private get blocked(): boolean {
    return !!immichStatus.peek()?.needs_reconnect;
  }
  private get dirty(): boolean {
    return (
      JSON.stringify(this.meaningful(this.draft)) !== JSON.stringify(this.meaningful(this.baseline))
    );
  }
  private meaningful(entries: ImmichMappingEntry[]): ImmichMappingEntry[] {
    return entries
      .filter((entry) => entry.path_prefix || entry.library_id)
      .map((entry) => ({ ...entry, library_id: this.full ? entry.library_id : null }))
      .sort((a, b) => a.volume_id - b.volume_id);
  }
  private row(id: number): ImmichMappingEntry {
    return (
      this.draft.find((entry) => entry.volume_id === id) ?? {
        volume_id: id,
        library_id: null,
        path_prefix: '',
      }
    );
  }
  private half(entry: ImmichMappingEntry): boolean {
    return this.full && !!entry.library_id !== !!entry.path_prefix;
  }

  protected override willUpdate(): void {
    const status = immichStatus.peek();
    if (status && this.stored !== status.mappings && !this.dirty) {
      this.stored = status.mappings;
      this.baseline = status.mappings.map(({ volume_id, library_id, path_prefix }) => ({
        volume_id,
        library_id,
        path_prefix,
      }));
      this.draft = this.baseline.map((entry) => ({ ...entry }));
    }
  }

  protected override updated(): void {
    this.guard.sync();
    if (!this.isConnected) return;
    const status = immichStatus.peek();
    const key = `${status?.connection?.connected_at}:${status?.mode}:${status?.needs_reconnect}`;
    if (key !== this.connectionKey) {
      this.connectionKey = key;
      this.abortDetects();
      if (this.full && !this.blocked) void this.reloadLibraries();
      else {
        ++this.libraryToken;
        this.loading = false;
      }
    }
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    ++this.libraryToken;
    this.abortDetects();
    this.connectionKey = '';
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.requestUpdate();
  }

  private abortDetects(): void {
    for (const controller of this.detects.values()) controller.abort();
    this.detects.clear();
    this.detecting = new Set();
  }

  private async reloadLibraries(): Promise<void> {
    const token = ++this.libraryToken;
    this.loading = true;
    this.libraryError = null;
    try {
      const response = await fetchImmichLibraries();
      if (token === this.libraryToken) this.libraries = response.libraries;
    } catch (error) {
      if (token === this.libraryToken) this.libraryError = describeImmichError(error).message;
    } finally {
      if (token === this.libraryToken) this.loading = false;
    }
  }

  private edit(id: number, change: Partial<ImmichMappingEntry>): void {
    const previous = this.row(id);
    if ('library_id' in change && change.library_id !== previous.library_id) {
      this.detects.get(id)?.abort();
      this.detects.delete(id);
      this.detecting = new Set([...this.detecting].filter((value) => value !== id));
      const { [id]: _old, ...rest } = this.suggestions;
      this.suggestions = rest;
      const library = this.libraries.find((item) => item.id === change.library_id);
      if (!previous.path_prefix && library?.import_paths.length === 1) {
        change = { ...change, path_prefix: library.import_paths[0] ?? '' };
      }
    }
    const next = { ...previous, ...change };
    this.draft = [...this.draft.filter((entry) => entry.volume_id !== id), next];
    this.rowErrors = { ...this.rowErrors, [id]: '' };
    this.failure = null;
  }

  private options(entry: ImmichMappingEntry): SelectOption[] {
    const options: SelectOption[] = [
      { value: '', label: 'Not mapped' },
      ...this.libraries.map((library) => ({
        value: library.id,
        label: library.name,
        title: `${library.asset_count} assets · ${library.import_paths.join(', ')}`,
      })),
    ];
    if (entry.library_id && !this.libraries.some((library) => library.id === entry.library_id)) {
      options.push({ value: entry.library_id, label: 'Unknown library' });
    }
    return options;
  }

  private async detect(id: number): Promise<void> {
    if (this.detects.has(id) || this.blocked) return;
    const controller = new AbortController();
    this.detects.set(id, controller);
    this.detecting = new Set(this.detects.keys());
    this.rowErrors = { ...this.rowErrors, [id]: '' };
    try {
      const response = await detectImmichPrefix(
        {
          volume_id: id,
          library_id: this.full ? this.row(id).library_id : null,
        },
        controller.signal,
      );
      if (this.detects.get(id) === controller && !controller.signal.aborted) {
        this.suggestions = { ...this.suggestions, [id]: response.suggestions };
      }
    } catch (error) {
      if (!isAbortError(error) && this.detects.get(id) === controller) {
        this.rowErrors = { ...this.rowErrors, [id]: describeImmichError(error).message };
      }
    } finally {
      if (this.detects.get(id) === controller) {
        this.detects.delete(id);
        this.detecting = new Set(this.detects.keys());
      }
    }
  }

  private discard(): void {
    this.abortDetects();
    const status = immichStatus.peek();
    this.baseline = (status?.mappings ?? []).map(({ volume_id, library_id, path_prefix }) => ({
      volume_id,
      library_id,
      path_prefix,
    }));
    this.draft = this.baseline.map((entry) => ({ ...entry }));
    this.stored = status?.mappings;
    this.rowErrors = {};
    this.suggestions = {};
    this.failure = null;
  }

  private async save(): Promise<void> {
    if (this.saving || this.blocked || this.draft.some((entry) => this.half(entry))) return;
    // Keep the sent entry order: validation indices refer to this list, not the visible rows.
    const entries = volumes
      .peek()
      .map((volume) => this.row(volume.id))
      .filter((entry) => !!entry.path_prefix)
      .map((entry) => ({ ...entry, library_id: this.full ? entry.library_id : null }));
    this.saving = true;
    this.rowErrors = {};
    this.failure = null;
    try {
      const response = await saveImmichMappings(entries);
      const status = immichStatus.peek();
      if (status) applyImmichStatus({ ...status, mappings: response.mappings });
      this.discard();
      if (response.mappings.length) {
        try {
          await requestImmichSync({ full: false });
          void loadTasks();
          toast.success('Mappings saved. Syncing with Immich…');
        } catch (error) {
          if (isApiError(error) && error.code === 'immich_not_mapped') {
            toast.success('Mappings saved.');
          } else {
            this.failure = `Mappings saved, but syncing could not start. ${describeImmichError(error).message} Syncing also starts by itself within about a minute.`;
          }
        }
      } else toast.success('Mappings saved.');
    } catch (error) {
      const unplaced: string[] = [];
      const rows: Record<number, string> = {};
      if (isApiError(error) && error.code === 'validation_failed' && error.fields.length) {
        for (const item of error.fields) {
          const match = /^mappings\[(\d+)\]\.(path_prefix|library_id|volume_id)$/.exec(item.field);
          const entry = match ? entries[Number(match[1])] : undefined;
          if (entry)
            rows[entry.volume_id] = [rows[entry.volume_id], item.message].filter(Boolean).join(' ');
          else unplaced.push(item.message);
        }
        this.rowErrors = rows;
        this.failure = unplaced.join(' ') || null;
      } else this.failure = describeImmichError(error).message;
    } finally {
      this.saving = false;
    }
  }

  private async exclude(): Promise<void> {
    if (this.excluding || this.blocked) return;
    this.excluding = true;
    this.failure = null;
    try {
      this.excluded = await excludeImmichTrash();
    } catch (error) {
      this.failure = describeImmichError(error).message;
    } finally {
      this.excluding = false;
    }
  }

  private renderRow(id: number, name: string, online: boolean) {
    const entry = this.row(id);
    const error =
      this.rowErrors[id] ||
      (this.half(entry) ? 'Choose a library and enter a prefix, or clear both to unmap.' : '');
    const library = this.libraries.find((item) => item.id === entry.library_id);
    const suggestions = this.suggestions[id];
    return html`<tr data-volume=${id}>
      <th class="align-top">${name} ${!online ? html`<span class="badge badge-warning badge-sm">Offline</span>` : nothing}</th>
      ${
        this.full
          ? html`<td class="align-top">
        <opm-select label=${`Immich library for ${name}`} show-label .options=${this.options(entry)}
          .value=${entry.library_id ?? ''} .disabled=${this.saving || this.blocked}
          @select-change=${(event: SelectChangeEvent) => this.edit(id, { library_id: event.detail.value || null })}></opm-select>
        ${
          library
            ? html`<p class="mt-2 whitespace-normal">${library.asset_count} assets · ${library.import_paths.join(', ')}</p>`
            : entry.library_id
              ? html`<p class="text-warning">Unknown library. Reload libraries or choose another.</p>`
              : nothing
        }
      </td>`
          : nothing
      }
      <td class="min-w-60 align-top">
        <label class="fieldset">
          <span class="fieldset-legend">Path prefix for ${name}</span>
          <input class="input input-sm w-full font-mono" placeholder="/mnt/photo" .value=${live(entry.path_prefix)}
            ?disabled=${this.saving || this.blocked} aria-invalid=${!!error} aria-describedby=${`immich-map-${id}-error`}
            @input=${(event: Event) => this.edit(id, { path_prefix: (event.target as HTMLInputElement).value })} />
        </label>
        <p id=${`immich-map-${id}-error`} class="text-error whitespace-normal">${error}</p>
        <button class="btn btn-sm mt-2" ?disabled=${this.detecting.has(id) || this.saving || this.blocked}
          @click=${() => this.detect(id)}>
          ${this.detecting.has(id) ? html`<span class="loading loading-spinner loading-xs"></span>` : nothing}Auto-detect
        </button>
        ${
          suggestions
            ? suggestions.length
              ? html`<ul class="list mt-2">
          ${suggestions.map(
            (suggestion) => html`<li class="list-row px-0">
            <div class="min-w-0"><p class="font-mono break-all">${suggestion.prefix}</p>
              <p>Found ${suggestion.matched} of ${suggestion.sampled} sampled files</p></div>
            <button class="btn btn-sm" ?disabled=${this.saving || this.blocked} @click=${() => this.edit(id, { path_prefix: suggestion.prefix })}>Use</button>
          </li>`,
          )}
        </ul>`
              : html`<p class="mt-2 whitespace-normal">Immich knows none of the sampled files of this volume. Check that its library has scanned the volume, or type the prefix by hand.</p>`
            : nothing
        }
      </td>
    </tr>`;
  }

  protected override render() {
    const status = immichStatus.value;
    const full = status?.mode === 'full';
    return html`<section class="card border border-base-300 bg-base-200">
      <div class="card-body gap-4 min-w-0">
        <h2 class="card-title">Library mapping</h2>
        <p>A file <code>volume/2024/a.jpg</code> is <code>prefix/2024/a.jpg</code> for Immich.</p>
        ${this.blocked ? html`<p>Connect again first</p>` : nothing}
        ${volumesError.value ? html`<div class="alert alert-error" role="alert">${volumesError.value}</div>` : nothing}
        ${this.failure ? html`<div class="alert alert-error" role="alert">${this.failure}</div>` : nothing}
        ${this.libraryError ? html`<div class="alert alert-error" role="alert">${this.libraryError}</div>` : nothing}
        ${
          full
            ? html`<div><button class="btn btn-sm" ?disabled=${this.loading || this.blocked || this.saving} @click=${this.reloadLibraries}>
          ${this.loading ? html`<span class="loading loading-spinner loading-xs"></span>` : nothing}Reload libraries
        </button></div>`
            : html`<p>Limited mode: enter each volume's path prefix by hand. Library selection and trash exclusion need an administrator account.</p>`
        }
        <div class="overflow-x-auto"><table class="table table-sm">
          <thead><tr><th>Volume</th>${full ? html`<th>Immich library</th>` : nothing}<th>Path as Immich sees it</th></tr></thead>
          <tbody>${volumes.value.map((volume) => this.renderRow(volume.id, volume.name, volume.online))}</tbody>
        </table></div>
        ${!volumes.value.length ? html`<p>No mounted volumes are available.</p>` : nothing}
        ${
          this.dirty
            ? html`<opm-save-bar .invalid=${this.blocked || this.draft.some((entry) => this.half(entry))}
          invalid-message=${this.blocked ? 'Connect again first' : 'Complete the highlighted mappings'}
          .saving=${this.saving} @save=${this.save} @discard=${this.discard}></opm-save-bar>`
            : nothing
        }
        <p>The application's trash folder must not be imported as new photos.</p>
        ${
          full
            ? html`<div><button class="btn btn-sm" ?disabled=${this.excluding || this.blocked || this.saving} @click=${this.exclude}>
          ${this.excluding ? html`<span class="loading loading-spinner loading-xs"></span>` : nothing}Keep the trash out of Immich
        </button></div>`
            : nothing
        }
        ${
          this.excluded
            ? html`<div class="alert ${this.excluded.failed.length ? 'alert-warning' : 'alert-success'}" role="alert">
          <div><p>Exclusion pattern: <code>${this.excluded.pattern}</code></p>
            ${this.excluded.updated.map((item) => html`<p>${item.name}: updated</p>`)}
            ${this.excluded.already_excluded.map((item) => html`<p>${item.name}: already excluded</p>`)}
            ${this.excluded.failed.map((item) => html`<p>${item.name}: ${item.message}</p>`)}
          </div>
        </div>`
            : nothing
        }
      </div>
    </section>`;
  }
}

customElements.define('opm-immich-mappings', OpmImmichMappings);
declare global {
  interface HTMLElementTagNameMap {
    'opm-immich-mappings': OpmImmichMappings;
  }
}
