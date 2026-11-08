import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';
import { browseTool } from '@/tools/browse/tool';
import '@/tools/framework/apm-tool-view';

/** The Browse route: the tool view rendering `browseTool`. */
export class ApmBrowsePage extends ApmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }

  protected override render() {
    return html`<apm-tool-view .config=${browseTool}></apm-tool-view>`;
  }
}

customElements.define('apm-browse-page', ApmBrowsePage);

declare global {
  interface HTMLElementTagNameMap {
    'apm-browse-page': ApmBrowsePage;
  }
}
