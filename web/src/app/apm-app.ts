import { html } from 'lit';
import { ApmElement } from '@/base/apm-element';

export class ApmApp extends ApmElement {
  protected override render() {
    return html`
      <main class="min-h-screen grid place-items-center bg-base-200">
        <div class="card bg-base-100 shadow-xl">
          <div class="card-body items-center text-center">
            <h1 class="card-title">Advanced Photo Manager</h1>
            <p>The frontend scaffold is running.</p>
            <div class="card-actions">
              <button type="button" class="btn btn-primary">Get started</button>
            </div>
          </div>
        </div>
      </main>
    `;
  }
}

customElements.define('apm-app', ApmApp);

declare global {
  interface HTMLElementTagNameMap {
    'apm-app': ApmApp;
  }
}
