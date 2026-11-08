import { html, nothing, type PropertyValues } from 'lit';
import { createRef, ref } from 'lit/directives/ref.js';
import { ApmElement } from '@/base/apm-element';
import type { IconName } from '@/components/apm-icon';
import '@/components/apm-icon';
import { MAX_ENTRY_CHARS, sameText, textKey, toggleIn } from '@/util/filter';
import { formatCount } from '@/util/format-count';

/** One value to offer. `count` is how many items carry it (shown when known). */
export interface MultiSelectOption {
  value: string;
  count?: number;
}

/** `detail` is the new list of chosen values. */
export type MultiSelectChangeEvent = CustomEvent<string[]>;

/** The most rows the panel draws before it asks the user to type to narrow the list down. */
const DEFAULT_MAX_VISIBLE = 100;

let instanceCounter = 0;

/**
 * A button that opens a panel to choose any number of values from a long list (cameras, tags),
 * with a search box. The chosen values also show OUTSIDE the panel as removable chips, so the
 * active choice is visible and removable without opening it.
 *
 *   html`<apm-multi-select
 *     label="Cameras"
 *     icon="camera"
 *     placeholder="Search cameras"
 *     .options=${[{ value: 'Canon EOS R5', count: 120 }, { value: 'Sony A7 IV', count: 31 }]}
 *     .selected=${filter.cameras}
 *     .loading=${loadingFacets}
 *     @multi-select-change=${(e: MultiSelectChangeEvent) => this.setCameras(e.detail)}
 *   ></apm-multi-select>`
 *
 * The component is CONTROLLED: it never changes `selected` itself. `multi-select-change`
 * (bubbles) carries the new list (a new array, in selection order) after every toggle, removal,
 * addition and "Clear"; the host feeds it back through `selected`. Typing in the search box
 * never fires it.
 *
 * Values are compared without case (and NFC-normalised), as the server does, so `canon eos r5`
 * and `Canon EOS R5` are one value. A chosen value that is not in `options` (the facets are
 * still loading, or the value is gone from the folder) is listed first in the panel, and can be
 * unchecked there or removed with its chip.
 *
 * `allowCustom` lets the user add a value that is not in the list: an `Add "text"` row appears
 * for a text that matches nothing, and Enter in the search box adds the text (the spelling of an
 * option that matches it without case wins). `maxVisible` limits the rows drawn (the rest is
 * reached by searching). `loading` shows a spinner row, keeping the values already known. `icon`
 * decorates the button.
 *
 * Keyboard: Enter, Space or ArrowDown on the button opens the panel and focuses the search box;
 * ArrowDown/ArrowUp then move between the search box, the add row and the checkboxes; Space
 * toggles a checkbox; Escape closes the panel and focuses the button again. The panel also
 * closes on a press outside it and when focus leaves it. If it would stick out of the right edge
 * of the window it opens towards the left instead (checked again whenever the button may have
 * moved while it is open: after an update and on a window resize). Below the `sm` breakpoint
 * (a panel that wide fits on neither side of every button) it spans the window.
 */
export class ApmMultiSelect extends ApmElement {
  static override properties = {
    options: { attribute: false },
    selected: { attribute: false },
    label: { type: String },
    placeholder: { type: String },
    icon: { type: String },
    allowCustom: { type: Boolean, attribute: 'allow-custom' },
    loading: { type: Boolean },
    disabled: { type: Boolean },
    maxVisible: { type: Number, attribute: 'max-visible' },
    panelOpen: { state: true },
    query: { state: true },
  };

  declare options: readonly MultiSelectOption[];
  declare selected: readonly string[];
  /** The text of the button and the accessible name of the panel. */
  declare label: string;
  /** Shown in the empty search box. */
  declare placeholder: string;
  declare icon: IconName | undefined;
  declare allowCustom: boolean;
  declare loading: boolean;
  declare disabled: boolean;
  declare maxVisible: number;
  declare private panelOpen: boolean;
  declare private query: string;

  private readonly uid = `apm-multi-select-${++instanceCounter}`;
  private readonly detailsRef = createRef<HTMLDetailsElement>();
  private readonly summaryRef = createRef<HTMLElement>();
  private readonly panelRef = createRef<HTMLElement>();
  private readonly searchRef = createRef<HTMLInputElement>();
  private restoreFocus = false;

  constructor() {
    super();
    this.options = [];
    this.selected = [];
    this.label = '';
    this.placeholder = '';
    this.allowCustom = false;
    this.loading = false;
    this.disabled = false;
    this.maxVisible = DEFAULT_MAX_VISIBLE;
    this.panelOpen = false;
    this.query = '';
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('inline-flex', 'flex-wrap', 'items-center', 'gap-1', 'max-w-full');
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    document.removeEventListener('pointerdown', this.onOutsidePointerDown, true);
    window.removeEventListener('resize', this.onResize);
    this.panelOpen = false;
    this.query = '';
  }

  protected override willUpdate(changed: PropertyValues): void {
    if (this.panelOpen && this.disabled) this.panelOpen = false;
    // Here and not in `updated`: a state change there schedules a second update.
    if (changed.has('panelOpen') && !this.panelOpen) this.query = '';
  }

  protected override updated(changed: PropertyValues): void {
    if (this.panelOpen) {
      if (changed.has('panelOpen')) {
        document.addEventListener('pointerdown', this.onOutsidePointerDown, true);
        window.addEventListener('resize', this.onResize);
        this.searchRef.value?.focus();
      }
      // Every update: a chip, a badge or a sibling can move the button while the panel is open.
      this.placePanel();
    } else if (changed.get('panelOpen') === true) {
      document.removeEventListener('pointerdown', this.onOutsidePointerDown, true);
      window.removeEventListener('resize', this.onResize);
      this.detailsRef.value?.classList.remove('dropdown-end');
      if (this.restoreFocus) this.summaryRef.value?.focus();
      this.restoreFocus = false;
    }
  }

  /**
   * Opens the panel towards the left when it would stick out of the window on the right. It is
   * measured in the default placement, so the answer does not depend on the previous one. A class
   * and not state: no second update and no frame in the wrong place.
   */
  private placePanel(): void {
    const dropdown = this.detailsRef.value;
    const panel = this.panelRef.value;
    if (!dropdown || !panel) return;
    dropdown.classList.remove('dropdown-end');
    dropdown.classList.toggle(
      'dropdown-end',
      panel.getBoundingClientRect().right > window.innerWidth,
    );
  }

  private isChosen(value: string): boolean {
    return this.selected.some((entry) => sameText(entry, value));
  }

  private emit(values: string[]): void {
    this.dispatchEvent(
      new CustomEvent<string[]>('multi-select-change', { detail: values, bubbles: true }),
    );
  }

  private openPanel(): void {
    if (!this.disabled) this.panelOpen = true;
  }

  private closePanel(restoreFocus: boolean): void {
    if (!this.panelOpen) return;
    this.restoreFocus = restoreFocus;
    this.panelOpen = false;
  }

  private toggleValue(value: string): void {
    if (this.disabled) return;
    this.emit(toggleIn(this.selected, value, sameText));
  }

  private removeValue(value: string): void {
    if (this.disabled) return;
    this.emit(this.selected.filter((entry) => !sameText(entry, value)));
    // The chip is about to go: keep the focus somewhere that stays.
    this.summaryRef.value?.focus();
  }

  private clear(): void {
    if (this.disabled || this.selected.length === 0) return;
    this.emit([]);
  }

  /** Adds the typed text; an option that matches it without case lends its spelling. */
  private addTyped(): void {
    const text = this.query.trim();
    if (!this.allowCustom || this.disabled || text === '') return;
    const spelling = this.options.find((option) => sameText(option.value, text))?.value ?? text;
    this.query = '';
    if (!this.isChosen(spelling)) this.emit([...this.selected, spelling]);
  }

  private stops(): HTMLElement[] {
    return Array.from(
      this.querySelectorAll<HTMLElement>(
        'input[type="search"], [data-add], input[type="checkbox"]',
      ),
    );
  }

  private moveFocus(step: 1 | -1): void {
    const stops = this.stops();
    const current = stops.indexOf(document.activeElement as HTMLElement);
    if (current === -1) {
      if (step === 1) stops[0]?.focus();
      return;
    }
    stops[Math.min(stops.length - 1, Math.max(0, current + step))]?.focus();
  }

  private readonly onSummaryClick = (event: Event): void => {
    // The open state is ours (the browser must not toggle the details by itself).
    event.preventDefault();
    if (this.panelOpen) this.closePanel(true);
    else this.openPanel();
  };

  private readonly onKeydown = (event: KeyboardEvent): void => {
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    switch (event.key) {
      case 'Escape':
        if (!this.panelOpen) return;
        // Esc closes this panel only: the page's own Esc shortcut must not also run.
        event.preventDefault();
        event.stopPropagation();
        this.closePanel(true);
        return;
      case 'ArrowDown':
      case 'ArrowUp':
        if (this.disabled) return;
        if (this.panelOpen) {
          event.preventDefault();
          this.moveFocus(event.key === 'ArrowDown' ? 1 : -1);
        } else if (event.key === 'ArrowDown') {
          event.preventDefault();
          this.openPanel();
        }
        return;
      case 'Enter':
        if (event.target !== this.searchRef.value || event.isComposing) return;
        event.preventDefault();
        this.addTyped();
        return;
    }
  };

  private readonly onFocusOut = (event: FocusEvent): void => {
    const next = event.relatedTarget;
    if (this.panelOpen && next instanceof Node && !this.detailsRef.value?.contains(next)) {
      this.closePanel(false);
    }
  };

  /** Only listens while the panel is open. Capture: a handler elsewhere cannot swallow the press. */
  private readonly onOutsidePointerDown = (event: Event): void => {
    const target = event.target;
    if (target instanceof Node && this.detailsRef.value?.contains(target)) return;
    this.closePanel(false);
  };

  /** Only listens while the panel is open. */
  private readonly onResize = (): void => {
    if (this.panelOpen) this.placePanel();
  };

  private readonly onSearchInput = (event: Event): void => {
    this.query = (event.currentTarget as HTMLInputElement).value;
  };

  private readonly onCheckboxChange = (event: Event, value: string): void => {
    // Stay controlled: the box shows what `selected` says until the host feeds the change back.
    (event.currentTarget as HTMLInputElement).checked = this.isChosen(value);
    this.toggleValue(value);
  };

  /**
   * What the panel lists: the chosen values the options lack, then the options, narrowed by the
   * search and cut at `maxVisible`. `canAdd` is the text to offer as a new value, if any.
   */
  private rows(): { shown: MultiSelectOption[]; matching: number; canAdd: string | null } {
    const known = new Set(this.options.map((option) => textKey(option.value)));
    const missing = new Map<string, MultiSelectOption>();
    for (const value of this.selected) {
      const key = textKey(value);
      if (!known.has(key) && !missing.has(key)) missing.set(key, { value });
    }
    const all = [...missing.values(), ...this.options];
    const text = this.query.trim();
    const needle = textKey(text);
    const matching = needle === '' ? all : all.filter((o) => textKey(o.value).includes(needle));
    const canAdd =
      this.allowCustom && text !== '' && !all.some((option) => sameText(option.value, text))
        ? text
        : null;
    const cap = this.maxVisible >= 1 ? this.maxVisible : DEFAULT_MAX_VISIBLE;
    return { shown: matching.slice(0, cap), matching: matching.length, canAdd };
  }

  private renderRow(option: MultiSelectOption) {
    const count = option.count;
    const counted = typeof count === 'number' && Number.isFinite(count);
    return html`
      <li>
        <label>
          <input
            type="checkbox"
            class="checkbox checkbox-sm"
            .checked=${this.isChosen(option.value)}
            aria-label=${counted ? `${option.value} (${formatCount(count)})` : nothing}
            ?disabled=${this.disabled}
            @change=${(event: Event) => this.onCheckboxChange(event, option.value)}
          />
          <span class="truncate" title=${option.value}>${option.value}</span>
          ${counted ? html`<span class="badge badge-ghost badge-sm">${formatCount(count)}</span>` : nothing}
        </label>
      </li>
    `;
  }

  private renderPanel() {
    const { shown, matching, canAdd } = this.rows();
    const capped = matching > shown.length;
    const searchName = `${this.allowCustom ? 'Search or add' : 'Search'} ${this.label}`.trim();
    const empty = shown.length === 0 && canAdd === null;
    return html`
      <label class="input input-sm w-full">
        <apm-icon name="search" size="sm"></apm-icon>
        <input
          ${ref(this.searchRef)}
          type="search"
          class="grow"
          maxlength=${MAX_ENTRY_CHARS}
          placeholder=${this.placeholder || nothing}
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          aria-label=${searchName}
          .value=${this.query}
          @input=${this.onSearchInput}
        />
      </label>
      <ul class="menu menu-sm mt-1 max-h-60 flex-nowrap overflow-y-auto p-0">
        ${
          canAdd === null
            ? nothing
            : html`
              <li>
                <button type="button" data-add @click=${() => this.addTyped()}>
                  <apm-icon name="plus" size="sm"></apm-icon>
                  <span class="truncate">Add "${canAdd}"</span>
                </button>
              </li>
            `
        }
        ${shown.map((option) => this.renderRow(option))}
      </ul>
      ${
        this.loading
          ? html`<p class="flex items-center gap-2 px-2 py-1 text-sm text-base-content/70" role="status">
              <span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
              Loading…
            </p>`
          : empty
            ? html`<p class="px-2 py-1 text-sm text-base-content/70">
                ${this.query.trim() === '' ? 'No values' : 'No match'}
              </p>`
            : nothing
      }
      <div class="mt-1 flex items-center justify-between gap-2 text-xs text-base-content/70">
        <span>${capped ? `Showing the first ${formatCount(shown.length)}, type to search` : ''}</span>
        <button
          type="button"
          class="btn btn-ghost btn-xs"
          ?disabled=${this.disabled || this.selected.length === 0}
          @click=${() => this.clear()}
        >
          Clear
        </button>
      </div>
    `;
  }

  private renderChip(value: string) {
    return html`
      <li class="badge badge-soft badge-primary h-7 min-w-0 max-w-full gap-1 pr-0.5">
        <span class="truncate" title=${value}>${value}</span>
        <button
          type="button"
          class="btn btn-ghost btn-circle btn-xs size-5 min-h-0 shrink-0"
          aria-label="Remove ${value}"
          ?disabled=${this.disabled}
          @click=${() => this.removeValue(value)}
        >
          <apm-icon name="x" size="xs"></apm-icon>
        </button>
      </li>
    `;
  }

  protected override render() {
    const count = this.selected.length;
    const panelId = `${this.uid}-panel`;
    return html`
      <details
        ${ref(this.detailsRef)}
        class="dropdown"
        ?open=${this.panelOpen}
        @keydown=${this.onKeydown}
        @focusout=${this.onFocusOut}
      >
        <summary
          ${ref(this.summaryRef)}
          class="btn btn-sm ${this.disabled ? 'btn-disabled' : ''}"
          aria-label=${count > 0 ? `${this.label} (${count} selected)` : nothing}
          aria-expanded=${this.panelOpen ? 'true' : 'false'}
          aria-controls=${panelId}
          aria-disabled=${this.disabled ? 'true' : nothing}
          tabindex=${this.disabled ? '-1' : nothing}
          @click=${this.onSummaryClick}
        >
          ${this.icon ? html`<apm-icon name=${this.icon} size="sm"></apm-icon>` : nothing}
          <span>${this.label}</span>
          ${count > 0 ? html`<span class="badge badge-primary badge-sm">${formatCount(count)}</span>` : nothing}
          <apm-icon name="chevron-down" size="xs"></apm-icon>
        </summary>
        <div
          ${ref(this.panelRef)}
          id=${panelId}
          class="dropdown-content mt-1 rounded-box border border-base-300 bg-base-100 p-2 shadow-lg max-sm:fixed max-sm:inset-x-2 sm:w-72"
          role="group"
          aria-label=${this.label || nothing}
          aria-busy=${this.loading ? 'true' : nothing}
        >
          ${this.panelOpen ? this.renderPanel() : nothing}
        </div>
      </details>
      ${
        count > 0
          ? html`<ul class="contents" role="list" aria-label="Selected ${this.label}">
              ${this.selected.map((value) => this.renderChip(value))}
            </ul>`
          : nothing
      }
    `;
  }
}

customElements.define('apm-multi-select', ApmMultiSelect);

declare global {
  interface HTMLElementTagNameMap {
    'apm-multi-select': ApmMultiSelect;
  }
  interface HTMLElementEventMap {
    'multi-select-change': MultiSelectChangeEvent;
  }
}
