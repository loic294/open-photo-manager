import { html, nothing } from 'lit';
import { repeat } from 'lit/directives/repeat.js';
import { OpmElement } from '@/base/opm-element';
import '@/components/opm-icon';
import type { IconName } from '@/components/opm-icon';
import {
  dismissToast,
  holdToast,
  releaseToast,
  type Toast,
  type ToastKind,
  toasts,
} from '@/stores/toasts';

/** Tailwind needs complete class strings. */
const ALERT_CLASS: Record<ToastKind, string> = {
  success: 'alert-success',
  info: 'alert-info',
  warning: 'alert-warning',
  error: 'alert-error',
};

const ICON: Record<ToastKind, IconName> = {
  success: 'circle-check',
  info: 'info',
  warning: 'triangle-alert',
  error: 'circle-x',
};

/**
 * Renders the toasts of `@/stores/toasts` (DaisyUI `toast` + `alert`). The shell owns the single
 * instance; call `toast.success(...)` etc. from `@/components/toast` to show one.
 *
 * The container is a polite live region that is always present, so screen readers announce each
 * new toast; errors use `role="alert"` and are announced immediately. A toast stays while the
 * pointer or keyboard focus is on it.
 */
export class OpmToastHost extends OpmElement {
  protected override render() {
    return html`
      <div
        class="toast toast-end toast-bottom z-50"
        role="status"
        aria-live="polite"
        aria-atomic="false"
        aria-relevant="additions"
      >
        ${repeat(
          toasts.value,
          (item) => item.id,
          (item) => this.renderToast(item),
        )}
      </div>
    `;
  }

  private renderToast({ id, kind, message, action }: Toast) {
    return html`
      <div
        class="alert ${ALERT_CLASS[kind]} max-w-md"
        role=${kind === 'error' ? 'alert' : nothing}
        @mouseenter=${() => holdToast(id)}
        @mouseleave=${() => releaseToast(id)}
        @focusin=${() => holdToast(id)}
        @focusout=${() => releaseToast(id)}
      >
        <opm-icon name=${ICON[kind]}></opm-icon>
        <span class="min-w-0 break-words">${message}</span>
        ${
          action
            ? html`<button type="button" class="btn btn-sm" @click=${() => action.run()}>
          ${action.label}
        </button>`
            : nothing
        }
        <button
          type="button"
          class="btn btn-ghost btn-xs btn-circle"
          aria-label="Dismiss notification"
          @click=${() => dismissToast(id)}
        >
          <opm-icon name="x" size="sm"></opm-icon>
        </button>
      </div>
    `;
  }
}

customElements.define('opm-toast-host', OpmToastHost);

declare global {
  interface HTMLElementTagNameMap {
    'opm-toast-host': OpmToastHost;
  }
}
