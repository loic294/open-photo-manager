import { html } from 'lit';
import { afterEach, describe, expect, it } from 'vitest';
import { ApmElement } from '@/base/apm-element';
import {
  type CardActivation,
  ITEM_OPEN_EVENT,
  type ItemOpenEvent,
  SelectionController,
} from '@/controllers/selection-controller';
import {
  activePane,
  type IdsProvider,
  type PaneId,
  PaneSelection,
  resetSelectionForTests,
  selection,
} from '@/stores/selection';
import { mount } from '@/test/helpers';

const ITEMS = [1, 2, 3, 4, 5];

/**
 * A pane built the way the pane todo builds it: the controller is a field, and `render()` speaks
 * the grid's contract (a bubbling `card-activate` from a card, `selectedIds` and `active` read
 * from the controller).
 */
class ApmTestSelectionPane extends ApmElement {
  static override properties = { pane: { type: String } };

  pane: PaneId = 'source';
  readonly selecting = new SelectionController(this, { pane: () => this.pane });

  protected override render() {
    const { selecting } = this;
    return html`
      <span class="count">${selecting.count}</span>
      <span class="active">${String(selecting.active)}</span>
      <span class="busy">${String(selecting.busy)}</span>
      <span class="current">${String(selecting.isActivePane)}</span>
      ${ITEMS.map(
        (id, index) => html`
          <button
            type="button"
            data-id=${id}
            aria-pressed=${selecting.selectedIds.has(id)}
            @click=${(event: MouseEvent) => this.activate(event, id, index)}
          >
            ${id}
          </button>
        `,
      )}
    `;
  }

  /** What the grid does on a click: one `card-activate` that bubbles up from the card. */
  private activate(event: MouseEvent, id: number, index: number): void {
    const detail: CardActivation = {
      id,
      index,
      via: 'card',
      shift: event.shiftKey,
      toggle: event.metaKey || event.ctrlKey,
    };
    (event.currentTarget as HTMLElement).dispatchEvent(
      new CustomEvent('card-activate', { bubbles: true, detail }),
    );
  }
}

/** A host with no controller: each test attaches its own. */
class ApmTestSelectionBare extends ApmElement {
  protected override render() {
    return html``;
  }
}

customElements.define('apm-test-selection-pane', ApmTestSelectionPane);
customElements.define('apm-test-selection-bare', ApmTestSelectionBare);

declare global {
  interface HTMLElementTagNameMap {
    'apm-test-selection-pane': ApmTestSelectionPane;
    'apm-test-selection-bare': ApmTestSelectionBare;
  }
}

const stops: Array<() => void> = [];

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  resetSelectionForTests();
});

function mountPane(pane: PaneId = 'source'): Promise<ApmTestSelectionPane> {
  return mount('apm-test-selection-pane', { pane });
}

function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function activation(overrides: Partial<CardActivation> = {}): CardActivation {
  return { id: 1, index: 0, via: 'card', shift: false, toggle: false, ...overrides };
}

/** Dispatches the grid's event from `from`, the way a card does. */
function send(from: EventTarget, detail: unknown): void {
  from.dispatchEvent(new CustomEvent('card-activate', { bubbles: true, detail }));
}

function card(host: ParentNode, id: number): HTMLButtonElement {
  const found = host.querySelector<HTMLButtonElement>(`button[data-id="${id}"]`);
  if (!found) throw new Error(`no card ${id}`);
  return found;
}

function click(host: ParentNode, id: number, modifiers: MouseEventInit = {}): void {
  card(host, id).dispatchEvent(new MouseEvent('click', { bubbles: true, ...modifiers }));
}

function text(host: ParentNode, selector: string): string | undefined {
  return host.querySelector(selector)?.textContent?.trim();
}

function members(pane: PaneSelection): number[] {
  return [...pane.ids.value].sort((a, b) => a - b);
}

function answering(ids: number[]): IdsProvider {
  return async () => ids;
}

/** A provider whose single answer waits for the test. */
function pending(): { provider: IdsProvider; answer: (ids: number[]) => void } {
  let answer: (ids: number[]) => void = () => undefined;
  const provider: IdsProvider = () =>
    new Promise<number[]>((settle) => {
      answer = settle;
    });
  return { provider, answer: (ids) => answer(ids) };
}

function recordOpens(): ItemOpenEvent[] {
  const opens: ItemOpenEvent[] = [];
  const record = (event: ItemOpenEvent): void => {
    opens.push(event);
  };
  document.body.addEventListener(ITEM_OPEN_EVENT, record);
  stops.push(() => document.body.removeEventListener(ITEM_OPEN_EVENT, record));
  return opens;
}

describe('SelectionController: card-activate', () => {
  it('toggles the item when its checkbox is activated', async () => {
    const host = await mountPane();
    send(card(host, 3), activation({ id: 3, index: 2, via: 'checkbox' }));
    expect(selection.source.toArray()).toEqual([3]);
    send(card(host, 3), activation({ id: 3, index: 2, via: 'checkbox' }));
    expect(selection.source.toArray()).toEqual([]);
  });

  it('toggles a card clicked with Cmd or Ctrl, even when nothing is selected', async () => {
    const host = await mountPane();
    const opens = recordOpens();
    click(host, 2, { metaKey: true });
    click(host, 4, { ctrlKey: true });
    expect(selection.source.toArray()).toEqual([2, 4]);
    click(host, 2, { metaKey: true });
    expect(selection.source.toArray()).toEqual([4]);
    expect(opens).toEqual([]);
  });

  it('opens the item on a plain click while nothing is selected', async () => {
    const host = await mountPane();
    const opens = recordOpens();
    click(host, 3);
    expect(selection.source.count.value).toBe(0);
    expect(opens).toHaveLength(1);
    expect(opens[0]?.detail).toEqual({ id: 3, index: 2 });
    expect(opens[0]?.bubbles).toBe(true);
    expect(opens[0]?.target).toBe(host);
  });

  it('selects on a plain click while something is selected, and opens nothing', async () => {
    const host = await mountPane();
    const opens = recordOpens();
    click(host, 1, { metaKey: true });
    click(host, 2);
    click(host, 3);
    expect(selection.source.toArray()).toEqual([1, 2, 3]);
    click(host, 2);
    expect(selection.source.toArray()).toEqual([1, 3]);
    expect(opens).toEqual([]);
  });

  it('opens on Enter even while items are selected, and keeps the selection', async () => {
    const host = await mountPane();
    const opens = recordOpens();
    selection.source.add([1]);
    send(card(host, 4), activation({ id: 4, index: 3, via: 'enter', shift: true, toggle: true }));
    expect(opens).toHaveLength(1);
    expect(opens[0]?.detail).toEqual({ id: 4, index: 3 });
    expect(selection.source.toArray()).toEqual([1]);
  });

  it('toggles on Space', async () => {
    const host = await mountPane();
    send(card(host, 2), activation({ id: 2, index: 1, via: 'space' }));
    expect(selection.source.toArray()).toEqual([2]);
    send(card(host, 2), activation({ id: 2, index: 1, via: 'space' }));
    expect(selection.source.toArray()).toEqual([]);
  });

  it('lets card-activate keep bubbling above the host', async () => {
    const host = await mountPane();
    const seen: Event[] = [];
    const listen = (event: Event): void => {
      seen.push(event);
    };
    document.body.addEventListener('card-activate', listen);
    stops.push(() => document.body.removeEventListener('card-activate', listen));
    send(card(host, 1), activation({ via: 'checkbox' }));
    expect(seen).toHaveLength(1);
  });

  it('only changes the selection of its own pane', async () => {
    const source = await mountPane('source');
    const comparison = await mountPane('comparison');
    click(comparison, 5, { metaKey: true });
    expect(selection.comparison.toArray()).toEqual([5]);
    expect(selection.source.count.value).toBe(0);
    click(source, 1, { ctrlKey: true });
    expect(selection.source.toArray()).toEqual([1]);
    expect(selection.comparison.toArray()).toEqual([5]);
  });

  const MALFORMED: Array<[string, unknown]> = [
    ['no detail', undefined],
    ['a string detail', 'card'],
    ['no id', { index: 0, via: 'card', shift: false, toggle: true }],
    ['a fractional id', { id: 1.5, index: 0, via: 'card', shift: false, toggle: true }],
    ['a string id', { id: '3', index: 0, via: 'card', shift: false, toggle: true }],
    ['no index', { id: 3, via: 'card', shift: false, toggle: true }],
    ['an unknown via', { id: 3, index: 0, via: 'click', shift: false, toggle: true }],
    ['a non-boolean shift', { id: 3, index: 0, via: 'card', shift: 1, toggle: true }],
    ['a non-boolean toggle', { id: 3, index: 0, via: 'card', shift: false, toggle: 'yes' }],
  ];

  it.each(MALFORMED)('ignores an event with %s', async (_name, detail) => {
    const host = await mountPane();
    const opens = recordOpens();
    send(card(host, 1), detail);
    expect(selection.source.count.value).toBe(0);
    expect(opens).toEqual([]);
  });
});

describe('SelectionController: ranges', () => {
  it('selects from the last clicked item to the item clicked with Shift', async () => {
    const host = await mountPane();
    selection.source.setIdsProvider(answering(ITEMS));
    click(host, 2, { metaKey: true });
    click(host, 4, { shiftKey: true });
    await flush();
    expect(members(selection.source)).toEqual([2, 3, 4]);
    expect(selection.source.busy.value).toBe(false);
    click(host, 1, { shiftKey: true });
    await flush();
    expect(members(selection.source)).toEqual([1, 2, 3, 4]);
  });

  it('selects just the item when Shift is held and nothing was clicked before', async () => {
    const host = await mountPane();
    selection.source.setIdsProvider(answering(ITEMS));
    click(host, 3, { shiftKey: true });
    await flush();
    expect(members(selection.source)).toEqual([3]);
  });

  it('lets Shift win over Cmd', async () => {
    const host = await mountPane();
    selection.source.setIdsProvider(answering(ITEMS));
    click(host, 1, { metaKey: true });
    click(host, 3, { shiftKey: true, metaKey: true });
    await flush();
    expect(members(selection.source)).toEqual([1, 2, 3]);
  });

  it('selects a range on Shift+Space and on a Shift-click on a checkbox', async () => {
    const host = await mountPane();
    selection.source.setIdsProvider(answering(ITEMS));
    send(card(host, 2), activation({ id: 2, index: 1, via: 'space' }));
    send(card(host, 4), activation({ id: 4, index: 3, via: 'space', shift: true }));
    await flush();
    expect(members(selection.source)).toEqual([2, 3, 4]);
    send(card(host, 5), activation({ id: 5, index: 4, via: 'checkbox', shift: true }));
    await flush();
    expect(members(selection.source)).toEqual([2, 3, 4, 5]);
  });

  it('reports busy while the list of ids is on its way', async () => {
    const host = await mountPane();
    const { provider, answer } = pending();
    selection.source.setIdsProvider(provider);
    click(host, 1, { metaKey: true });
    click(host, 3, { shiftKey: true });
    await host.updateComplete;
    expect(host.selecting.busy).toBe(true);
    expect(text(host, '.busy')).toBe('true');
    answer(ITEMS);
    await flush();
    await host.updateComplete;
    expect(host.selecting.busy).toBe(false);
    expect(text(host, '.busy')).toBe('false');
    expect(members(selection.source)).toEqual([1, 2, 3]);
  });
});

describe('SelectionController: active pane', () => {
  it('marks its pane active on pointerdown inside it', async () => {
    const source = await mountPane('source');
    const comparison = await mountPane('comparison');
    card(comparison, 1).dispatchEvent(new Event('pointerdown', { bubbles: true }));
    expect(activePane.value).toBe('comparison');
    card(source, 1).dispatchEvent(new Event('pointerdown', { bubbles: true }));
    expect(activePane.value).toBe('source');
  });

  it('marks its pane active on focusin inside it', async () => {
    await mountPane('source');
    const comparison = await mountPane('comparison');
    card(comparison, 2).dispatchEvent(new FocusEvent('focusin', { bubbles: true }));
    expect(activePane.value).toBe('comparison');
  });

  it('still sees a pointerdown that a card stops from propagating', async () => {
    const comparison = await mountPane('comparison');
    const button = card(comparison, 1);
    button.addEventListener('pointerdown', (event) => event.stopPropagation());
    button.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    expect(activePane.value).toBe('comparison');
  });

  it('ignores pointerdown outside the pane', async () => {
    await mountPane('comparison');
    document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    expect(activePane.value).toBe('source');
  });

  it('renders isActivePane for its own pane and follows the active pane', async () => {
    const source = await mountPane('source');
    const comparison = await mountPane('comparison');
    expect(text(source, '.current')).toBe('true');
    expect(text(comparison, '.current')).toBe('false');
    activePane.value = 'comparison';
    await Promise.all([source.updateComplete, comparison.updateComplete]);
    expect(text(source, '.current')).toBe('false');
    expect(text(comparison, '.current')).toBe('true');
  });
});

describe('SelectionController: bottom bar actions', () => {
  it('selectAll selects every id of its pane and marks the pane active', async () => {
    const comparison = await mountPane('comparison');
    selection.comparison.setIdsProvider(answering([10, 20, 30]));
    await comparison.selecting.selectAll();
    expect(members(selection.comparison)).toEqual([10, 20, 30]);
    expect(selection.source.count.value).toBe(0);
    expect(activePane.value).toBe('comparison');
  });

  it('clear empties its pane and marks the pane active', async () => {
    const source = await mountPane('source');
    selection.source.add([1, 2]);
    activePane.value = 'comparison';
    source.selecting.clear();
    expect(selection.source.count.value).toBe(0);
    expect(activePane.value).toBe('source');
  });

  it('clear leaves the other pane alone', async () => {
    const source = await mountPane('source');
    selection.source.add([1]);
    selection.comparison.add([2]);
    source.selecting.clear();
    expect(selection.comparison.toArray()).toEqual([2]);
  });
});

describe('SelectionController: rendering', () => {
  it('re-renders the host when the selection changes', async () => {
    const host = await mountPane();
    expect(text(host, '.count')).toBe('0');
    expect(text(host, '.active')).toBe('false');
    expect(card(host, 2).getAttribute('aria-pressed')).toBe('false');
    selection.source.toggle(2);
    await host.updateComplete;
    expect(text(host, '.count')).toBe('1');
    expect(text(host, '.active')).toBe('true');
    expect(card(host, 2).getAttribute('aria-pressed')).toBe('true');
    expect(card(host, 1).getAttribute('aria-pressed')).toBe('false');
    selection.source.clear();
    await host.updateComplete;
    expect(text(host, '.count')).toBe('0');
    expect(card(host, 2).getAttribute('aria-pressed')).toBe('false');
  });

  it('hands out a new Set on every change, so the grid sees a new value', async () => {
    const host = await mountPane();
    const before = host.selecting.selectedIds;
    click(host, 1, { metaKey: true });
    expect(host.selecting.selectedIds).not.toBe(before);
    expect([...host.selecting.selectedIds]).toEqual([1]);
    expect([...before]).toEqual([]);
  });

  it('follows its pane property', async () => {
    const host = await mountPane('source');
    selection.comparison.add([1, 2, 3]);
    host.pane = 'comparison';
    await host.updateComplete;
    expect(host.selecting.pane).toBe('comparison');
    expect(host.selecting.selection).toBe(selection.comparison);
    expect(text(host, '.count')).toBe('3');
    click(host, 4, { metaKey: true });
    expect(selection.comparison.toArray()).toEqual([1, 2, 3, 4]);
    expect(selection.source.count.value).toBe(0);
  });
});

describe('SelectionController: lifecycle', () => {
  it('stops listening when the host is disconnected', async () => {
    const host = await mountPane('comparison');
    const button = card(host, 1);
    host.remove();
    send(button, activation({ id: 1, via: 'checkbox' }));
    button.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    button.dispatchEvent(new FocusEvent('focusin', { bubbles: true }));
    expect(selection.comparison.count.value).toBe(0);
    expect(activePane.value).toBe('source');
  });

  it('listens again, and only once, when the host is reconnected', async () => {
    const host = await mountPane();
    host.remove();
    document.body.append(host);
    await host.updateComplete;
    send(card(host, 2), activation({ id: 2, index: 1, via: 'checkbox' }));
    expect(selection.source.toArray()).toEqual([2]);
  });

  it('keeps the selection when the host is disconnected', async () => {
    const host = await mountPane();
    click(host, 1, { metaKey: true });
    host.remove();
    expect(selection.source.toArray()).toEqual([1]);
  });

  it('falls back to the source pane when the active comparison pane goes away', async () => {
    const comparison = await mountPane('comparison');
    activePane.value = 'comparison';
    comparison.remove();
    expect(activePane.value).toBe('source');
  });

  it('leaves the active pane alone when another pane goes away', async () => {
    const source = await mountPane('source');
    const comparison = await mountPane('comparison');
    activePane.value = 'comparison';
    source.remove();
    expect(activePane.value).toBe('comparison');
    activePane.value = 'source';
    comparison.remove();
    expect(activePane.value).toBe('source');
  });

  it('starts listening at once when added to a host that is already connected', async () => {
    const host = await mount('apm-test-selection-bare');
    const controller = new SelectionController(host, { pane: 'comparison' });
    send(host, activation({ id: 6, via: 'checkbox' }));
    expect(controller.pane).toBe('comparison');
    expect(selection.comparison.toArray()).toEqual([6]);
  });

  it('drives the selection it is given instead of the registry one', async () => {
    const host = await mount('apm-test-selection-bare');
    const own = new PaneSelection();
    const controller = new SelectionController(host, { pane: 'source', selection: own });
    send(host, activation({ id: 7, via: 'checkbox' }));
    expect(own.toArray()).toEqual([7]);
    expect(selection.source.count.value).toBe(0);
    expect(controller.selection).toBe(own);
    expect(controller.count).toBe(1);
    expect(controller.active).toBe(true);
    expect([...controller.selectedIds]).toEqual([7]);
    controller.clear();
    expect(own.count.value).toBe(0);
  });

  it('defaults to the registry selection of its pane', async () => {
    const host = await mount('apm-test-selection-bare');
    const controller = new SelectionController(host, { pane: 'source' });
    expect(controller.selection).toBe(selection.source);
  });
});
