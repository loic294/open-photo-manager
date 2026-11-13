import { effect } from '@preact/signals-core';
import { html, nothing, type PropertyValues, type TemplateResult } from 'lit';
import { repeat } from 'lit/directives/repeat.js';
import { styleMap } from 'lit/directives/style-map.js';
import { api } from '@/api/client';
import { errorMessage, isAbortError } from '@/api/errors';
import type { DuplicateFile } from '@/api/generated/DuplicateFile';
import type { DuplicateFileExecuteOptions } from '@/api/generated/DuplicateFileExecuteOptions';
import type { DuplicateFileGroup } from '@/api/generated/DuplicateFileGroup';
import type { DuplicateFileGroups } from '@/api/generated/DuplicateFileGroups';
import type { DuplicateFileParams } from '@/api/generated/DuplicateFileParams';
import type { PlanDto } from '@/api/generated/PlanDto';
import type { Scope } from '@/api/generated/Scope';
import { createPlan, executePlan } from '@/api/plans';
import { events } from '@/api/sse';
import { OpmElement } from '@/base/opm-element';
import { confirmDialog } from '@/components/confirm-dialog';
import '@/components/opm-file-card';
import '@/components/opm-folder-picker';
import type { MatchChangeEvent } from '@/components/opm-match-mode';
import '@/components/opm-match-mode';
import '@/components/opm-modal';
import { openPlanResult } from '@/components/opm-plan-result';
import { locations } from '@/stores/nav';
import { lastRun, trackPlanRun } from '@/stores/plan-runs';
import { volumeIdByName, volumes } from '@/stores/volumes';
import { matchMode, strictPrefix } from '@/tools/duplicates/duplicates-state';
import {
  defaultRules,
  differingFields,
  inFolder,
  parentFolders,
  type SelectionRule,
  selectWithReasons,
} from '@/tools/duplicates/file-selection';
import '@/tools/duplicates/opm-duplicate-selection';
import { EMPTY_FILTER } from '@/util/filter';
import { formatCaptureDate, formatFileSize } from '@/util/format';
import { isMatchMode } from '@/util/match-modes';

const PAGE_SIZE = 20;

export class OpmDuplicateFiles extends OpmElement {
  static override properties = {
    viewControls: { attribute: false },
    source: { state: true },
    excluded: { state: true },
    groups: { state: true },
    keep: { state: true },
    plan: { state: true },
    busy: { state: true },
    failure: { state: true },
    searched: { state: true },
    stale: { state: true },
    size: { state: true },
    visible: { state: true },
    automatic: { state: true },
    rules: { state: true },
    picking: { state: true },
    context: { state: true },
    folderFile: { state: true },
    folderIndex: { state: true },
    folderRecursive: { state: true },
  };
  declare viewControls: TemplateResult | undefined;
  private source: Scope = { volume_id: null, dir: '', recursive: true };
  private excluded: Scope[] = [];
  private groups: DuplicateFileGroup[] = [];
  private keep = new Set<number>();
  private plan: PlanDto | null = null;
  private busy = false;
  private failure = '';
  private searched = false;
  private stale = false;
  private size = 280;
  private visible = PAGE_SIZE;
  private reasons = new Map<number, string>();
  private observer: IntersectionObserver | null = null;
  private automatic = false;
  private rules = defaultRules();
  private picking: 'source' | 'exclude' | null = null;
  private context: { file: DuplicateFile; x: number; y: number } | null = null;
  private folderFile: DuplicateFile | null = null;
  private folderIndex = 0;
  private folderRecursive = false;
  private request: AbortController | null = null;
  private stops: (() => void)[] = [];
  private initialized = false;

  override connectedCallback(): void {
    super.connectedCallback();
    if (this.plan) this.stale = true;
    this.stops = [
      effect(() => {
        if (this.initialized) return;
        const location = locations.source.peek();
        const id = volumeIdByName(location.volume);
        if (location.volume !== null && id === undefined) return;
        this.source = { volume_id: id ?? null, dir: location.dir, recursive: true };
        this.initialized = true;
      }),
      events.on('fs_change', () => {
        if (this.plan) this.stale = true;
      }),
      events.onResync(() => {
        if (this.plan) this.stale = true;
      }),
      events.on('task', ({ task }) => {
        if (task.kind === 'index' && task.state === 'done' && this.plan) this.stale = true;
      }),
    ];
    document.addEventListener('pointerdown', this.dismissContext);
    document.addEventListener('keydown', this.contextKey);
  }

  override disconnectedCallback(): void {
    this.observer?.disconnect();
    this.request?.abort();
    for (const stop of this.stops.splice(0)) stop();
    document.removeEventListener('pointerdown', this.dismissContext);
    document.removeEventListener('keydown', this.contextKey);
    super.disconnectedCallback();
  }

  protected override updated(_changed: PropertyValues<this>): void {
    this.observer?.disconnect();
    const sentinel = this.querySelector('[data-load-more]');
    if (!sentinel || typeof IntersectionObserver === 'undefined') return;
    this.observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) this.visible += PAGE_SIZE;
      },
      { root: this.querySelector('[data-groups]'), rootMargin: '600px' },
    );
    this.observer.observe(sentinel);
  }

  private readonly dismissContext = (event: Event): void => {
    if (event.target instanceof Element && event.target.closest('[data-duplicate-menu]')) return;
    this.context = null;
  };
  private readonly contextKey = (event: KeyboardEvent): void => {
    if (event.key === 'Escape') this.context = null;
  };

  private folderLabel(scope: Pick<Scope, 'volume_id' | 'dir'>): string {
    const volume = volumes.value.find((entry) => entry.id === scope.volume_id);
    return volume ? `${volume.name}/${scope.dir}` : 'All volumes';
  }

  private invalidate(): void {
    this.stale = true;
    this.failure = '';
  }

  private async search(): Promise<void> {
    if (this.busy || !this.initialized) return;
    this.request?.abort();
    const request = new AbortController();
    this.request = request;
    this.busy = true;
    this.failure = '';
    this.groups = [];
    this.keep = new Set();
    this.visible = PAGE_SIZE;
    const params: DuplicateFileParams = {
      matching: {
        mode: matchMode.peek(),
        strict_prefix: strictPrefix.peek(),
        source: { scope: this.source, filter: EMPTY_FILTER },
        comparison: { scope: { volume_id: null, dir: '', recursive: true }, filter: EMPTY_FILTER },
      },
      excluded: this.excluded,
    };
    try {
      const plan = await createPlan(
        'duplicates',
        { ...params },
        {
          replaces: this.plan?.id ?? null,
          signal: request.signal,
        },
      );
      if (request.signal.aborted) return;
      this.plan = plan;
      const response = await api.get<DuplicateFileGroups>(`/api/plans/${plan.id}/duplicate-files`, {
        signal: request.signal,
      });
      if (request.signal.aborted) return;
      this.groups = response.groups;
      this.applyRules(this.rules);
      this.searched = true;
      this.stale = false;
    } catch (error) {
      if (!isAbortError(error))
        this.failure = errorMessage(
          error,
          'Could not find duplicates. Try a narrower source folder.',
        );
    } finally {
      if (this.request === request) this.busy = false;
    }
  }

  private applyRules(rules: SelectionRule[]): void {
    this.rules = rules;
    const selections = this.groups.map((group) => selectWithReasons(group, rules));
    this.reasons = new Map(selections.flatMap((selection) => [...selection.reasons]));
    this.keep = new Set(selections.flatMap((selection) => [...selection.keep]));
  }

  private choose(group: DuplicateFileGroup, id: number): void {
    const keep = new Set(this.keep);
    for (const file of group.files) keep.delete(file.item.id);
    const selection = selectWithReasons(group, this.rules, id);
    for (const keeper of selection.keep) keep.add(keeper);
    this.reasons = new Map([...this.reasons, ...selection.reasons]);
    this.keep = keep;
  }

  private preferFolder(file: DuplicateFile): void {
    this.context = null;
    this.folderFile = file;
    this.folderIndex = 0;
    this.folderRecursive = false;
  }

  private async trash(): Promise<void> {
    if (!this.plan || this.busy || this.stale) return;
    const remove = this.groups
      .flatMap((group) => group.files)
      .filter((file) => !this.keep.has(file.item.id));
    if (remove.length === 0) return;
    const plan = this.plan;
    const keep = [...this.keep];
    this.busy = true;
    try {
      const confirmed = await confirmDialog({
        title: `Trash ${remove.length} duplicate files?`,
        message: html`<p>${formatFileSize(remove.reduce((sum, file) => sum + file.item.size, 0))} will move to the trash, including copies outside your source folder. ${keep.length} files will be kept. Sidecars with no remaining owner move with their files.</p>
          <p class="mt-2">Files are rechecked before removal. Changed files or files without a live matching keeper are skipped.</p>`,
        confirmLabel: `Trash ${remove.length} files`,
        danger: true,
      });
      if (!confirmed || !this.isConnected || this.stale || this.plan !== plan) return;
      const options: DuplicateFileExecuteOptions = { keep };
      const task = await executePlan(plan.id, { options: { ...options }, only: null });
      trackPlanRun({ tool: 'duplicates', planId: plan.id, taskId: task.id, title: task.title });
      this.plan = { ...plan, state: 'executing', task_id: task.id };
      this.stale = true;
    } catch (error) {
      this.failure = errorMessage(error, 'Could not trash duplicates');
    } finally {
      this.busy = false;
    }
  }

  private renderGroup(group: DuplicateFileGroup, index: number) {
    const first = group.files[0];
    const kept = group.files.filter((file) => this.keep.has(file.item.id)).length;
    const differences = differingFields(group);
    return html`<article class="card border-2 border-base-300 bg-base-200 p-4" aria-label=${`Duplicate group ${index + 1}`}>
      <div class="mb-3 flex flex-wrap items-baseline justify-between gap-2">
        <h2 class="min-w-0 break-all text-base font-semibold">${first?.item.name}</h2>
        <span class="text-sm text-base-content/70">${group.files.length} copies · ${kept} to keep</span>
      </div>
      <div class="grid gap-4" role="group" aria-label=${`Copies of ${first?.item.name}`}
        style=${styleMap({ gridTemplateColumns: `repeat(auto-fill, minmax(min(100%, ${this.size}px), 1fr))` })}>
        ${repeat(
          group.files,
          (file) => file.item.id,
          (file) => {
            const keep = this.keep.has(file.item.id);
            const path = `${this.folderLabel({ volume_id: file.item.volume_id, dir: file.item.dir })}/${file.item.name}`;
            return html`<div class="min-w-0">
            <div style=${styleMap({ aspectRatio: file.item.width !== null && file.item.width > 0 && file.item.height !== null && file.item.height > 0 ? `${file.item.width} / ${file.item.height}` : '1' })}
              title=${this.reasons.get(file.item.id) ?? ''}
              @contextmenu=${(event: MouseEvent) => {
                event.preventDefault();
                if (!this.busy && !this.stale) {
                  this.context = {
                    file,
                    x: Math.min(event.clientX, window.innerWidth - 260),
                    y: Math.min(event.clientY, window.innerHeight - 80),
                  };
                  void this.updateComplete.then(() =>
                    this.querySelector<HTMLButtonElement>('[data-duplicate-menu] button')?.focus(),
                  );
                }
              }}>
              <opm-file-card .item=${{ ...file.item, overlay: { matched: keep, will_delete: !keep, planned: false } }}
                .size=${this.size} .focused=${true} .selected=${keep} .asButton=${true} .highlightSelection=${false} .showCaption=${false}
                @card-activate=${() => {
                  if (!this.busy && !this.stale) this.choose(group, file.item.id);
                }}>
              </opm-file-card>
            </div>
            <dl class="mt-2 space-y-1 text-sm">
              <div><dt class="sr-only">Filename</dt><dd data-copy-name class="break-all ${differences.name ? 'font-bold' : 'font-normal'}">${file.item.name}</dd></div>
              <div><dt class="sr-only">Size</dt><dd data-copy-size class=${differences.size ? 'font-bold' : 'font-normal'}
                title=${`${file.item.size.toLocaleString()} bytes`}>${formatFileSize(file.item.size)}${differences.size ? ` (${file.item.size.toLocaleString()} bytes)` : nothing}</dd></div>
              <div><dt class="sr-only">Capture date</dt><dd data-copy-date class=${differences.capture_ts ? 'font-bold' : 'font-normal'}>${file.item.capture_ts === null ? 'No capture date' : formatCaptureDate(file.item.capture_ts, undefined, true)}</dd></div>
            </dl>
            <div class="mt-2 flex items-center justify-between gap-2 text-sm">
              <span class="badge badge-sm ${keep ? 'badge-success' : 'badge-error'}" title=${this.reasons.get(file.item.id) ?? ''}>${keep ? 'Keep' : 'Remove'}</span>
            </div>
            <p class="mt-2 break-all text-xs text-base-content/70" title=${path}>${this.folderLabel({ volume_id: file.item.volume_id, dir: file.item.dir })}</p>
          </div>`;
          },
        )}
      </div>
      ${kept > 1 ? html`<p class="mt-3 text-sm text-base-content/70">Multiple keepers are needed because not every file in this group directly matches every other file.</p>` : nothing}
    </article>`;
  }

  protected override render() {
    const total = this.groups.reduce((sum, group) => sum + group.files.length, 0);
    const run = lastRun('duplicates').value;
    const actionable =
      this.groups.length > 0 && !this.busy && !this.stale && this.plan?.state === 'ready';
    const tags = [
      ...new Set(this.groups.flatMap((group) => group.files.flatMap((file) => file.tags))),
    ].sort();
    const sourceFiles = this.groups
      .flatMap((group) => group.files)
      .filter((file) => inFolder(file, this.source, true));
    const inFolderCount = sourceFiles.filter((file) => file.item.dir === this.source.dir).length;
    const folderChoices = this.folderFile ? parentFolders(this.folderFile) : [];
    return html`<section class="flex h-full min-h-0 flex-col" aria-label="Duplicate files">
      <div class="flex-none space-y-3 border-b border-base-300 p-3">
        <div class="flex flex-wrap items-center gap-3">
          ${this.viewControls}
          <opm-match-mode .mode=${matchMode.value} .strict=${strictPrefix.value} .disabled=${this.busy}
            @match-change=${(event: MatchChangeEvent) => {
              if (
                !event.detail ||
                !isMatchMode(event.detail.mode) ||
                typeof event.detail.strict !== 'boolean'
              )
                return;
              matchMode.value = event.detail.mode;
              strictPrefix.value = event.detail.strict;
              this.invalidate();
            }}></opm-match-mode>
          <button class="btn btn-primary btn-sm ml-auto" ?disabled=${this.busy || !this.initialized} @click=${() => this.search()}>
            ${this.busy ? html`<span class="loading loading-spinner loading-xs"></span>` : nothing}Find duplicates</button>
        </div>
        <div class="flex flex-wrap items-center gap-2 text-sm">
          <span>Source</span>
          <button class="btn btn-sm btn-outline max-w-full" ?disabled=${this.busy} @click=${() => {
            this.picking = 'source';
          }}>
            <span class="truncate">${this.folderLabel(this.source)}</span></button>
          ${this.searched && !this.stale ? html`<span role="status" class="text-base-content/70">${inFolderCount} files with duplicates in folder · ${sourceFiles.length - inFolderCount} in subfolders${!this.source.recursive ? ' (not searched)' : ''}</span>` : nothing}
          <label class="flex items-center gap-2"><input class="checkbox checkbox-sm" type="checkbox" .checked=${this.source.recursive} ?disabled=${this.busy}
            @change=${(event: Event) => {
              this.source = {
                ...this.source,
                recursive: (event.target as HTMLInputElement).checked,
              };
              this.invalidate();
            }}>Include subfolders</label>
          <button class="btn btn-sm btn-ghost" ?disabled=${this.busy} @click=${() => {
            this.picking = 'exclude';
          }}>Exclude folder</button>
          ${this.excluded.map(
            (folder, index) => html`<button class="btn btn-xs" ?disabled=${this.busy}
            aria-label=${`Stop excluding ${this.folderLabel(folder)}`}
            @click=${() => {
              this.excluded = this.excluded.filter((_, i) => i !== index);
              this.invalidate();
            }}>Exclude ${this.folderLabel(folder)} ×</button>`,
          )}
        </div>
        <p class="text-xs text-base-content/70">The source chooses which files to match. Copies are searched across all volumes. Exclusions include subfolders and hide files on both sides.</p>
      </div>
      <div data-groups class="min-h-0 flex-1 space-y-4 overflow-auto p-3" aria-busy=${this.busy}>
        ${this.failure ? html`<div class="alert alert-error" role="alert">${this.failure}</div>` : nothing}
        ${this.stale && this.plan ? html`<div class="alert alert-warning" role="status">Refresh the search before changing selections or removing files.</div>` : nothing}
        ${this.plan?.warnings
          .filter((warning) => warning.code !== 'overlapping_scopes')
          .map(
            (warning) => html`
          <div class="alert alert-warning" role="status">${warning.count}: ${warning.message}</div>`,
          )}
        ${
          !this.busy && this.groups.length === 0
            ? html`<div class="py-12 text-center">
          <h2 class="text-lg font-semibold">${this.searched ? 'No duplicates found' : 'Compare every copy, side by side'}</h2>
          <p class="mx-auto mt-2 max-w-prose text-sm text-base-content/70">${
            this.searched
              ? 'Try another source folder, fewer exclusions, or a different matching mode.'
              : 'Choose a source folder and find duplicates. Click a copy to keep it. Green means keep; red means remove. Nothing is removed until you confirm.'
          }</p>
        </div>`
            : nothing
        }
        ${repeat(
          this.groups.slice(0, this.visible),
          (group) => group.files[0]?.item.id,
          (group, index) => this.renderGroup(group, index),
        )}
        ${
          this.visible < this.groups.length
            ? html`<div data-load-more class="flex justify-center py-4">
          <button class="btn btn-sm btn-ghost" @click=${() => {
            this.visible += PAGE_SIZE;
          }}>Load more groups</button>
        </div>`
            : nothing
        }
      </div>
      <footer class="flex flex-none flex-wrap items-center gap-3 border-t border-base-300 bg-base-100 p-3">
        <span class="text-sm" role="status">${this.groups.length} groups · ${this.keep.size} to keep · ${total - this.keep.size} to remove</span>
        <label class="flex items-center gap-2 text-sm">Thumbnail size
          <input class="range range-xs w-28" type="range" min="160" max="480" step="20" .value=${String(this.size)}
            aria-label="Duplicate thumbnail size" @input=${(event: Event) => {
              this.size = Number((event.target as HTMLInputElement).value);
            }}>
        </label>
        <div class="ml-auto flex flex-wrap gap-2">
          ${run?.result ? html`<button class="btn btn-sm btn-ghost" @click=${() => openPlanResult(run)}>Execution details</button>` : nothing}
          <button class="btn btn-sm btn-outline" ?disabled=${!actionable} @click=${() => {
            this.automatic = true;
          }}>Automatic Selection</button>
          <button class="btn btn-sm btn-error" ?disabled=${!actionable || total === this.keep.size} @click=${() => this.trash()}>Trash ${total - this.keep.size} duplicates</button>
        </div>
      </footer>
      <opm-folder-picker .open=${this.picking !== null} .initial=${this.picking === 'source' ? this.source : { volume_id: null, dir: '', recursive: true }}
        .plan=${this.picking === 'source' && this.plan && !this.stale ? { plan_id: this.plan.id, side: 'source' } : null}
        .heading=${this.picking === 'source' ? 'Select source folder' : 'Exclude folder and subfolders'}
        @folder-picked=${(event: CustomEvent<Scope>) => {
          if (this.picking === 'source') {
            this.source = { ...event.detail, recursive: this.source.recursive };
            this.initialized = true;
          } else if (
            !this.excluded.some(
              (folder) =>
                folder.volume_id === event.detail.volume_id && folder.dir === event.detail.dir,
            )
          )
            this.excluded = [...this.excluded, { ...event.detail, recursive: true }];
          this.invalidate();
        }} @close=${() => {
          this.picking = null;
        }}></opm-folder-picker>
      <opm-duplicate-selection .open=${this.automatic} .rules=${this.rules} .tags=${tags}
        @selection-rules=${(event: CustomEvent<SelectionRule[]>) => this.applyRules(event.detail)}
        @close=${() => {
          this.automatic = false;
        }}></opm-duplicate-selection>
      ${
        this.context
          ? html`<ul class="menu fixed z-50 w-64 rounded-box border border-base-300 bg-base-100 p-2 shadow-lg" data-duplicate-menu
        style=${styleMap({ left: `${Math.max(0, this.context.x)}px`, top: `${Math.max(0, this.context.y)}px` })}>
        <li><button @click=${() => {
          if (this.context) this.preferFolder(this.context.file);
        }}>Keep files from this folder</button></li>
      </ul>`
          : nothing
      }
      <opm-modal .open=${this.folderFile !== null} heading="Keep files from this folder"
        @close=${() => {
          this.folderFile = null;
        }}
        .body=${html`<label class="block text-sm">Folder
          <select class="select mt-2 w-full" aria-label="Preferred folder or parent" .value=${String(this.folderIndex)}
            @change=${(event: Event) => {
              this.folderIndex = Number((event.target as HTMLSelectElement).value);
            }}>
            ${folderChoices.map((folder, index) => html`<option value=${String(index)}>${this.folderLabel(folder)}</option>`)}
          </select></label>
          <label class="mt-4 flex items-center gap-2"><input class="radio radio-sm" type="radio" name="duplicate-folder-depth" .checked=${!this.folderRecursive}
            @change=${() => {
              this.folderRecursive = false;
            }}>Only this specific folder</label>
          <label class="mt-3 flex items-center gap-2"><input class="radio radio-sm" type="radio" name="duplicate-folder-depth" .checked=${this.folderRecursive}
            @change=${() => {
              this.folderRecursive = true;
            }}>This folder and all subfolders</label>
          <p class="mt-4 text-sm text-base-content/70">Adds this folder as the highest-priority automatic rule and applies it to every group. Other rules break ties.</p>`}
        .actions=${html`<button class="btn btn-ghost" @click=${() => {
          this.folderFile = null;
        }}>Cancel</button>
          <button class="btn btn-primary" @click=${() => {
            const folder = folderChoices[this.folderIndex];
            if (!folder || this.stale) return;
            const kind = this.folderRecursive ? 'subtree' : 'folder';
            const rule = this.rules.find((entry) => entry.kind === kind);
            if (rule)
              this.applyRules([
                { ...rule, enabled: true, folders: [folder] },
                ...this.rules.filter((entry) => entry.kind !== kind),
              ]);
            this.folderFile = null;
          }}>Apply to all groups</button>`}></opm-modal>
    </section>`;
  }
}

customElements.define('opm-duplicate-files', OpmDuplicateFiles);
declare global {
  interface HTMLElementTagNameMap {
    'opm-duplicate-files': OpmDuplicateFiles;
  }
}
