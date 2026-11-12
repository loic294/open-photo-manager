import { html } from 'lit';
import { OpmElement } from '@/base/opm-element';
import { cleanupTool } from '@/tools/cleanup/tool';
import '@/tools/framework/opm-tool-view';

export class OpmCleanupPage extends OpmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }

  protected override render() {
    return html`<opm-tool-view .config=${cleanupTool}></opm-tool-view>`;
  }
}

customElements.define('opm-cleanup-page', OpmCleanupPage);

declare global {
  interface HTMLElementTagNameMap {
    'opm-cleanup-page': OpmCleanupPage;
  }
}
