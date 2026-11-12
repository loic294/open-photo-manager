import { html, nothing, type PropertyValues } from 'lit';
import { createRef, ref } from 'lit/directives/ref.js';
import { OpmElement } from '@/base/opm-element';
import type { IconName } from '@/components/opm-icon';
import '@/components/opm-icon';
import { lastAction } from '@/stores/prefs';
import { formatCount } from '@/util/format-count';

/**
 * One entry of the split button. `danger` colours the main button red while the entry is
 * selected, `hint` is a tooltip on the menu entry, `disabled` greys the entry out.
 */
export interface SplitOption {
  id: string;
  label: string;
  icon?: IconName;
  danger?: boolean;
  disabled?: boolean;
  hint?: string;
}

export interface SplitDetail {
  optionId: string;
}

export type SplitActionEvent = CustomEvent<SplitDetail>;
export type SplitSelectEvent = CustomEvent<SplitDetail>;

let instanceCounter = 0;

/**
 * A compact split button for the action area of a tool: the main button runs the SELECTED entry, the
 * chevron next to it opens a menu (upwards) to select another one.
 *
 *   html`<opm-split-button
 *     persist-key="duplicates"
 *     default-id="delete-comparison"
 *     .options=${[
 *       { id: 'delete-source', label: 'Delete source items', icon: 'trash-2', danger: true },
 *       { id: 'move', label: 'Move files', icon: 'folder-input', hint: 'Moves the matches' },
 *     ]}
 *     .count=${matches}
 *     .busy=${running}
 *     @split-action=${(e: SplitActionEvent) => this.confirmAndRun(e.detail.optionId)}
 *   ></opm-split-button>`
 *
 * Events (both bubble, `detail` is `{ optionId }`):
 * - `split-action`: the main button was pressed. Only the main button ever triggers it.
 * - `split-select`: an entry was chosen in the menu. This only SELECTS it (the user then presses
 *   the main button); the actions are destructive, so the confirmation belongs to the host.
 *
 * The selected entry is remembered per `persistKey` in the `lastAction` preference
 * (`localStorage['opm.v1.lastAction']`, shared with `lastActionOf(tool)`): use the tool id. The
 * effective entry is the remembered one if it is enabled, else `defaultId`, else the first enabled
 * entry. A remembered entry that is disabled or gone is skipped WITHOUT forgetting it, so it
 * returns when it is enabled again. Without a `persistKey` the choice only lives as long as the
 * element. With no enabled entry the first one is shown and both buttons are disabled.
 *
 * `busy` shows a spinner and disables both buttons, `disabled` only disables them, `count` is a
 * badge in the main button. The menu closes on a choice, Escape (focus returns to the chevron), an
 * outside press and when focus leaves it; ArrowUp/ArrowDown/Home/End move between the entries.
 */
export class OpmSplitButton extends OpmElement {
  static override properties = {
    options: { attribute: false },
    defaultId: { type: String, attribute: 'default-id' },
    persistKey: { type: String, attribute: 'persist-key' },
    busy: { type: Boolean },
    disabled: { type: Boolean },
    count: { type: Number },
    menuOpen: { state: true },
    localChoice: { state: true },
  };

  declare options: SplitOption[];
  /** The entry to select when the user has not chosen one yet (or the chosen one is unusable). */
  declare defaultId: string | undefined;
  /** Key of the remembered choice, normally the tool id. */
  declare persistKey: string;
  declare busy: boolean;
  declare disabled: boolean;
  /** Shown as a badge in the main button, for example the number of matched items. */
  declare count: number | undefined;
  declare private menuOpen: boolean;
  declare private localChoice: string | undefined;

  private readonly uid = `opm-split-${++instanceCounter}`;
  private readonly detailsRef = createRef<HTMLDetailsElement>();
  private readonly summaryRef = createRef<HTMLElement>();
  private restoreFocus = false;

  constructor() {
    super();
    this.options = [];
    this.persistKey = '';
    this.busy = false;
    this.disabled = false;
    this.menuOpen = false;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    // Leave room for both pane footers' select-all controls on a narrow screen.
    this.classList.add('inline-flex', 'min-w-0', 'max-w-[calc(100vw-6rem)]');
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    document.removeEventListener('pointerdown', this.onOutsidePointerDown, true);
    this.menuOpen = false;
  }

  protected override willUpdate(): void {
    if (this.menuOpen && !this.canOpen()) this.menuOpen = false;
  }

  protected override updated(changed: PropertyValues): void {
    if (!changed.has('menuOpen')) return;
    if (this.menuOpen) {
      document.addEventListener('pointerdown', this.onOutsidePointerDown, true);
      this.querySelector<HTMLElement>('[role="menuitemradio"][aria-checked="true"]')?.focus();
    } else if (changed.get('menuOpen') === true) {
      document.removeEventListener('pointerdown', this.onOutsidePointerDown, true);
      if (this.restoreFocus) this.summaryRef.value?.focus();
      this.restoreFocus = false;
    }
  }

  private canOpen(): boolean {
    return !this.disabled && !this.busy && this.options.some((option) => !option.disabled);
  }

  /** The remembered, default or first entry. `usable` is false when no entry is enabled. */
  private resolve(): { option: SplitOption; usable: boolean } | undefined {
    const remembered = this.persistKey ? lastAction.value[this.persistKey] : this.localChoice;
    const enabled = this.options.filter((option) => !option.disabled);
    const pool = enabled.length > 0 ? enabled : this.options;
    const option =
      pool.find((candidate) => candidate.id === remembered) ??
      pool.find((candidate) => candidate.id === this.defaultId) ??
      pool[0];
    return option && { option, usable: enabled.length > 0 };
  }

  private remember(id: string): void {
    if (!this.persistKey) {
      this.localChoice = id;
      return;
    }
    const current = lastAction.peek();
    if (current[this.persistKey] !== id) {
      lastAction.value = { ...current, [this.persistKey]: id };
    }
  }

  private emit(type: 'split-action' | 'split-select', optionId: string): void {
    this.dispatchEvent(new CustomEvent<SplitDetail>(type, { detail: { optionId }, bubbles: true }));
  }

  private openMenu(): void {
    if (this.canOpen()) this.menuOpen = true;
  }

  private closeMenu(restoreFocus: boolean): void {
    if (!this.menuOpen) return;
    this.restoreFocus = restoreFocus;
    this.menuOpen = false;
  }

  private entries(): HTMLButtonElement[] {
    return Array.from(
      this.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]:not(:disabled)'),
    );
  }

  private moveFocus(step: 1 | -1 | 'first' | 'last'): void {
    const entries = this.entries();
    if (entries.length === 0) return;
    const active = document.activeElement;
    const current = active instanceof HTMLButtonElement ? entries.indexOf(active) : -1;
    let next: number;
    if (step === 'first') next = 0;
    else if (step === 'last') next = entries.length - 1;
    else if (current === -1) next = step === 1 ? 0 : entries.length - 1;
    else next = (current + step + entries.length) % entries.length;
    entries[next]?.focus();
  }

  private readonly onMainClick = (): void => {
    const resolved = this.resolve();
    if (!resolved?.usable || this.disabled || this.busy) return;
    this.emit('split-action', resolved.option.id);
  };

  private readonly onSummaryClick = (event: Event): void => {
    // The open state is ours (the browser must not toggle the details by itself).
    event.preventDefault();
    if (this.menuOpen) this.closeMenu(true);
    else this.openMenu();
  };

  private choose(option: SplitOption): void {
    if (option.disabled) return;
    this.remember(option.id);
    this.closeMenu(true);
    this.emit('split-select', option.id);
  }

  private readonly onKeydown = (event: KeyboardEvent): void => {
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    switch (event.key) {
      case 'Escape':
        if (!this.menuOpen) return;
        // Esc closes this menu only: the page's own Esc shortcut must not also run.
        event.preventDefault();
        event.stopPropagation();
        this.closeMenu(true);
        return;
      case 'ArrowDown':
      case 'ArrowUp':
        if (!this.canOpen()) return;
        event.preventDefault();
        if (this.menuOpen) this.moveFocus(event.key === 'ArrowDown' ? 1 : -1);
        else this.openMenu();
        return;
      case 'Home':
      case 'End':
        if (!this.menuOpen) return;
        event.preventDefault();
        this.moveFocus(event.key === 'Home' ? 'first' : 'last');
        return;
    }
  };

  private readonly onFocusOut = (event: FocusEvent): void => {
    const next = event.relatedTarget;
    if (this.menuOpen && next instanceof Node && !this.detailsRef.value?.contains(next)) {
      this.closeMenu(false);
    }
  };

  /** Only listens while the menu is open. Capture: a handler elsewhere cannot swallow the press. */
  private readonly onOutsidePointerDown = (event: Event): void => {
    const target = event.target;
    if (target instanceof Node && this.detailsRef.value?.contains(target)) return;
    this.closeMenu(false);
  };

  private renderCount() {
    const { count } = this;
    if (typeof count !== 'number' || !Number.isFinite(count)) return nothing;
    return html`<span class="badge badge-neutral">${formatCount(count)}</span>`;
  }

  private renderEntry(option: SplitOption, index: number, selectedId: string) {
    const selected = option.id === selectedId;
    const hint = option.hint || undefined;
    const hintId = `${this.uid}-hint-${index}`;
    return html`
      <li
        role="none"
        class=${hint ? 'tooltip tooltip-left' : nothing}
        data-tip=${hint ?? nothing}
      >
        <button
          type="button"
          role="menuitemradio"
          tabindex="-1"
          class=${selected ? 'menu-active' : option.danger ? 'text-error' : nothing}
          aria-checked=${selected ? 'true' : 'false'}
          aria-describedby=${hint ? hintId : nothing}
          ?disabled=${option.disabled}
          @click=${() => this.choose(option)}
        >
          ${option.icon ? html`<opm-icon name=${option.icon} size="sm"></opm-icon>` : nothing}
          <span>${option.label}</span>
          ${selected ? html`<opm-icon name="check" size="sm"></opm-icon>` : nothing}
        </button>
        ${hint ? html`<span id=${hintId} class="sr-only">${hint}</span>` : nothing}
      </li>
    `;
  }

  protected override render() {
    const resolved = this.resolve();
    if (!resolved) return nothing;
    const { option, usable } = resolved;
    const locked = !usable || this.disabled || this.busy;
    const colour = option.danger ? 'btn-error' : 'btn-primary';
    const menuId = `${this.uid}-menu`;
    return html`
      <div class="join min-w-0 max-w-full">
        <button
          type="button"
          class="btn btn-sm join-item min-w-0 flex-1 ${colour}"
          title=${option.label}
          ?disabled=${locked}
          aria-busy=${this.busy ? 'true' : nothing}
          @click=${this.onMainClick}
        >
          ${
            this.busy
              ? html`<span class="loading loading-spinner" aria-hidden="true"></span>`
              : option.icon
                ? html`<opm-icon name=${option.icon} size="sm"></opm-icon>`
                : nothing
          }
          <span class="truncate">${option.label}</span>
          ${this.renderCount()}
        </button>
        <details
          ${ref(this.detailsRef)}
          class="dropdown dropdown-top dropdown-end shrink-0"
          ?open=${this.menuOpen}
          @keydown=${this.onKeydown}
          @focusout=${this.onFocusOut}
        >
          <summary
            ${ref(this.summaryRef)}
            class="btn btn-sm btn-square join-item ${colour} ${locked ? 'btn-disabled' : ''}"
            aria-label="Choose action"
            aria-haspopup="menu"
            aria-expanded=${this.menuOpen ? 'true' : 'false'}
            aria-controls=${menuId}
            aria-disabled=${locked ? 'true' : nothing}
            tabindex=${locked ? '-1' : nothing}
            @click=${this.onSummaryClick}
          >
            <opm-icon name="chevron-up"></opm-icon>
          </summary>
          <ul
            id=${menuId}
            class="menu dropdown-content mb-2 w-max min-w-56 rounded-box border border-base-300 bg-base-100 p-2 shadow-lg"
            role="menu"
            aria-label="Choose action"
          >
            ${this.options.map((entry, index) => this.renderEntry(entry, index, option.id))}
          </ul>
        </details>
      </div>
    `;
  }
}

customElements.define('opm-split-button', OpmSplitButton);

declare global {
  interface HTMLElementTagNameMap {
    'opm-split-button': OpmSplitButton;
  }
  interface HTMLElementEventMap {
    'split-action': SplitActionEvent;
    'split-select': SplitSelectEvent;
  }
}
