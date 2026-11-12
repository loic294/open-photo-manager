import { html } from 'lit';
import { OpmElement } from '@/base/opm-element';
import '@/components/opm-icon';

export class OpmSaveBar extends OpmElement {
  static override properties = {
    invalid: { type: Boolean },
    saving: { type: Boolean },
    message: { type: String },
    invalidMessage: { type: String, attribute: 'invalid-message' },
  };

  invalid = false;
  saving = false;
  message = 'Unsaved changes';
  invalidMessage = 'Some values are not valid';

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('contents');
  }

  protected override render() {
    return html`
      <div class="navbar sticky bottom-4 z-10 min-h-12 rounded-box border border-base-300 bg-base-100 px-4 shadow-lg">
        <div class="navbar-start w-auto min-w-0 flex-1">
          <span role="status" class="flex items-center gap-2 text-sm">
            <opm-icon name=${this.invalid ? 'circle-alert' : 'info'} size="sm"></opm-icon>
            ${this.invalid ? this.invalidMessage : this.message}
          </span>
        </div>
        <div class="navbar-end w-auto flex-none gap-2">
          <button type="button" class="btn btn-ghost" ?disabled=${this.saving}
            @click=${() => this.dispatchEvent(new Event('discard', { bubbles: true }))}>Discard</button>
          <button type="button" class="btn btn-primary" ?disabled=${this.invalid || this.saving}
            @click=${() => this.dispatchEvent(new Event('save', { bubbles: true }))}>
            ${this.saving ? html`<span class="loading loading-spinner loading-xs"></span>` : html`<opm-icon name="save" size="sm"></opm-icon>`}
            Save
          </button>
        </div>
      </div>
    `;
  }
}

customElements.define('opm-save-bar', OpmSaveBar);
declare global {
  interface HTMLElementTagNameMap {
    'opm-save-bar': OpmSaveBar;
  }
}
