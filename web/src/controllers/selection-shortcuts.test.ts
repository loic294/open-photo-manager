import { afterEach, describe, expect, it } from 'vitest';
import { installSelectionShortcuts } from '@/controllers/selection-shortcuts';
import {
  activePane,
  type PaneId,
  type PaneSelection,
  resetSelectionForTests,
  selection,
} from '@/stores/selection';

const ITEMS = [1, 2, 3, 4, 5];

const stops: Array<() => void> = [];

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  resetSelectionForTests();
});

function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function members(pane: PaneSelection): number[] {
  return [...pane.ids.value].sort((a, b) => a - b);
}

function install(target?: Document | HTMLElement): () => void {
  const uninstall = installSelectionShortcuts(target);
  stops.push(uninstall);
  return uninstall;
}

/** Puts a counting provider on a pane: `select-all` is the only thing that calls it. */
function arm(pane: PaneId = 'source', ids: number[] = ITEMS): () => number {
  let calls = 0;
  selection[pane].setIdsProvider(async () => {
    calls += 1;
    return ids;
  });
  return () => calls;
}

function put<T extends HTMLElement>(element: T, parent: ParentNode = document.body): T {
  parent.append(element);
  stops.push(() => element.remove());
  return element;
}

function make(tag: string, attributes: Record<string, string> = {}): HTMLElement {
  const element = document.createElement(tag);
  for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, value);
  return element;
}

function press(target: EventTarget, init: KeyboardEventInit): KeyboardEvent {
  const event = new KeyboardEvent('keydown', {
    bubbles: true,
    cancelable: true,
    composed: true,
    ...init,
  });
  target.dispatchEvent(event);
  return event;
}

function selectAllKey(overrides: KeyboardEventInit = {}): KeyboardEventInit {
  return { key: 'a', metaKey: true, ...overrides };
}

const TEXT_TARGETS: Array<[string, () => HTMLElement]> = [
  ['an input with no type', () => make('input')],
  ['a text input', () => make('input', { type: 'text' })],
  ['a search input', () => make('input', { type: 'search' })],
  ['an email input', () => make('input', { type: 'email' })],
  ['a number input', () => make('input', { type: 'number' })],
  ['a textarea', () => make('textarea')],
  ['a select', () => make('select')],
  ['a contenteditable element', () => make('div', { contenteditable: '' })],
  ['a contenteditable="true" element', () => make('div', { contenteditable: 'true' })],
  ['an element with role=textbox', () => make('div', { role: 'textbox' })],
];

describe('installSelectionShortcuts: Cmd/Ctrl+A', () => {
  it('selects every item of the active pane with Cmd+A and keeps the page text unselected', async () => {
    install();
    const calls = arm();
    const event = press(document.body, selectAllKey());
    await flush();
    expect(event.defaultPrevented).toBe(true);
    expect(calls()).toBe(1);
    expect(members(selection.source)).toEqual(ITEMS);
  });

  it('does the same with Ctrl+A and with a capital A', async () => {
    install();
    arm();
    press(document.body, { key: 'a', ctrlKey: true });
    await flush();
    expect(members(selection.source)).toEqual(ITEMS);
    selection.source.clear();
    press(document.body, { key: 'A', metaKey: true });
    await flush();
    expect(members(selection.source)).toEqual(ITEMS);
  });

  it('targets the pane the user touched last', async () => {
    install();
    const sourceCalls = arm('source', [1, 2]);
    const comparisonCalls = arm('comparison', [7, 8, 9]);
    activePane.value = 'comparison';
    press(document.body, selectAllKey());
    await flush();
    expect(members(selection.comparison)).toEqual([7, 8, 9]);
    expect(selection.source.count.value).toBe(0);
    expect(sourceCalls()).toBe(0);
    expect(comparisonCalls()).toBe(1);
  });

  it('reports busy while the list is on its way', async () => {
    install();
    let answer: (ids: number[]) => void = () => undefined;
    selection.source.setIdsProvider(
      () =>
        new Promise<number[]>((settle) => {
          answer = settle;
        }),
    );
    press(document.body, selectAllKey());
    expect(selection.source.busy.value).toBe(true);
    answer(ITEMS);
    await flush();
    expect(selection.source.busy.value).toBe(false);
    expect(members(selection.source)).toEqual(ITEMS);
  });

  it.each([
    ['a plain A', { key: 'a' }],
    ['Shift+Cmd+A', selectAllKey({ shiftKey: true })],
    ['Alt+Cmd+A', selectAllKey({ altKey: true })],
    ['Cmd+B', { key: 'b', metaKey: true }],
    ['Cmd alone', { key: 'Meta', metaKey: true }],
  ])('ignores %s', async (_name, init) => {
    install();
    const calls = arm();
    const event = press(document.body, init);
    await flush();
    expect(event.defaultPrevented).toBe(false);
    expect(calls()).toBe(0);
    expect(selection.source.count.value).toBe(0);
  });

  it.each(TEXT_TARGETS)('leaves Cmd+A to %s', async (_name, create) => {
    install();
    const calls = arm();
    const event = press(put(create()), selectAllKey());
    await flush();
    expect(event.defaultPrevented).toBe(false);
    expect(calls()).toBe(0);
    expect(selection.source.count.value).toBe(0);
  });

  it('leaves Cmd+A to an element inside editable content', async () => {
    install();
    const calls = arm();
    const editor = put(make('div', { contenteditable: 'true' }));
    const bold = make('b');
    editor.append(bold);
    const event = press(bold, selectAllKey());
    await flush();
    expect(event.defaultPrevented).toBe(false);
    expect(calls()).toBe(0);
  });

  it('leaves Cmd+A to a text field inside a shadow root', async () => {
    install();
    const calls = arm();
    const host = put(make('div'));
    const field = make('input', { type: 'text' });
    host.attachShadow({ mode: 'open' }).append(field);
    const event = press(field, selectAllKey());
    await flush();
    expect(event.defaultPrevented).toBe(false);
    expect(calls()).toBe(0);
  });

  it.each(['checkbox', 'radio', 'range', 'button', 'submit', 'reset', 'color', 'file', 'image'])(
    'still handles Cmd+A while a %s input has the focus',
    async (type) => {
      install();
      arm();
      const event = press(put(make('input', { type })), selectAllKey());
      await flush();
      expect(event.defaultPrevented).toBe(true);
      expect(members(selection.source)).toEqual(ITEMS);
    },
  );

  it('still handles Cmd+A while a button or an editable="false" element has the focus', async () => {
    install();
    arm();
    press(put(make('button', { type: 'button' })), selectAllKey());
    await flush();
    expect(members(selection.source)).toEqual(ITEMS);
    selection.source.clear();
    press(put(make('div', { contenteditable: 'false' })), selectAllKey());
    await flush();
    expect(members(selection.source)).toEqual(ITEMS);
  });

  it('stands aside while a modal dialog is open', async () => {
    install();
    const calls = arm();
    const dialog = put(make('dialog'));
    dialog.setAttribute('open', '');
    const event = press(document.body, selectAllKey());
    await flush();
    expect(event.defaultPrevented).toBe(false);
    expect(calls()).toBe(0);
    dialog.removeAttribute('open');
    press(document.body, selectAllKey());
    await flush();
    expect(calls()).toBe(1);
  });

  it('ignores an event that something already handled', async () => {
    install();
    const calls = arm();
    const target = put(make('div'));
    target.addEventListener('keydown', (event) => event.preventDefault());
    press(target, selectAllKey());
    await flush();
    expect(calls()).toBe(0);
  });

  it('ignores keydown events that are not keyboard events', async () => {
    install();
    const calls = arm();
    document.body.dispatchEvent(new Event('keydown', { bubbles: true }));
    await flush();
    expect(calls()).toBe(0);
  });
});

describe('installSelectionShortcuts: Escape', () => {
  it('clears the active pane, and does not cancel the event', () => {
    install();
    selection.source.add([1, 2, 3]);
    const event = press(document.body, { key: 'Escape' });
    expect(selection.source.count.value).toBe(0);
    expect(event.defaultPrevented).toBe(false);
  });

  it('clears the other pane only when the active one has nothing to clear', () => {
    install();
    selection.source.add([1, 2]);
    selection.comparison.add([3]);
    press(document.body, { key: 'Escape' });
    expect(selection.source.count.value).toBe(0);
    expect(selection.comparison.toArray()).toEqual([3]);
    press(document.body, { key: 'Escape' });
    expect(selection.comparison.count.value).toBe(0);
  });

  it('starts with the comparison pane when it is the active one', () => {
    install();
    selection.source.add([1]);
    selection.comparison.add([2]);
    activePane.value = 'comparison';
    press(document.body, { key: 'Escape' });
    expect(selection.comparison.count.value).toBe(0);
    expect(selection.source.toArray()).toEqual([1]);
    press(document.body, { key: 'Escape' });
    expect(selection.source.count.value).toBe(0);
  });

  it('does nothing when nothing is selected', () => {
    install();
    activePane.value = 'comparison';
    expect(() => press(document.body, { key: 'Escape' })).not.toThrow();
    expect(selection.source.count.value).toBe(0);
    expect(selection.comparison.count.value).toBe(0);
    expect(activePane.value).toBe('comparison');
  });

  it('cancels a select-all that is still waiting for its list', async () => {
    install();
    let answer: (ids: number[]) => void = () => undefined;
    selection.source.setIdsProvider(
      () =>
        new Promise<number[]>((settle) => {
          answer = settle;
        }),
    );
    press(document.body, selectAllKey());
    expect(selection.source.busy.value).toBe(true);
    press(document.body, { key: 'Escape' });
    expect(selection.source.busy.value).toBe(false);
    answer(ITEMS);
    await flush();
    expect(selection.source.count.value).toBe(0);
  });

  it.each([
    ['Shift+Escape', { shiftKey: true }],
    ['Ctrl+Escape', { ctrlKey: true }],
    ['Cmd+Escape', { metaKey: true }],
    ['Alt+Escape', { altKey: true }],
  ])('ignores %s', (_name, modifiers) => {
    install();
    selection.source.add([1, 2]);
    press(document.body, { key: 'Escape', ...modifiers });
    expect(selection.source.count.value).toBe(2);
  });

  it.each(TEXT_TARGETS)('leaves Escape to %s', (_name, create) => {
    install();
    selection.source.add([1, 2]);
    press(put(create()), { key: 'Escape' });
    expect(selection.source.count.value).toBe(2);
  });

  it('still clears while a checkbox has the focus', () => {
    install();
    selection.source.add([1, 2]);
    press(put(make('input', { type: 'checkbox' })), { key: 'Escape' });
    expect(selection.source.count.value).toBe(0);
  });

  it('stands aside while a modal dialog is open', () => {
    install();
    selection.source.add([1, 2]);
    put(make('dialog', { open: '' }));
    press(document.body, { key: 'Escape' });
    expect(selection.source.count.value).toBe(2);
  });

  it('ignores an Escape that something already handled', () => {
    install();
    selection.source.add([1, 2]);
    const target = put(make('div'));
    target.addEventListener('keydown', (event) => event.preventDefault());
    press(target, { key: 'Escape' });
    expect(selection.source.count.value).toBe(2);
  });
});

describe('installSelectionShortcuts: installation', () => {
  it('stops handling keys once uninstalled', async () => {
    const uninstall = install();
    const calls = arm();
    selection.source.add([9]);
    uninstall();
    const event = press(document.body, selectAllKey());
    press(document.body, { key: 'Escape' });
    await flush();
    expect(event.defaultPrevented).toBe(false);
    expect(calls()).toBe(0);
    expect(selection.source.toArray()).toEqual([9]);
  });

  it('can be uninstalled twice and reinstalled', async () => {
    const uninstall = install();
    uninstall();
    expect(() => uninstall()).not.toThrow();
    install();
    arm();
    press(document.body, selectAllKey());
    await flush();
    expect(members(selection.source)).toEqual(ITEMS);
  });

  it('handles keys inside an element it is installed on, and only inside it', async () => {
    const root = put(make('div'));
    const inside = put(make('div'), root);
    install(root);
    const calls = arm();
    press(document.body, selectAllKey());
    await flush();
    expect(calls()).toBe(0);
    const event = press(inside, selectAllKey());
    await flush();
    expect(event.defaultPrevented).toBe(true);
    expect(members(selection.source)).toEqual(ITEMS);
  });

  it('still looks for an open dialog in the whole document when installed on an element', async () => {
    const root = put(make('div'));
    install(root);
    const calls = arm();
    put(make('dialog', { open: '' }));
    press(root, selectAllKey());
    await flush();
    expect(calls()).toBe(0);
  });

  it('is independent of the controllers: it works with no pane mounted', async () => {
    install();
    arm('comparison', [4, 5]);
    activePane.value = 'comparison';
    press(document.body, selectAllKey());
    await flush();
    expect(members(selection.comparison)).toEqual([4, 5]);
  });
});
