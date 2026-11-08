import { html, nothing } from 'lit';
import type { MatchMode } from '@/api/generated/MatchMode';
import { ApmElement } from '@/base/apm-element';
import type { SelectChangeEvent } from '@/components/apm-select';
import '@/components/apm-select';
import { isMatchMode, MATCH_MODES } from '@/util/match-modes';

export interface MatchChangeDetail {
  mode: MatchMode;
  strict: boolean;
}

export type MatchChangeEvent = CustomEvent<MatchChangeDetail>;

export class ApmMatchMode extends ApmElement {
  static override properties = {
    mode: { type: String },
    strict: { type: Boolean },
    label: { type: String },
    disabled: { type: Boolean },
  };

  mode: MatchMode = 'hash';
  strict = false;
  label = 'Match by';
  disabled = false;

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('inline-flex', 'flex-wrap', 'items-center', 'gap-3', 'max-w-full');
  }

  private emit(mode: MatchMode, strict: boolean): void {
    if (this.disabled) return;
    this.dispatchEvent(
      new CustomEvent<MatchChangeDetail>('match-change', {
        detail: { mode, strict },
        bubbles: true,
      }),
    );
    this.requestUpdate();
  }

  protected override updated(): void {
    const toggle = this.querySelector('input');
    if (toggle) toggle.checked = this.strict;
  }

  protected override render() {
    return html`
      <apm-select
        class="max-w-full"
        .options=${MATCH_MODES}
        .value=${this.mode}
        .label=${this.label}
        show-label
        size="sm"
        ?disabled=${this.disabled}
        @select-change=${(event: SelectChangeEvent) => {
          event.stopPropagation();
          if (isMatchMode(event.detail.value)) this.emit(event.detail.value, this.strict);
        }}
      ></apm-select>
      ${
        this.mode === 'name_prefix'
          ? html`<label
              class="label gap-2 text-base-content"
              title="Also match when the next character continues a number or word, so IMG_0001 matches IMG_00012"
            >
              <input
                type="checkbox"
                class="toggle toggle-sm"
                .checked=${this.strict}
                ?disabled=${this.disabled}
                @change=${(event: Event) => {
                  event.stopPropagation();
                  this.emit(this.mode, (event.currentTarget as HTMLInputElement).checked);
                }}
              />
              <span>Strict prefix</span>
            </label>`
          : nothing
      }
    `;
  }
}

customElements.define('apm-match-mode', ApmMatchMode);

declare global {
  interface HTMLElementTagNameMap {
    'apm-match-mode': ApmMatchMode;
  }
  interface GlobalEventHandlersEventMap {
    'match-change': MatchChangeEvent;
  }
}
