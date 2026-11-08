import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';
import { duplicatesTool } from '@/tools/duplicates/tool';
import '@/tools/framework/apm-tool-view';

export class ApmDuplicatesPage extends ApmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }

  protected override render() {
    return html`<apm-tool-view .config=${duplicatesTool}></apm-tool-view>`;
  }
}

customElements.define('apm-duplicates-page', ApmDuplicatesPage);

declare global {
  interface HTMLElementTagNameMap {
    'apm-duplicates-page': ApmDuplicatesPage;
  }
}
