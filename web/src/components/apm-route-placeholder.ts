import { html, nothing } from 'lit';
import { ApmElement } from '@/base/apm-element';
import '@/components/apm-icon';
import type { IconName } from '@/components/apm-icon';

/**
 * Stand-in view for a route whose feature is not built yet. The router also uses it for the
 * "page not found" state, with an action link back to a page that exists.
 *
 * A feature replaces it by pointing its entry in `app/routes.ts` at the real view; this file
 * stays for the routes that are still empty. The shell owns the page's `<h1>`, so the heading
 * here is an `<h2>`.
 */
export class ApmRoutePlaceholder extends ApmElement {
  static override properties = {
    heading: { type: String },
    message: { type: String },
    icon: { type: String },
    actionHref: { type: String, attribute: 'action-href' },
    actionLabel: { type: String, attribute: 'action-label' },
  };

  declare heading: string;
  declare message: string;
  declare icon: IconName | undefined;
  declare actionHref: string;
  declare actionLabel: string;

  constructor() {
    super();
    this.heading = '';
    this.message = 'Coming soon';
    this.icon = undefined;
    this.actionHref = '';
    this.actionLabel = '';
  }

  protected override render() {
    return html`
      <div class="hero h-full min-h-0">
        <div class="hero-content flex-col text-center">
          ${
            this.icon
              ? html`<apm-icon name=${this.icon} size="xl" class="text-base-content/40"></apm-icon>`
              : nothing
          }
          ${this.heading ? html`<h2 class="text-2xl font-semibold">${this.heading}</h2>` : nothing}
          <p class="text-base-content/70">${this.message}</p>
          ${
            this.actionHref && this.actionLabel
              ? html`<a class="btn btn-primary" href=${this.actionHref}>${this.actionLabel}</a>`
              : nothing
          }
        </div>
      </div>
    `;
  }
}

customElements.define('apm-route-placeholder', ApmRoutePlaceholder);

declare global {
  interface HTMLElementTagNameMap {
    'apm-route-placeholder': ApmRoutePlaceholder;
  }
}
