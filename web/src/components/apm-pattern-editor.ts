import { html, nothing, type PropertyValues } from 'lit';
import { live } from 'lit/directives/live.js';
import { errorMessage, isAbortError } from '@/api/errors';
import { fetchFormatTokens, previewFormat } from '@/api/format';
import type { FormatPreviewResponse } from '@/api/generated/FormatPreviewResponse';
import type { FormatToken } from '@/api/generated/FormatToken';
import type { FormatTokensResponse } from '@/api/generated/FormatTokensResponse';
import type { SideParams } from '@/api/generated/SideParams';
import { ApmElement } from '@/base/apm-element';
import '@/components/apm-icon';
import { skipLabel } from '@/util/skip-codes';

export interface PatternChangeDetail {
  value: string;
  status: 'checking' | 'valid' | 'invalid' | 'idle';
}
export type PatternChangeEvent = CustomEvent<PatternChangeDetail>;

/** Converts the formatter's UTF-8 byte offsets to the DOM's UTF-16 selection offsets. */
export function patternSpan(text: string, start: number, end: number): [number, number] {
  const encoder = new TextEncoder();
  let bytes = 0;
  let units = 0;
  let first = 0;
  let last = 0;
  for (const char of text) {
    if (bytes < start) first = units + char.length;
    if (bytes < end) last = units + char.length;
    bytes += encoder.encode(char).length;
    units += char.length;
  }
  return [first, last];
}

let nextId = 0;

export class ApmPatternEditor extends ApmElement {
  static override properties = {
    value: { type: String },
    source: { attribute: false },
    basePrefix: { attribute: false },
    disabled: { type: Boolean },
    label: { type: String },
    limit: { type: Number },
    catalogue: { state: true },
    catalogueError: { state: true },
    preview: { state: true },
    previewError: { state: true },
    checking: { state: true },
  };
  value = '';
  source: SideParams | null = null;
  basePrefix: string | null = null;
  disabled = false;
  label = 'Folder pattern';
  limit = 5;
  private catalogue: FormatTokensResponse | null = null;
  private catalogueError = '';
  private preview: FormatPreviewResponse | null = null;
  private previewError = '';
  private checking = false;
  private composing = false;
  private readonly inputId = `pattern-${++nextId}`;
  private timer: number | undefined;
  private request: AbortController | null = null;
  private generation = 0;
  private key = '';
  private caret: [number, number] | null = null;

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'min-w-0');
    this.key = '';
    void this.loadCatalogue();
    this.requestUpdate();
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.cancelPreview();
  }

  protected override willUpdate(_changed: PropertyValues): void {
    const key = JSON.stringify([this.value, this.source, this.limit, this.disabled]);
    if (this.key !== key) {
      this.key = key;
      this.schedule();
    }
  }

  protected override updated(): void {
    if (this.caret) {
      const input = this.querySelector('input');
      input?.focus();
      input?.setSelectionRange(...this.caret);
      this.caret = null;
    }
  }

  private emit(value: string, status: PatternChangeDetail['status']): void {
    this.dispatchEvent(
      new CustomEvent<PatternChangeDetail>('pattern-change', {
        detail: { value, status },
        bubbles: true,
      }),
    );
  }

  private cancelPreview(): void {
    clearTimeout(this.timer);
    this.request?.abort();
    this.request = null;
    ++this.generation;
  }

  private schedule(): void {
    this.cancelPreview();
    this.previewError = '';
    if (this.disabled || this.composing || !this.source) {
      this.checking = false;
      this.preview = null;
      if (!this.composing) this.emit(this.value, 'idle');
      return;
    }
    this.checking = true;
    this.emit(this.value, 'checking');
    const generation = this.generation;
    this.timer = window.setTimeout(() => void this.check(generation), 300);
  }

  private async check(generation: number): Promise<void> {
    if (!this.source || !this.isConnected) return;
    const request = new AbortController();
    this.request = request;
    const value = this.value;
    try {
      const preview = await previewFormat(
        {
          pattern: value,
          source: this.source,
          limit: this.limit,
        },
        request.signal,
      );
      if (generation !== this.generation || !this.isConnected) return;
      this.preview = preview;
      this.emit(value, preview.error ? 'invalid' : 'valid');
    } catch (error) {
      if (generation !== this.generation || !this.isConnected || isAbortError(error)) return;
      this.previewError = errorMessage(error, 'Could not check the pattern');
      this.preview = null;
      this.emit(value, 'idle');
    } finally {
      if (generation === this.generation) {
        this.checking = false;
        this.request = null;
      }
    }
  }

  private async loadCatalogue(): Promise<void> {
    this.catalogueError = '';
    try {
      const catalogue = await fetchFormatTokens();
      if (this.isConnected) this.catalogue = catalogue;
    } catch (error) {
      if (this.isConnected) this.catalogueError = errorMessage(error, 'Variables unavailable');
    }
  }

  private edit(value: string, caret?: [number, number]): void {
    if (this.disabled || this.composing) return;
    this.cancelPreview();
    this.checking = this.source !== null;
    this.caret = caret ?? [value.length, value.length];
    this.emit(value, this.source ? 'checking' : 'idle');
    this.requestUpdate();
    // The controlled property wins, even if the host rejects an edit.
    queueMicrotask(() => {
      if (this.isConnected) this.schedule();
    });
  }

  private insert(token: string): void {
    const input = this.querySelector('input');
    const start = input?.selectionStart ?? this.value.length;
    const end = input?.selectionEnd ?? start;
    const value = this.value.slice(0, start) + token + this.value.slice(end);
    this.edit(value, [start + token.length, start + token.length]);
  }

  private showIssue(): void {
    const issue = this.preview?.error;
    const input = this.querySelector('input');
    if (!issue || !input) return;
    input.focus();
    input.setSelectionRange(...patternSpan(this.value, issue.start, issue.end));
  }

  private useSuggestion(): void {
    const issue = this.preview?.error;
    if (!issue?.suggestion) return;
    const [start, end] = patternSpan(this.value, issue.start, issue.end);
    const value = this.value.slice(0, start) + issue.suggestion + this.value.slice(end);
    this.edit(value, [start + issue.suggestion.length, start + issue.suggestion.length]);
  }

  protected override render() {
    const issue = this.checking ? null : this.preview?.error;
    const groups = new Map<string, FormatToken[]>();
    for (const token of this.catalogue?.tokens ?? []) {
      const group = groups.get(token.group) ?? [];
      group.push(token);
      groups.set(token.group, group);
    }
    return html`<div class="flex min-w-0 flex-col gap-1 text-xs">
      <label class="input input-sm w-full ${issue ? 'input-error validator' : ''}" for=${this.inputId}>
        ${this.basePrefix ? html`<span class="label max-w-40 truncate" title=${this.basePrefix}>${this.basePrefix}</span>` : nothing}
        <input id=${this.inputId} class="font-mono min-w-0" aria-label=${this.label}
          .value=${live(this.value)} ?disabled=${this.disabled} spellcheck="false" autocapitalize="off"
          aria-invalid=${issue ? 'true' : 'false'} aria-describedby="${this.inputId}-hint"
          @input=${(event: InputEvent) => {
            if (!this.composing && !event.isComposing) {
              const input = event.target as HTMLInputElement;
              this.edit(input.value, [input.selectionStart ?? 0, input.selectionEnd ?? 0]);
            }
          }}
          @compositionstart=${() => {
            this.composing = true;
            this.cancelPreview();
          }}
          @compositionend=${(event: CompositionEvent) => {
            this.composing = false;
            this.edit((event.target as HTMLInputElement).value);
          }} />
      </label>
      <div id="${this.inputId}-hint" class=${issue ? 'validator-hint mt-0 text-error' : 'text-base-content/70'}>
        ${
          issue
            ? html`<span role="alert">${issue.message}</span>
          <button class="btn btn-ghost btn-xs" type="button" @click=${() => this.showIssue()}>Show</button>
          ${
            issue.suggestion
              ? html`<button class="btn btn-ghost btn-xs" type="button" ?disabled=${this.disabled}
            @click=${() => this.useSuggestion()}>Use ${issue.suggestion}</button>`
              : nothing
          }`
            : nothing
        }
        ${!this.source ? 'Pick a source folder to preview' : nothing}
        ${!this.checking && this.preview?.appends_filename ? 'No file name in the pattern: the original name is added at the end' : nothing}
      </div>
      <details class="dropdown block min-w-0">
        <summary class="btn btn-ghost btn-xs w-full justify-between">Variables and examples
          <apm-icon name="chevron-down" size="xs"></apm-icon></summary>
        <div class="dropdown-content left-0 z-30 w-full rounded-box bg-base-100 p-2 shadow-lg">
          ${
            this.catalogueError
              ? html`<div class="alert alert-warning py-1" role="alert">
            ${this.catalogueError}<button class="btn btn-xs" type="button" @click=${() => this.loadCatalogue()}>Retry</button></div>`
              : nothing
          }
          ${!this.catalogue && !this.catalogueError ? html`<div class="flex gap-1"><span class="skeleton h-5 w-16"></span><span class="skeleton h-5 w-16"></span></div>` : nothing}
          ${[...groups].map(
            ([group, tokens]) => html`<div class="flex flex-wrap items-center gap-1 py-0.5">
            <span class="w-12 font-medium">${group}</span>
            ${tokens.map(
              (
                token,
              ) => html`<span class="tooltip" data-tip="${token.description} (${token.example})">
              <button class="btn btn-xs font-mono" type="button" ?disabled=${this.disabled}
                title="${token.description} (${token.example})"
                @pointerdown=${(event: PointerEvent) => event.preventDefault()}
                @click=${(event: Event) => {
                  this.insert(`{${token.name}}`);
                  const details = (event.currentTarget as HTMLElement).closest('details');
                  if (details) details.open = false;
                }}>{${token.name}}</button></span>`,
            )}
          </div>`,
          )}
          <details class="dropdown">
            <summary class="btn btn-ghost btn-xs">Examples</summary>
            <ul class="dropdown-content menu bg-base-100 rounded-box z-20 w-72 p-2 shadow-lg">
              ${this.catalogue?.examples.map(
                (example) => html`<li><button type="button" ?disabled=${this.disabled}
                @click=${(event: Event) => {
                  this.edit(example.pattern);
                  const details = (event.target as HTMLElement).closest('details');
                  if (details) details.open = false;
                }}><span><code>${example.pattern}</code><br />${example.description}<br />${example.result}</span></button></li>`,
              )}
            </ul>
          </details>
        </div>
      </details>
      ${
        this.previewError
          ? html`<div role="alert" class="alert alert-warning py-1">
        Could not check the pattern: ${this.previewError}
        <button type="button" class="btn btn-xs" @click=${() => this.schedule()}>Retry</button></div>`
          : nothing
      }
      <div class="min-h-4" role="status" aria-live="polite">${this.checking ? html`<span class="loading loading-dots loading-xs"></span> Checking pattern...` : nothing}</div>
      ${
        this.preview && !this.preview.error
          ? html`<details class="dropdown block min-w-0">
        <summary class="btn btn-ghost btn-xs w-full justify-between">Live preview (${this.preview.samples.length} ${this.preview.samples.length === 1 ? 'sample' : 'samples'})
          <apm-icon name="chevron-down" size="xs"></apm-icon></summary>
        <div class="dropdown-content left-0 z-30 w-full rounded-box bg-base-100 p-2 shadow-lg">
          ${
            this.preview.samples.length === 0
              ? 'No files to preview in the selected source'
              : html`<table class="table table-xs table-fixed w-full">
            <thead><tr><th>Source</th><th>Destination</th><th>Date from</th></tr></thead>
            <tbody>${this.preview.samples.map(
              (sample) => html`<tr>
              <td class="break-all">${sample.source}</td>
              <td class="break-all">${sample.skip ? skipLabel(sample.skip) : `${this.basePrefix ?? ''}${sample.destination ?? ''}`}</td>
              <td>${sample.capture_src ? html`<span class="badge badge-ghost badge-xs">${sample.capture_src}</span>` : nothing}</td>
            </tr>`,
            )}</tbody></table>`
          }
        </div>
      </details>`
          : nothing
      }
    </div>`;
  }
}

customElements.define('apm-pattern-editor', ApmPatternEditor);
declare global {
  interface HTMLElementTagNameMap {
    'apm-pattern-editor': ApmPatternEditor;
  }
  interface GlobalEventHandlersEventMap {
    'pattern-change': PatternChangeEvent;
  }
}
