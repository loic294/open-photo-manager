import { html } from 'lit';
import { ifDefined } from 'lit/directives/if-defined.js';
import { ApmElement } from '@/base/apm-element';

export interface SelectOption {
  value: string;
  label: string;
  /** Tooltip of the entry. */
  title?: string;
  disabled?: boolean;
}

export type SelectSize = 'xs' | 'sm' | 'md';

export interface SelectChangeDetail {
  value: string;
}

export type SelectChangeEvent = CustomEvent<SelectChangeDetail>;

declare global {
  interface GlobalEventHandlersEventMap {
    'select-change': SelectChangeEvent;
  }
}

/** Complete class names, so Tailwind finds them. */
const SIZE_CLASS: Readonly<Record<SelectSize, string>> = {
  xs: 'select-xs',
  sm: 'select-sm',
  md: 'select-md',
};

/**
 * The one drop-down of the app: a thin wrapper over the DaisyUI `select` (a native `<select>`, so
 * the keyboard, the screen readers and the mobile pickers all work). Nobody writes a raw
 * `<select>` in a component; use this instead.
 *
 *   html`<apm-select
 *     label="Sort by"
 *     size="xs"
 *     .options=${[{ value: 'name', label: 'Name' }, { value: 'date', label: 'Date' }]}
 *     .value=${sort.key}
 *     @select-change=${(e: SelectChangeEvent) => setKey(e.detail.value)}
 *   ></apm-select>`
 *
 * Properties: `options` (`value` and `label`, optionally a `title` tooltip and `disabled`),
 * `value`, `label` (the accessible name: always set it), `showLabel` (also show the label as text
 * inside the control, attribute `show-label`), `size` (`xs` | `sm` | `md`, default `sm`) and
 * `disabled`. The control is as wide as its longest entry.
 *
 * Event: `select-change` (bubbles) with `detail: { value }`, fired when the user picks an entry.
 * The native `change` event is stopped here, so listen to this one.
 *
 * The element is CONTROLLED: it keeps no value of its own. After a change it shows whatever
 * `value` still says, so a host that does not accept the change simply leaves its `value` as it
 * was and the control snaps back. A `value` that no option has shows an empty control.
 */
export class ApmSelect extends ApmElement {
  static override properties = {
    options: { attribute: false },
    value: { type: String },
    label: { type: String },
    showLabel: { type: Boolean, attribute: 'show-label' },
    size: { type: String },
    disabled: { type: Boolean },
  };

  declare options: SelectOption[];
  declare value: string;
  /** The accessible name of the control. */
  declare label: string;
  /** Show `label` as text inside the control, as well as naming it. */
  declare showLabel: boolean;
  declare size: SelectSize;
  declare disabled: boolean;

  constructor() {
    super();
    this.options = [];
    this.value = '';
    this.label = '';
    this.showLabel = false;
    this.size = 'sm';
    this.disabled = false;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.classList.add('inline-flex');
  }

  protected override updated(): void {
    // The options exist only after the select's own bindings are set, so the value is applied
    // here: it also puts a select the user changed back to the value the host still has.
    const select = this.querySelector('select');
    if (select && select.value !== this.value) select.value = this.value;
  }

  private onChange(event: Event): void {
    event.stopPropagation();
    const { value } = event.currentTarget as HTMLSelectElement;
    this.dispatchEvent(
      new CustomEvent<SelectChangeDetail>('select-change', { detail: { value }, bubbles: true }),
    );
    // Even when the host keeps its `value`, the control has to go back to it.
    this.requestUpdate();
  }

  private renderSelect(classes: string | undefined) {
    return html`
      <select
        class=${ifDefined(classes)}
        aria-label=${this.label}
        title=${ifDefined(this.showLabel || !this.label ? undefined : this.label)}
        ?disabled=${this.disabled}
        @change=${this.onChange}
      >
        ${this.options.map(
          (option) =>
            html`<option value=${option.value} title=${ifDefined(option.title)} ?disabled=${option.disabled === true}>${option.label}</option>`,
        )}
      </select>
    `;
  }

  protected override render() {
    const size = SIZE_CLASS[this.size] ?? SIZE_CLASS.sm;
    if (!this.showLabel) return this.renderSelect(`select ${size} w-auto`);
    return html`
      <label class="select ${size} w-auto">
        <span class="label">${this.label}</span>
        ${this.renderSelect(undefined)}
      </label>
    `;
  }
}

customElements.define('apm-select', ApmSelect);

declare global {
  interface HTMLElementTagNameMap {
    'apm-select': ApmSelect;
  }
}
