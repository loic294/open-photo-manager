import type { ReactiveController } from 'lit';
import type { OpmElement } from '@/base/opm-element';
import {
  activePane,
  type PaneId,
  type PaneSelection,
  selection as paneSelections,
} from '@/stores/selection';
import { type Activation, interpret } from '@/util/selection-rules';

const CARD_ACTIVATE_EVENT = 'card-activate';
const VIA: readonly unknown[] = ['checkbox', 'card', 'space', 'enter'];
const CAPTURE: AddEventListenerOptions = { capture: true };

/** The detail of the grid's bubbling `card-activate` event. Never sent for planned ghosts. */
export interface CardActivation extends Activation {
  /** Position of the item in the grid. */
  index: number;
}

/** The viewer listens for this on or above the pane; the selection never opens anything itself. */
export const ITEM_OPEN_EVENT = 'item-open';

export interface ItemOpenDetail {
  id: number;
  index: number;
}

export type ItemOpenEvent = CustomEvent<ItemOpenDetail>;

declare global {
  interface GlobalEventHandlersEventMap {
    [ITEM_OPEN_EVENT]: ItemOpenEvent;
  }
}

function isCardActivation(value: unknown): value is CardActivation {
  if (typeof value !== 'object' || value === null) return false;
  const detail = value as Record<string, unknown>;
  return (
    Number.isInteger(detail.id) &&
    Number.isInteger(detail.index) &&
    VIA.includes(detail.via) &&
    typeof detail.shift === 'boolean' &&
    typeof detail.toggle === 'boolean'
  );
}

export interface SelectionControllerOptions {
  /**
   * The pane this controller serves. A Lit property is not set yet when the field initialiser
   * runs, so a pane passes a function: `{ pane: () => this.pane }`.
   */
  pane: PaneId | (() => PaneId);
  /** Defaults to the registry's selection of the pane. */
  selection?: PaneSelection;
}

/**
 * Connects a pane element to its selection. While the host is connected it:
 * - turns the grid's `card-activate` events into a selection change (see `interpret`), or into a
 *   bubbling `item-open` event from the host when the activation means "open";
 * - marks the pane active (`activePane`, the target of the keyboard shortcuts) on `pointerdown`
 *   and `focusin` anywhere inside it.
 *
 * `selectedIds`, `active`, `count`, `busy` and `isActivePane` read signals: use them in the host's
 * `render()` and it re-renders when they change.
 *
 *   private readonly selecting = new SelectionController(this, { pane: () => this.pane });
 *   // render(): .selectedIds=${this.selecting.selectedIds} .selectionActive=${this.selecting.active}
 *
 * The selection outlives the controller: disconnecting never clears it (the context key does).
 */
export class SelectionController implements ReactiveController {
  private readonly host: OpmElement;
  private readonly options: SelectionControllerOptions;

  constructor(host: OpmElement, options: SelectionControllerOptions) {
    this.host = host;
    this.options = options;
    host.addController(this);
  }

  get pane(): PaneId {
    const { pane } = this.options;
    return typeof pane === 'function' ? pane() : pane;
  }

  get selection(): PaneSelection {
    return this.options.selection ?? paneSelections[this.pane];
  }

  /** The selected ids: a NEW Set on every change, so pass it to the grid as is. */
  get selectedIds(): ReadonlySet<number> {
    return this.selection.ids.value;
  }

  /** True while something is selected: the grid shows its checkboxes and a click selects. */
  get active(): boolean {
    return this.selection.active.value;
  }

  get count(): number {
    return this.selection.count.value;
  }

  /** True while select-all (or a range) waits for the list of ids: spin the select-all button. */
  get busy(): boolean {
    return this.selection.busy.value;
  }

  /** True when the user touched this pane last: highlight it in the bottom bar. */
  get isActivePane(): boolean {
    return activePane.value === this.pane;
  }

  /** For the bottom bar's `select-all`. Also marks the pane active, as a click inside would. */
  selectAll(): Promise<void> {
    activePane.value = this.pane;
    return this.selection.selectAll();
  }

  /** For the bottom bar's `clear-selection`. Also marks the pane active. */
  clear(): void {
    activePane.value = this.pane;
    this.selection.clear();
  }

  hostConnected(): void {
    this.host.addEventListener(CARD_ACTIVATE_EVENT, this.onCardActivate);
    this.host.addEventListener('pointerdown', this.markActive, CAPTURE);
    this.host.addEventListener('focusin', this.markActive, CAPTURE);
  }

  hostDisconnected(): void {
    this.host.removeEventListener(CARD_ACTIVATE_EVENT, this.onCardActivate);
    this.host.removeEventListener('pointerdown', this.markActive, CAPTURE);
    this.host.removeEventListener('focusin', this.markActive, CAPTURE);
    // A pane that is gone cannot receive the shortcuts: the source pane always exists.
    if (this.pane !== 'source' && activePane.peek() === this.pane) activePane.value = 'source';
  }

  private readonly onCardActivate = (event: Event): void => {
    const detail = (event as CustomEvent<unknown>).detail;
    if (!isCardActivation(detail)) return;
    const { selection } = this;
    const decision = interpret(detail, { active: selection.active.peek() });
    switch (decision.type) {
      case 'toggle':
        selection.toggle(decision.id);
        break;
      case 'range':
        void selection.rangeTo(decision.id);
        break;
      case 'open':
        this.host.dispatchEvent(
          new CustomEvent<ItemOpenDetail>(ITEM_OPEN_EVENT, {
            bubbles: true,
            detail: { id: decision.id, index: detail.index },
          }),
        );
        break;
    }
  };

  private readonly markActive = (): void => {
    activePane.value = this.pane;
  };
}
