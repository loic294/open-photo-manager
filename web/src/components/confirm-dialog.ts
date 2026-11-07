import { signal } from '@preact/signals-core';
import { html, type TemplateResult } from 'lit';
import { live } from 'lit/directives/live.js';
import { ApmElement } from '@/base/apm-element';
import '@/components/apm-icon';
import '@/components/apm-modal';

export interface ConfirmOptions {
  /** The heading, e.g. "Delete 12 files?". */
  title: string;
  /** What will happen. For destructive actions state the exact counts and sizes. */
  message: string | TemplateResult;
  /** Default `Confirm`. Name the action ("Delete 12 files") rather than "OK". */
  confirmLabel?: string;
  /** Default `Cancel`. */
  cancelLabel?: string;
  /** Destructive: red confirm button, warning alert, and focus starts on Cancel. */
  danger?: boolean;
}

interface Request extends ConfirmOptions {
  resolve: (confirmed: boolean) => void;
}

const MESSAGE_ID = 'apm-confirm-message';

const queue: Request[] = [];
const active = signal<Request | undefined>(undefined);
let host: ApmConfirmDialog | undefined;

/**
 * Asks the user to confirm and resolves with their answer: `true` for the confirm button,
 * `false` for Cancel, Esc, the X or a click outside.
 *
 *   if (!(await confirmDialog({
 *     title: 'Delete 12 files?',
 *     message: 'They are moved to the trash (48 MB).',
 *     confirmLabel: 'Delete 12 files',
 *     danger: true,
 *   }))) return;
 *
 * One shared dialog is mounted on first use. Calls made while one is showing wait their turn
 * (first come, first served).
 */
export function confirmDialog(options: ConfirmOptions): Promise<boolean> {
  return new Promise((resolve) => {
    ensureHost();
    queue.push({ ...options, resolve });
    pump();
  });
}

function ensureHost(): void {
  if (host?.isConnected) return;
  host = document.createElement('apm-confirm-dialog');
  document.body.append(host);
}

function pump(): void {
  if (active.peek() === undefined) active.value = queue.shift();
}

function settle(request: Request, confirmed: boolean): void {
  if (active.peek() !== request) return;
  request.resolve(confirmed);
  active.value = undefined;
  pump();
}

function cancelAll(): void {
  const pending = [active.peek(), ...queue.splice(0)];
  active.value = undefined;
  for (const request of pending) request?.resolve(false);
}

/**
 * The view of the shared confirm dialog. Not for direct use: call `confirmDialog()`.
 * Removing it from the page answers everything still waiting with `false`.
 */
export class ApmConfirmDialog extends ApmElement {
  /** What the latest render showed; undefined when it rendered the dialog closed. */
  private current: Request | undefined;
  /** Kept after closing so the content does not vanish during the closing animation. */
  private last: Request | undefined;
  private focused: Request | undefined;

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    cancelAll();
  }

  protected override updated(): void {
    if (!this.current || this.current === this.focused) return;
    this.focused = this.current;
    void this.focusPreferredButton();
  }

  private async focusPreferredButton(): Promise<void> {
    const modal = this.querySelector('apm-modal');
    await modal?.updateComplete;
    modal?.querySelector<HTMLElement>('[autofocus]')?.focus();
  }

  /**
   * The modal also fires `close` after this dialog closed it itself because a request was
   * answered, and that event can arrive after the next request was already queued. Only a
   * close while a request was on show is the user dismissing it.
   */
  private readonly onClose = (): void => {
    if (this.current) settle(this.current, false);
  };

  protected override render() {
    this.current = active.value;
    this.last = this.current ?? this.last;
    const request = this.last;
    return html`
      <apm-modal
        size="sm"
        .open=${live(this.current !== undefined)}
        .heading=${request?.title ?? ''}
        .body=${request ? this.renderMessage(request) : undefined}
        .actions=${request ? this.renderActions(request) : undefined}
        .describedBy=${MESSAGE_ID}
        @close=${this.onClose}
      ></apm-modal>
    `;
  }

  private renderMessage({ message, danger }: Request) {
    return danger
      ? html`<div class="alert alert-soft alert-warning">
          <apm-icon name="triangle-alert"></apm-icon>
          <span id=${MESSAGE_ID} class="whitespace-pre-line">${message}</span>
        </div>`
      : html`<p id=${MESSAGE_ID} class="whitespace-pre-line">${message}</p>`;
  }

  private renderActions(request: Request) {
    const { danger = false, cancelLabel = 'Cancel', confirmLabel = 'Confirm' } = request;
    return html`
      <button
        type="button"
        class="btn btn-ghost"
        ?autofocus=${danger}
        @click=${() => settle(request, false)}
      >
        ${cancelLabel}
      </button>
      <button
        type="button"
        class="btn ${danger ? 'btn-error' : 'btn-primary'}"
        ?autofocus=${!danger}
        @click=${() => settle(request, true)}
      >
        ${confirmLabel}
      </button>
    `;
  }
}

customElements.define('apm-confirm-dialog', ApmConfirmDialog);

declare global {
  interface HTMLElementTagNameMap {
    'apm-confirm-dialog': ApmConfirmDialog;
  }
}
