import { html, nothing } from 'lit';
import { OpmElement } from '@/base/opm-element';
import type { IconName } from '@/components/opm-icon';
import '@/components/opm-icon';
import { toggleIn } from '@/util/filter';
import { formatCount } from '@/util/format-count';

/**
 * One button of the group. `label` is the visible text. `name` replaces it as the accessible
 * name when the text alone says too little (`1` -> "1 star"). `count` is shown as a superscript
 * and appended to the accessible name.
 */
export interface SegmentOption {
  value: string;
  label: string;
  name?: string;
  icon?: IconName;
  /** A native tooltip. */
  title?: string;
  count?: number;
  disabled?: boolean;
}

/** One value in `single` mode, the list of chosen values in `multi` mode. */
export type SegmentedValue = string | string[];

export type SegmentedChangeEvent = CustomEvent<SegmentedValue>;

/**
 * A row of joined buttons to choose from a few options: one at a time (`single`, a radio group)
 * or any number (`multi`, toggle buttons).
 *
 *   html`<opm-segmented
 *     label="Rating"
 *     mode="multi"
 *     .options=${[
 *       { value: '0', label: 'Unrated' },
 *       { value: '5', label: '5', name: '5 stars', icon: 'star', count: 12 },
 *     ]}
 *     .value=${['5']}
 *     @segmented-change=${(e: SegmentedChangeEvent) => this.setRatings(e.detail as string[])}
 *   ></opm-segmented>`
 *
 * The component is CONTROLLED: it never changes `value` itself. The `segmented-change` event
 * (bubbles) carries the new value, a `string` in `single` mode and a new `string[]` in `multi`
 * mode, and the host feeds it back through `value`. Pressing the chosen option of a `single`
 * group changes nothing and sends nothing.
 *
 * Keyboard: in `single` mode the group is one tab stop (the chosen option, else the first enabled
 * one) and the arrow keys, Home and End move to the next enabled option AND choose it, as in a
 * native radio group. In `multi` mode every button is a tab stop, Space and Enter toggle it.
 *
 * `disabled` disables every button, an option's own `disabled` only that one.
 *
 * The row is never wider than its container: below the `sm` breakpoint a row that does not fit
 * scrolls sideways (the focus ring of its outer buttons is then clipped).
 */
export class OpmSegmented extends OpmElement {
  static override properties = {
    mode: { type: String },
    options: { attribute: false },
    value: { attribute: false },
    label: { type: String },
    disabled: { type: Boolean },
  };

  declare mode: 'single' | 'multi';
  declare options: readonly SegmentOption[];
  declare value: SegmentedValue;
  /** The accessible name of the whole group. */
  declare label: string;
  declare disabled: boolean;

  constructor() {
    super();
    this.mode = 'single';
    this.options = [];
    this.value = '';
    this.label = '';
    this.disabled = false;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('inline-flex', 'max-w-full');
  }

  /** The chosen values as a list, whatever shape `value` has. */
  private chosen(): string[] {
    const { value } = this;
    if (Array.isArray(value)) return this.mode === 'single' ? value.slice(0, 1) : value;
    return value === '' ? [] : [value];
  }

  private isLocked(option: SegmentOption): boolean {
    return this.disabled || option.disabled === true;
  }

  private emit(value: SegmentedValue): void {
    this.dispatchEvent(
      new CustomEvent<SegmentedValue>('segmented-change', { detail: value, bubbles: true }),
    );
  }

  private select(option: SegmentOption): void {
    if (this.isLocked(option)) return;
    const chosen = this.chosen();
    if (this.mode === 'multi') this.emit(toggleIn(chosen, option.value));
    else if (!chosen.includes(option.value)) this.emit(option.value);
  }

  private readonly onKeydown = (event: KeyboardEvent): void => {
    if (this.mode !== 'single' || event.defaultPrevented) return;
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const buttons = Array.from(this.querySelectorAll('button'));
    const index = buttons.indexOf(event.target as HTMLButtonElement);
    if (index === -1) return;
    const usable = this.options.flatMap((option, position) =>
      this.isLocked(option) ? [] : [position],
    );
    const at = usable.indexOf(index);
    if (at === -1) return;
    let next: number | undefined;
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        next = usable[(at + 1) % usable.length];
        break;
      case 'ArrowLeft':
      case 'ArrowUp':
        next = usable[(at - 1 + usable.length) % usable.length];
        break;
      case 'Home':
        next = usable[0];
        break;
      case 'End':
        next = usable[usable.length - 1];
        break;
      default:
        return;
    }
    event.preventDefault();
    const option = next === undefined ? undefined : this.options[next];
    if (next === undefined || !option) return;
    buttons[next]?.focus();
    this.select(option);
  };

  private renderOption(option: SegmentOption, tabStop: string | undefined, chosen: string[]) {
    const single = this.mode === 'single';
    const on = chosen.includes(option.value);
    const counted = typeof option.count === 'number' && Number.isFinite(option.count);
    const name = counted
      ? `${option.name ?? option.label} (${formatCount(option.count as number)})`
      : option.name;
    return html`
      <button
        type="button"
        class="btn btn-sm join-item ${on ? 'btn-active btn-primary' : ''}"
        role=${single ? 'radio' : nothing}
        aria-checked=${single ? (on ? 'true' : 'false') : nothing}
        aria-pressed=${single ? nothing : on ? 'true' : 'false'}
        aria-label=${name ?? nothing}
        title=${option.title ?? nothing}
        tabindex=${single ? (option.value === tabStop ? '0' : '-1') : nothing}
        ?disabled=${this.isLocked(option)}
        @click=${() => this.select(option)}
      >
        ${option.icon ? html`<opm-icon name=${option.icon} size="sm"></opm-icon>` : nothing}
        <span>${option.label}</span>
        ${counted ? html`<sup class="opacity-70">${formatCount(option.count as number)}</sup>` : nothing}
      </button>
    `;
  }

  protected override render() {
    const chosen = this.chosen();
    const tabStop =
      this.options.find((option) => !this.isLocked(option) && chosen.includes(option.value)) ??
      this.options.find((option) => !this.isLocked(option));
    return html`
      <div
        class="join max-sm:overflow-x-auto"
        role=${this.mode === 'single' ? 'radiogroup' : 'group'}
        aria-label=${this.label || nothing}
        @keydown=${this.onKeydown}
      >
        ${this.options.map((option) => this.renderOption(option, tabStop?.value, chosen))}
      </div>
    `;
  }
}

customElements.define('opm-segmented', OpmSegmented);

declare global {
  interface HTMLElementTagNameMap {
    'opm-segmented': OpmSegmented;
  }
  interface HTMLElementEventMap {
    'segmented-change': SegmentedChangeEvent;
  }
}
