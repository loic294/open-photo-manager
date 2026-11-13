import { html, nothing } from 'lit';
import { createRef, ref } from 'lit/directives/ref.js';
import { OpmElement } from '@/base/opm-element';
import { tooltip } from '@/components/tooltip';
import '@/components/opm-icon';

export interface ChipValuesDetail {
  values: string[];
}

/** Fired after every change of the list; `detail.values` is a copy the listener may keep. */
export type ChipValuesChange = CustomEvent<ChipValuesDetail>;

interface Message {
  text: string;
  /** `error` turns the field red; `info` is plain muted text. */
  tone: 'error' | 'info';
}

/**
 * What ends an entry besides Enter. A comma always does. Space and tab do too, unless entries
 * may contain spaces (file names): then only line breaks do.
 */
const ENDS_ENTRY = { words: /[\s,]+/, names: /[,\r\n]+/ } as const;

const sameEntryKey = (entry: string): string => entry.normalize('NFC').toLowerCase();

let instances = 0;

/**
 * A field that edits a list of short texts (file extensions, names to ignore) as chips: the label,
 * the chips with a remove button each, a text box that adds more and a line for the hint or the
 * error. It renders the whole field, so the page adds no label of its own.
 *
 *   html`<opm-chip-input
 *     label="Sidecar extensions"
 *     placeholder="xmp"
 *     hint="Without the dot."
 *     .values=${this.draft.sidecar_extensions}
 *     .max=${50}
 *     .normalizeEntry=${normalizeExtension}
 *     .validateEntry=${validateExtension}
 *     @values-change=${(e: ChipValuesChange) => this.setExtensions(e.detail.values)}
 *   ></opm-chip-input>`
 *
 * Enter, a comma (or a space, see `allowSpaces`) and leaving the text box add what was typed.
 * Pasting text that holds separators adds every entry in it. An entry goes through
 * `normalizeEntry`, is skipped when blank or already in the list (compared without case) and is
 * refused when `validateEntry` returns a reason or the list holds `max` entries: the refused
 * text stays in the box and the message line says why. Backspace in an empty box removes the last
 * chip.
 *
 * The list is the `values` property (never mutated: every change makes a new array). The change is
 * reported with the `values-change` event and the component also keeps the new list itself, so a
 * parent that does not feed it back sees no flicker. Mark entries the server refused with
 * `errorIndexes` and say why with `error`; `error` wins over the hint.
 */
export class OpmChipInput extends OpmElement {
  static override properties = {
    values: { attribute: false },
    label: { type: String },
    hint: { type: String },
    placeholder: { type: String },
    max: { type: Number },
    normalizeEntry: { attribute: false },
    validateEntry: { attribute: false },
    error: { type: String },
    errorIndexes: { attribute: false },
    allowSpaces: { type: Boolean, attribute: 'allow-spaces' },
    disabled: { type: Boolean, reflect: true },
    message: { state: true },
  };

  declare values: readonly string[];
  declare label: string;
  /** Muted help under the field, replaced by any message. */
  declare hint: string;
  /** Shown in the text box while the list is empty. */
  declare placeholder: string;
  /** The most entries the list may hold. */
  declare max: number;
  /** Turns the typed text into the entry to store. Default: trim. */
  declare normalizeEntry: (raw: string) => string;
  /** Why an (already normalised) entry cannot be added, phrased to follow it; `null` if it can. */
  declare validateEntry: (entry: string) => string | null;
  /** A problem reported from outside (the server). Empty when there is none. */
  declare error: string;
  /** Positions of the chips to mark as refused. */
  declare errorIndexes: readonly number[];
  /** Entries may contain spaces (file names): only a comma or Enter ends one. */
  declare allowSpaces: boolean;
  declare disabled: boolean;
  declare message: Message | null;

  private readonly inputRef = createRef<HTMLInputElement>();
  private readonly uid = `opm-chip-input-${++instances}`;

  constructor() {
    super();
    this.values = [];
    this.label = '';
    this.hint = '';
    this.placeholder = '';
    this.max = Number.POSITIVE_INFINITY;
    this.normalizeEntry = (raw) => raw.trim();
    this.validateEntry = () => null;
    this.error = '';
    this.errorIndexes = [];
    this.allowSpaces = false;
    this.disabled = false;
    this.message = null;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block');
  }

  /** Focuses the text box (what the label points at). */
  override focus(options?: FocusOptions): void {
    this.inputRef.value?.focus(options);
  }

  private get endsEntry(): RegExp {
    return this.allowSpaces ? ENDS_ENTRY.names : ENDS_ENTRY.words;
  }

  private emit(values: string[]): void {
    this.values = values;
    this.dispatchEvent(
      new CustomEvent<ChipValuesDetail>('values-change', {
        detail: { values: [...values] },
        bubbles: true,
        composed: true,
      }),
    );
  }

  /**
   * Adds the entries in `pieces`, which are raw text, and returns the pieces that were refused (as
   * typed), for the text box to keep. The first refusal becomes the message.
   */
  private add(pieces: readonly string[]): string[] {
    const next = [...this.values];
    const known = new Set(next.map(sameEntryKey));
    const refused: string[] = [];
    let problem: string | null = null;
    let duplicate: string | null = null;

    for (const piece of pieces) {
      const entry = this.normalizeEntry(piece);
      if (entry === '') continue;
      if (known.has(sameEntryKey(entry))) {
        duplicate ??= `"${entry}" is already in the list`;
        continue;
      }
      if (next.length >= this.max) {
        refused.push(piece.trim());
        problem ??= `At most ${this.max} entries are allowed`;
        continue;
      }
      const reason = this.validateEntry(entry);
      if (reason !== null) {
        refused.push(piece.trim());
        problem ??= `"${entry}" ${reason}`;
        continue;
      }
      next.push(entry);
      known.add(sameEntryKey(entry));
    }

    if (problem !== null) this.message = { text: problem, tone: 'error' };
    else if (duplicate !== null) this.message = { text: duplicate, tone: 'info' };
    else this.message = null;
    if (next.length > this.values.length) this.emit(next);
    return refused;
  }

  private removeChip(index: number): void {
    this.message = null;
    this.emit(this.values.filter((_, position) => position !== index));
    this.inputRef.value?.focus();
  }

  /** The text box keeps only what could not be added. */
  private commit(input: HTMLInputElement, text: string): void {
    input.value = this.add(text.split(this.endsEntry)).join(', ');
  }

  private readonly onInput = (event: Event): void => {
    if ((event as InputEvent).isComposing) return;
    const input = event.currentTarget as HTMLInputElement;
    this.message = null;
    const parts = input.value.split(this.endsEntry);
    if (parts.length < 2) return;
    // A separator was typed: what precedes it is complete, what follows starts the next entry.
    const unfinished = parts[parts.length - 1] ?? '';
    const refused = this.add(parts.slice(0, -1));
    input.value = [...refused, unfinished].filter((text) => text !== '').join(', ');
  };

  private readonly onKeydown = (event: KeyboardEvent): void => {
    if (event.isComposing) return;
    const input = event.currentTarget as HTMLInputElement;
    if (event.key === 'Enter') {
      // Enter adds the entry; it must never submit a form around the field.
      event.preventDefault();
      this.commit(input, input.value);
    } else if (event.key === 'Backspace' && input.value === '' && this.values.length > 0) {
      this.message = null;
      this.emit(this.values.slice(0, -1));
    }
  };

  private readonly onPaste = (event: ClipboardEvent): void => {
    const pasted = event.clipboardData?.getData('text') ?? '';
    // One entry without separators is left to the browser: it lands in the box like typing.
    if (!this.endsEntry.test(pasted)) return;
    event.preventDefault();
    const input = event.currentTarget as HTMLInputElement;
    const start = input.selectionStart ?? input.value.length;
    const end = input.selectionEnd ?? input.value.length;
    this.message = null;
    this.commit(input, input.value.slice(0, start) + pasted + input.value.slice(end));
  };

  private readonly onBlur = (event: FocusEvent): void => {
    const input = event.currentTarget as HTMLInputElement;
    if (input.value.trim() !== '') this.commit(input, input.value);
  };

  private readonly onFieldClick = (event: Event): void => {
    if (!(event.target as Element).closest('button')) this.inputRef.value?.focus();
  };

  protected override render() {
    const inputId = `${this.uid}-input`;
    const messageId = `${this.uid}-message`;
    const invalid = this.error !== '' || this.message?.tone === 'error';
    const text = this.error || this.message?.text || this.hint;
    const flagged = new Set(this.errorIndexes);

    return html`
      <div class="flex flex-col gap-1">
        ${
          this.label
            ? html`<label for=${inputId} class="text-sm font-medium">${this.label}</label>`
            : nothing
        }
        <div
          class="input h-auto min-h-10 w-full flex-wrap gap-1 py-1 ${invalid ? 'validator' : ''}"
          @click=${this.onFieldClick}
        >
          <ul class="contents" role="list" aria-label=${this.label || nothing}>
            ${this.values.map(
              (value, index) => html`
                <li
                  class="badge badge-soft h-7 min-w-0 max-w-full gap-1 pr-0.5 ${
                    flagged.has(index) ? 'badge-error' : ''
                  }"
                >
                  <span class="truncate" ${tooltip(value)}>${value}</span>
                  <button
                    type="button"
                    class="btn btn-ghost btn-circle btn-xs size-5 min-h-0 shrink-0"
                    aria-label="Remove ${value}"
                    ?disabled=${this.disabled}
                    @click=${() => this.removeChip(index)}
                  >
                    <opm-icon name="x" size="xs"></opm-icon>
                  </button>
                </li>
              `,
            )}
          </ul>
          <input
            ${ref(this.inputRef)}
            id=${inputId}
            type="text"
            class="w-auto min-w-24 flex-1"
            placeholder=${this.values.length === 0 && this.placeholder ? this.placeholder : nothing}
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            ?disabled=${this.disabled}
            aria-invalid=${invalid ? 'true' : nothing}
            aria-describedby=${text ? messageId : nothing}
            @input=${this.onInput}
            @keydown=${this.onKeydown}
            @paste=${this.onPaste}
            @blur=${this.onBlur}
          />
        </div>
        ${
          text
            ? html`<p
                id=${messageId}
                class=${invalid ? 'validator-hint mt-0' : 'text-xs text-base-content/70'}
                role=${invalid ? 'alert' : nothing}
              >${text}</p>`
            : nothing
        }
      </div>
    `;
  }
}

customElements.define('opm-chip-input', OpmChipInput);

declare global {
  interface HTMLElementTagNameMap {
    'opm-chip-input': OpmChipInput;
  }
}
