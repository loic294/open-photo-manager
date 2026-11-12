import { html } from 'lit';
import { OpmElement } from '@/base/opm-element';
import { browseTool } from '@/tools/browse/tool';
import '@/tools/framework/opm-tool-view';

/** The Browse route: the tool view rendering `browseTool`. */
export class OpmBrowsePage extends OpmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }

  protected override render() {
    return html`<opm-tool-view .config=${browseTool}></opm-tool-view>`;
  }
}

customElements.define('opm-browse-page', OpmBrowsePage);

declare global {
  interface HTMLElementTagNameMap {
    'opm-browse-page': OpmBrowsePage;
  }
}
