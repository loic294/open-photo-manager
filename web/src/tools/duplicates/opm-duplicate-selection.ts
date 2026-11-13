import { html, nothing, type PropertyValues } from 'lit';
import { repeat } from 'lit/directives/repeat.js';
import type { Scope } from '@/api/generated/Scope';
import { OpmElement } from '@/base/opm-element';
import '@/components/opm-folder-picker';
import '@/components/opm-modal';
import { volumes } from '@/stores/volumes';
import { defaultRules, RULE_LABELS, type RuleKind, type SelectionRule } from './file-selection';

export class OpmDuplicateSelection extends OpmElement {
  static override properties = {
    open: { type: Boolean },
    rules: { attribute: false },
    tags: { attribute: false },
    draft: { state: true },
    folderRule: { state: true },
    announcement: { state: true },
  };
  open = false;
  rules = defaultRules();
  tags: string[] = [];
  private draft = defaultRules();
  private folderRule: RuleKind | null = null;
  private dragging: RuleKind | null = null;
  private announcement = '';

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has('open') && this.open) {
      this.draft = this.rules.map((rule) => ({ ...rule, folders: [...rule.folders] }));
      this.announcement = '';
    }
  }

  private change(kind: RuleKind, change: Partial<SelectionRule>): void {
    this.draft = this.draft.map((rule) => (rule.kind === kind ? { ...rule, ...change } : rule));
  }

  private move(kind: RuleKind, to: number): void {
    const from = this.draft.findIndex((rule) => rule.kind === kind);
    if (from < 0 || to < 0 || to >= this.draft.length || from === to) return;
    const draft = [...this.draft];
    const rule = draft.splice(from, 1)[0];
    if (!rule) return;
    draft.splice(to, 0, rule);
    this.draft = draft;
    this.announcement = `${RULE_LABELS[kind]} moved to priority ${to + 1}`;
  }

  private folderLabel(scope: Scope): string {
    return `${volumes.value.find((volume) => volume.id === scope.volume_id)?.name ?? 'All volumes'}/${scope.dir}`;
  }

  private close(): void {
    this.open = false;
    this.folderRule = null;
    this.dispatchEvent(new Event('close'));
  }

  protected override render() {
    const invalid = this.draft.some(
      (rule) =>
        rule.enabled &&
        ((rule.kind === 'tag' && !rule.tag) ||
          (['folder', 'subtree'].includes(rule.kind) && rule.folders.length === 0)),
    );
    return html`
      <opm-modal .open=${this.open} heading="Automatic Selection" size="lg" @close=${() => this.close()}
        .body=${html`
          <p class="mb-4 text-sm">Enable rules and drag them into priority order. The first rule wins; later rules break ties. Missing dates rank last. Exact ties keep the lowest file ID.</p>
          <ol class="max-h-[55vh] space-y-2 overflow-auto" aria-label="Selection rule priority">
            ${repeat(
              this.draft,
              (rule) => rule.kind,
              (rule, index) => html`
              <li class="rounded-box border border-base-300 p-3" draggable="true"
                @dragstart=${(event: DragEvent) => {
                  this.dragging = rule.kind;
                  event.dataTransfer?.setData('text/plain', rule.kind);
                  if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
                }}
                @dragover=${(event: DragEvent) => {
                  if (this.dragging) event.preventDefault();
                }}
                @drop=${(event: DragEvent) => {
                  event.preventDefault();
                  if (this.dragging) this.move(this.dragging, index);
                  this.dragging = null;
                }}
                @dragend=${() => {
                  this.dragging = null;
                }}>
                <div class="flex flex-wrap items-center gap-3">
                  <span class="cursor-grab text-base-content/70" aria-hidden="true">⠿</span>
                  <label class="flex min-w-0 flex-1 items-center gap-3">
                    <input type="checkbox" class="checkbox checkbox-sm" .checked=${rule.enabled}
                      @change=${(event: Event) => this.change(rule.kind, { enabled: (event.target as HTMLInputElement).checked })}>
                    <span>${RULE_LABELS[rule.kind]}</span>
                  </label>
                  <div class="join">
                    <button class="btn btn-xs btn-ghost join-item" aria-label=${`Move ${RULE_LABELS[rule.kind]} up`}
                      ?disabled=${index === 0} @click=${() => this.move(rule.kind, index - 1)}>↑</button>
                    <button class="btn btn-xs btn-ghost join-item" aria-label=${`Move ${RULE_LABELS[rule.kind]} down`}
                      ?disabled=${index === this.draft.length - 1} @click=${() => this.move(rule.kind, index + 1)}>↓</button>
                  </div>
                </div>
                ${
                  ['capture', 'modified', 'created', 'size'].includes(rule.kind)
                    ? html`
                  <select class="select select-sm mt-2 w-full" aria-label=${`${RULE_LABELS[rule.kind]} order`}
                    .value=${rule.direction} @change=${(event: Event) => this.change(rule.kind, { direction: (event.target as HTMLSelectElement).value === 'desc' ? 'desc' : 'asc' })}>
                    <option value="asc">${rule.kind === 'size' ? 'Smaller first' : 'Oldest first (ascending)'}</option>
                    <option value="desc">${rule.kind === 'size' ? 'Bigger first' : 'Newest first (descending)'}</option>
                  </select>`
                    : nothing
                }
                ${
                  rule.kind === 'tag'
                    ? html`
                  <label class="mt-2 block text-sm">Tag among matching files
                    <select class="select select-sm mt-1 w-full" aria-label="Preferred tag" .value=${rule.tag}
                      @change=${(event: Event) => this.change(rule.kind, { tag: (event.target as HTMLSelectElement).value })}>
                      <option value="">Select a tag</option>
                      ${this.tags.map((tag) => html`<option value=${tag}>${tag}</option>`)}
                    </select>
                  </label>`
                    : nothing
                }
                ${
                  ['folder', 'subtree'].includes(rule.kind)
                    ? html`
                  <div class="mt-2 flex flex-wrap items-center gap-2">
                    ${rule.folders.map(
                      (folder, folderIndex) => html`<button class="btn btn-xs"
                      aria-label=${`Remove ${this.folderLabel(folder)}`}
                      @click=${() => this.change(rule.kind, { folders: rule.folders.filter((_, i) => i !== folderIndex) })}>
                      ${this.folderLabel(folder)} ×</button>`,
                    )}
                    <button class="btn btn-sm btn-outline" @click=${() => {
                      this.folderRule = rule.kind;
                    }}>Add folder</button>
                  </div>`
                    : nothing
                }
              </li>`,
            )}
          </ol>
          <p class="sr-only" aria-live="polite">${this.announcement}</p>
          ${invalid ? html`<p class="mt-3 text-sm text-error" role="status">Choose a tag or folder for each enabled rule that requires one.</p>` : nothing}
          <p class="mt-3 text-sm text-base-content/70">Some name-based groups need several keepers: every removal must directly match a retained file. Applying rules replaces manual choices in all groups.</p>
        `}
        .actions=${html`<button class="btn btn-ghost" @click=${() => this.close()}>Cancel</button>
          <button class="btn btn-primary" ?disabled=${invalid} @click=${() => {
            this.dispatchEvent(
              new CustomEvent<SelectionRule[]>('selection-rules', {
                detail: this.draft,
                bubbles: true,
              }),
            );
            this.close();
          }}>Apply to all groups</button>`}>
      </opm-modal>
      <opm-folder-picker .open=${this.folderRule !== null} heading="Prefer files in folder"
        @close=${() => {
          this.folderRule = null;
        }}
        @folder-picked=${(event: CustomEvent<Scope>) => {
          const rule = this.draft.find((entry) => entry.kind === this.folderRule);
          if (
            rule &&
            !rule.folders.some(
              (folder) =>
                folder.volume_id === event.detail.volume_id && folder.dir === event.detail.dir,
            )
          ) {
            this.change(rule.kind, { folders: [...rule.folders, event.detail] });
          }
        }}></opm-folder-picker>`;
  }
}

customElements.define('opm-duplicate-selection', OpmDuplicateSelection);
declare global {
  interface HTMLElementTagNameMap {
    'opm-duplicate-selection': OpmDuplicateSelection;
  }
}
