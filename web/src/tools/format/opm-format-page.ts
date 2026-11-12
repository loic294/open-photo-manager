import { html } from 'lit';
import { OpmElement } from '@/base/opm-element';
import '@/tools/framework/opm-tool-view';
import { formatTool } from '@/tools/format/tool';

export class OpmFormatPage extends OpmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }
  protected override render() {
    return html`<opm-tool-view .config=${formatTool}></opm-tool-view>`;
  }
}
customElements.define('opm-format-page', OpmFormatPage);
declare global {
  interface HTMLElementTagNameMap {
    'opm-format-page': OpmFormatPage;
  }
}
