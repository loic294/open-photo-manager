import { html } from 'lit';
import { cache } from 'lit/directives/cache.js';
import { OpmElement } from '@/base/opm-element';
import { duplicatesTool } from '@/tools/duplicates/tool';
import '@/tools/framework/opm-tool-view';
import '@/tools/duplicates/opm-duplicate-files';

export class OpmDuplicatesPage extends OpmElement {
  static override properties = { fileView: { state: true } };
  private fileView = false;
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }

  protected override render() {
    return html`<div class="flex h-full min-h-0 flex-col">
      <div class="flex-none border-b border-base-300 px-3 py-2">
        <div class="join" role="group" aria-label="Duplicate view">
          <button class="btn btn-sm join-item ${this.fileView ? 'btn-ghost' : 'btn-active'}"
            aria-pressed=${!this.fileView} @click=${() => {
              this.fileView = false;
            }}>Folder view</button>
          <button class="btn btn-sm join-item ${this.fileView ? 'btn-active' : 'btn-ghost'}"
            aria-pressed=${this.fileView} @click=${() => {
              this.fileView = true;
            }}>File view</button>
        </div>
      </div>
      <div class="min-h-0 flex-1">${cache(
        this.fileView
          ? html`<opm-duplicate-files class="block h-full min-h-0"></opm-duplicate-files>`
          : html`<opm-tool-view .config=${duplicatesTool}></opm-tool-view>`,
      )}</div>
    </div>`;
  }
}

customElements.define('opm-duplicates-page', OpmDuplicatesPage);

declare global {
  interface HTMLElementTagNameMap {
    'opm-duplicates-page': OpmDuplicatesPage;
  }
}
