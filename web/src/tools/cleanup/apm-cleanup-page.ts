import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';
import { cleanupTool } from '@/tools/cleanup/tool';
import '@/tools/framework/apm-tool-view';

export class ApmCleanupPage extends ApmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }

  protected override render() {
    return html`<apm-tool-view .config=${cleanupTool}></apm-tool-view>`;
  }
}

customElements.define('apm-cleanup-page', ApmCleanupPage);

declare global {
  interface HTMLElementTagNameMap {
    'apm-cleanup-page': ApmCleanupPage;
  }
}
