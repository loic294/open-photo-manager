import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';
import '@/tools/framework/apm-tool-view';
import { renameTool } from '@/tools/rename/tool';

export class ApmRenamePage extends ApmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }
  protected override render() {
    return html`<apm-tool-view .config=${renameTool}></apm-tool-view>`;
  }
}
customElements.define('apm-rename-page', ApmRenamePage);
declare global {
  interface HTMLElementTagNameMap {
    'apm-rename-page': ApmRenamePage;
  }
}
