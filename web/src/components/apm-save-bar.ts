import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';
import '@/components/apm-icon';

export class ApmSaveBar extends ApmElement {
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
            <apm-icon name=${this.invalid ? 'circle-alert' : 'info'} size="sm"></apm-icon>
            ${this.invalid ? this.invalidMessage : this.message}
          </span>
        </div>
        <div class="navbar-end w-auto flex-none gap-2">
          <button type="button" class="btn btn-ghost" ?disabled=${this.saving}
            @click=${() => this.dispatchEvent(new Event('discard', { bubbles: true }))}>Discard</button>
          <button type="button" class="btn btn-primary" ?disabled=${this.invalid || this.saving}
            @click=${() => this.dispatchEvent(new Event('save', { bubbles: true }))}>
            ${this.saving ? html`<span class="loading loading-spinner loading-xs"></span>` : html`<apm-icon name="save" size="sm"></apm-icon>`}
            Save
          </button>
        </div>
      </div>
    `;
  }
}

customElements.define('apm-save-bar', ApmSaveBar);
declare global {
  interface HTMLElementTagNameMap {
    'apm-save-bar': ApmSaveBar;
  }
}
