import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';
import '@/tools/framework/apm-tool-view';
import { formatTool } from '@/tools/format/tool';

export class ApmFormatPage extends ApmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }
  protected override render() {
    return html`<apm-tool-view .config=${formatTool}></apm-tool-view>`;
  }
}
customElements.define('apm-format-page', ApmFormatPage);
declare global {
  interface HTMLElementTagNameMap {
    'apm-format-page': ApmFormatPage;
  }
}
