import { html } from 'lit';
import { OpmElement } from '@/base/opm-element';
import '@/tools/framework/opm-tool-view';
import { moveTool } from '@/tools/move/tool';

export class OpmMovePage extends OpmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }
  protected override render() {
    return html`<opm-tool-view .config=${moveTool}></opm-tool-view>`;
  }
}
customElements.define('opm-move-page', OpmMovePage);
declare global {
  interface HTMLElementTagNameMap {
    'opm-move-page': OpmMovePage;
  }
}
