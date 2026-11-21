import { html } from 'lit';
import { classMap } from 'lit/directives/class-map.js';
import { OpmElement } from '@/base/opm-element';
import { localOnlyChanges } from '@/stores/prefs';
import '@/components/opm-icon';

/**
 * Marks subsequent file operations as local-only. The request layer forwards the preference to
 * the server, while this control keeps the mode visible beside the theme control.
 */
export class OpmLocalOnlyToggle extends OpmElement {
  private toggle() {
    localOnlyChanges.value = !localOnlyChanges.peek();
  }

  protected override render() {
    const active = localOnlyChanges.value;
    const state = active ? 'On' : 'Off';
    return html`
      <div
        class="tooltip tooltip-left"
        data-tip=${
          active
            ? 'Local only changes: on. Operations will not sync.'
            : 'Local only changes: off. Operations use normal sync.'
        }
      >
        <button
          type="button"
          class=${classMap({
            'btn btn-sm gap-2': true,
            'btn-primary': active,
            'btn-ghost': !active,
          })}
          aria-label=${`Local only changes: ${state}`}
          aria-pressed=${String(active)}
          @click=${this.toggle}
        >
          <opm-icon name=${active ? 'cloud-off' : 'cloud'}></opm-icon>
          <span class="hidden sm:inline">Local only changes</span>
        </button>
      </div>
    `;
  }
}

customElements.define('opm-local-only-toggle', OpmLocalOnlyToggle);

declare global {
  interface HTMLElementTagNameMap {
    'opm-local-only-toggle': OpmLocalOnlyToggle;
  }
}
