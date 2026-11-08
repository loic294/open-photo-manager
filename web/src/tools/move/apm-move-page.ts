import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';
import '@/tools/framework/apm-tool-view';
import { moveTool } from '@/tools/move/tool';

export class ApmMovePage extends ApmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }
  protected override render() {
    return html`<apm-tool-view .config=${moveTool}></apm-tool-view>`;
  }
}
customElements.define('apm-move-page', ApmMovePage);
declare global {
  interface HTMLElementTagNameMap {
    'apm-move-page': ApmMovePage;
  }
}
