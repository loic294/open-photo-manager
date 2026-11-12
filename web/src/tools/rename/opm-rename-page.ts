import { html } from 'lit';
import { OpmElement } from '@/base/opm-element';
import '@/tools/framework/opm-tool-view';
import { renameTool } from '@/tools/rename/tool';

export class OpmRenamePage extends OpmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }
  protected override render() {
    return html`<opm-tool-view .config=${renameTool}></opm-tool-view>`;
  }
}
customElements.define('opm-rename-page', OpmRenamePage);
declare global {
  interface HTMLElementTagNameMap {
    'opm-rename-page': OpmRenamePage;
  }
}
