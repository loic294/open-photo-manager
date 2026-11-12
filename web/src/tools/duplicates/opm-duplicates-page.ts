import { html } from 'lit';
import { OpmElement } from '@/base/opm-element';
import { duplicatesTool } from '@/tools/duplicates/tool';
import '@/tools/framework/opm-tool-view';

export class OpmDuplicatesPage extends OpmElement {
  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('block', 'h-full', 'min-h-0');
  }

  protected override render() {
    return html`<opm-tool-view .config=${duplicatesTool}></opm-tool-view>`;
  }
}

customElements.define('opm-duplicates-page', OpmDuplicatesPage);

declare global {
  interface HTMLElementTagNameMap {
    'opm-duplicates-page': OpmDuplicatesPage;
  }
}
