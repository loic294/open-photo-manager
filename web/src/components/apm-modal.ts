import { html, nothing, type PropertyValues, type TemplateResult } from 'lit';
import { createRef, ref } from 'lit/directives/ref.js';
import { ApmElement } from '@/base/apm-element';
import '@/components/apm-icon';

/** Complete class strings for Tailwind. `md` is DaisyUI's own default box width (32rem). */
const SIZE_CLASS = {
  sm: 'max-w-sm',
  md: 'max-w-lg',
  lg: 'max-w-3xl',
  xl: 'max-w-5xl',
  full: 'h-full w-full max-h-none max-w-none rounded-none',
} as const;

export type ModalSize = keyof typeof SIZE_CLASS;

/** There are no slots in light DOM: content is passed as templates (or plain text). */
export type ModalContent = TemplateResult | string;

let headingCounter = 0;

/**
 * A modal dialog: a native `<dialog class="modal">` (DaisyUI) opened with `showModal()`, so the
 * rest of the page is inert, Tab stays inside and the browser handles Esc.
 *
 * Declarative: keep `open` in your own state and listen for `close`.
 *
 *   html`<apm-modal
 *     .open=${this.renaming}
 *     heading="Rename folder"
 *     .body=${html`<input class="input w-full" aria-label="Name" />`}
 *     .actions=${html`<button class="btn btn-primary" @click=${this.save}>Save</button>`}
 *     @close=${() => (this.renaming = false)}
 *   ></apm-modal>`
 *
 * Imperative: create the element, set `heading`/`body`/`actions`, append it and call `show()`.
 *
 * Esc, a click on the backdrop and the X button all close it (`open` becomes false, then `close`
 * fires, once, however it was closed) unless `persistent` is set. Focus returns to the element
 * that had it before opening. `describedBy` is the id of an element inside `body` that screen
 * readers should read with the dialog (use it for confirmations).
 *
 * Errors of an action running inside the modal belong in the modal (a DaisyUI `alert`): toasts
 * are not shown above an open modal.
 */
export class ApmModal extends ApmElement {
  static override properties = {
    open: { type: Boolean, reflect: true },
    heading: { type: String },
    body: { attribute: false },
    actions: { attribute: false },
    size: { type: String },
    persistent: { type: Boolean },
    describedBy: { type: String, attribute: 'describedby' },
  };

  declare open: boolean;
  declare heading: string;
  declare body: ModalContent | undefined;
  declare actions: ModalContent | undefined;
  declare size: ModalSize;
  /** When set, Esc, the backdrop and the X button do nothing: the content must offer a way out. */
  declare persistent: boolean;
  declare describedBy: string | undefined;

  private readonly dialogRef = createRef<HTMLDialogElement>();
  private readonly headingId = `apm-modal-heading-${++headingCounter}`;
  private opener: HTMLElement | null = null;

  constructor() {
    super();
    this.open = false;
    this.heading = '';
    this.size = 'md';
    this.persistent = false;
  }

  /** Opens the modal. Resolves once it is on screen. */
  async show(): Promise<void> {
    this.open = true;
    await this.updateComplete;
  }

  /** Closes the modal. Resolves once it is closed. */
  async close(): Promise<void> {
    this.open = false;
    await this.updateComplete;
  }

  protected override updated(changed: PropertyValues<this>): void {
    const dialog = this.dialogRef.value;
    if (!dialog) return;

    if (this.open && !dialog.open) {
      this.opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      dialog.showModal();
    } else if (!this.open && dialog.open) {
      dialog.close();
      this.restoreFocus();
    }

    if (changed.has('open') && changed.get('open') === true && !this.open) {
      this.dispatchEvent(new Event('close'));
    }
  }

  /** The browser already does this on close; this covers a lost focus (e.g. the opener moved). */
  private restoreFocus(): void {
    const opener = this.opener;
    this.opener = null;
    const active = document.activeElement;
    if (
      opener?.isConnected &&
      (active === null || active === document.body || this.contains(active))
    ) {
      opener.focus();
    }
  }

  private readonly dismiss = (): void => {
    if (!this.persistent) this.open = false;
  };

  private readonly onCancel = (event: Event): void => {
    // Always take over: a native close would skip `persistent` and our single `close` event.
    event.preventDefault();
    this.dismiss();
  };

  private readonly onNativeClose = (): void => {
    // The dialog closed by itself (e.g. the browser's close request). Ignore a stale event
    // that arrives after it was opened again.
    if (this.dialogRef.value?.open === false) this.open = false;
  };

  private readonly onBackdropClick = (event: Event): void => {
    event.preventDefault();
    this.dismiss();
  };

  protected override render() {
    const closable = !this.persistent;
    return html`
      <dialog
        ${ref(this.dialogRef)}
        class="modal"
        aria-labelledby=${this.heading ? this.headingId : nothing}
        aria-describedby=${this.describedBy || nothing}
        @cancel=${this.onCancel}
        @close=${this.onNativeClose}
      >
        <div class="modal-box ${SIZE_CLASS[this.size] ?? SIZE_CLASS.md}">
          ${
            this.heading || closable
              ? html`<div class="flex items-start justify-between gap-4">
                  <h3 id=${this.headingId} class="text-lg font-bold">${this.heading}</h3>
                  ${
                    closable
                      ? html`<button
                          type="button"
                          class="btn btn-sm btn-circle btn-ghost ml-auto"
                          aria-label="Close"
                          @click=${this.dismiss}
                        >
                          <apm-icon name="x" size="sm"></apm-icon>
                        </button>`
                      : nothing
                  }
                </div>`
              : nothing
          }
          ${this.body ? html`<div class="py-4">${this.body}</div>` : nothing}
          ${this.actions ? html`<div class="modal-action">${this.actions}</div>` : nothing}
        </div>
        ${
          closable
            ? html`<form method="dialog" class="modal-backdrop">
                <button type="submit" tabindex="-1" aria-hidden="true" @click=${this.onBackdropClick}>
                  close
                </button>
              </form>`
            : nothing
        }
      </dialog>
    `;
  }
}

customElements.define('apm-modal', ApmModal);

declare global {
  interface HTMLElementTagNameMap {
    'apm-modal': ApmModal;
  }
}
