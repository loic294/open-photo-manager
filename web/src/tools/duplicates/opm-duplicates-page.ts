import { html } from 'lit';
import { cache } from 'lit/directives/cache.js';
import { OpmElement } from '@/base/opm-element';
import { duplicatesTool } from '@/tools/duplicates/tool';
import '@/components/opm-icon';
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
    const viewControls = html`<div class="join" role="group" aria-label="Duplicate view">
          <button class="btn btn-sm join-item ${this.fileView ? 'btn-ghost' : 'btn-primary'}"
            aria-pressed=${!this.fileView} @click=${() => {
              this.fileView = false;
            }}><opm-icon name="folder-tree" size="sm"></opm-icon>Folder view</button>
          <button class="btn btn-sm join-item ${this.fileView ? 'btn-primary' : 'btn-ghost'}"
            aria-pressed=${this.fileView} @click=${() => {
              this.fileView = true;
            }}><opm-icon name="images" size="sm"></opm-icon>File view</button>
        </div>`;
    return html`<div class="flex h-full min-h-0 flex-col">
      <div class="min-h-0 flex-1">${cache(
        this.fileView
          ? html`<opm-duplicate-files class="block h-full min-h-0" .viewControls=${viewControls}></opm-duplicate-files>`
          : html`<opm-tool-view .config=${duplicatesTool} .leadingControls=${viewControls}></opm-tool-view>`,
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
