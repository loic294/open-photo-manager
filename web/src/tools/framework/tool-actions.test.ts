import { effect, signal } from '@preact/signals-core';
import { nothing, render } from 'lit';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { OpmSplitButton } from '@/components/opm-split-button';
import { lastAction, setLastAction } from '@/stores/prefs';
import { clearToasts, toasts } from '@/stores/toasts';
import { toolConfig } from '@/test/tool-config';
import { renderActions } from '@/tools/framework/tool-actions';
import type { ToolAction, ToolActions } from '@/tools/framework/tool-config';
import { createToolContext, type ToolContext } from '@/tools/framework/tool-context';
import { formatCount } from '@/util/format-count';

const contexts: ToolContext[] = [];
const stops: (() => void)[] = [];
const containers: HTMLElement[] = [];

beforeEach(() => {
  lastAction.value = {};
  localStorage.clear();
});

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  for (const target of containers.splice(0)) target.remove();
  for (const ctx of contexts.splice(0)) ctx.dispose();
  clearToasts();
  vi.restoreAllMocks();
  lastAction.value = {};
});

/** A context for a tool (`move`, whose last action the split button remembers) with `actions`. */
function contextFor(actions?: (ctx: ToolContext) => ToolActions | null): ToolContext {
  const ctx = createToolContext(toolConfig({ id: 'move', actions }), document.createElement('div'));
  contexts.push(ctx);
  return ctx;
}

function action(id: string, patch: Partial<ToolAction> = {}): ToolAction {
  return { id, label: `Action ${id}`, run: vi.fn(async () => {}), ...patch };
}

/** The actions of a tool whose `options` are `ids` (the first is the default). */
function actionsOf(...ids: string[]): ToolActions {
  return { options: ids.map((id) => action(id)), defaultId: ids[0] ?? '' };
}

/** Renders the actions now and again whenever a signal they read changes. */
function show(ctx: ToolContext): HTMLElement {
  const target = document.createElement('div');
  document.body.append(target);
  containers.push(target);
  stops.push(
    effect(() => {
      render(renderActions(ctx) ?? nothing, target);
    }),
  );
  return target;
}

/** Waits for the split buttons in `area` to render what they were given. */
async function settle(area: HTMLElement): Promise<void> {
  await Promise.resolve();
  const splits = area.querySelectorAll<OpmSplitButton>('opm-split-button');
  await Promise.all([...splits].map((split) => split.updateComplete));
}

const flush = () => new Promise<void>((resolve) => setTimeout(resolve));

function deferred() {
  let resolve!: () => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function part<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`No element matches "${selector}"`);
  return found;
}

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

const button = (area: HTMLElement) => part<HTMLButtonElement>(area, 'button');
const split = (area: HTMLElement) => part<OpmSplitButton>(area, 'opm-split-button');
const mainButton = (area: HTMLElement) => part<HTMLButtonElement>(split(area), '.join > button');

/** The wrapper that carries the tooltip: its `data-tip`, `null` when there is none. */
const tipOf = (area: HTMLElement) => part(area, 'div.inline-flex').getAttribute('data-tip');

const errorToasts = () =>
  toasts.value.filter((entry) => entry.kind === 'error').map((entry) => entry.message);

describe('no actions', () => {
  it('renders nothing for a tool without actions', () => {
    expect(renderActions(contextFor())).toBeUndefined();
  });

  it('renders nothing while the tool has none to offer', () => {
    expect(renderActions(contextFor(() => null))).toBeUndefined();
    expect(renderActions(contextFor(() => ({ options: [], defaultId: '' })))).toBeUndefined();
  });

  it('appears and goes with what the tool offers', () => {
    const offered = signal(false);
    const area = show(contextFor(() => (offered.value ? actionsOf('go') : null)));
    expect(area.querySelector('button')).toBeNull();
    offered.value = true;
    expect(button(area).textContent).toContain('Action go');
    offered.value = false;
    expect(area.querySelector('button')).toBeNull();
  });
});

describe('one action', () => {
  it('is a primary button with its icon, label and count', () => {
    const area = show(
      contextFor(() => ({
        options: [action('go', { label: 'Find matches', icon: 'search' })],
        defaultId: 'go',
        count: () => 1234,
      })),
    );
    const go = button(area);
    expect(go.type).toBe('button');
    expect(go.classList.contains('btn-primary')).toBe(true);
    expect(go.classList.contains('btn-lg')).toBe(true);
    expect(go.classList.contains('btn-error')).toBe(false);
    expect(part(go, 'opm-icon').getAttribute('name')).toBe('search');
    expect(text(go)).toBe(`Find matches ${formatCount(1234)}`);
    expect(part(go, '.badge').classList.contains('badge-neutral')).toBe(true);
  });

  it('is red when the action is dangerous', () => {
    const area = show(
      contextFor(() => ({
        options: [action('purge', { danger: true, icon: 'trash-2' })],
        defaultId: 'purge',
      })),
    );
    expect(button(area).classList.contains('btn-error')).toBe(true);
    expect(button(area).classList.contains('btn-primary')).toBe(false);
  });

  it('shows no badge without a count, and no icon without an icon', () => {
    const area = show(contextFor(() => actionsOf('go')));
    expect(area.querySelector('.badge')).toBeNull();
    expect(area.querySelector('opm-icon')).toBeNull();
  });

  it('shows a count of zero', () => {
    const area = show(contextFor(() => ({ ...actionsOf('go'), count: () => 0 })));
    expect(text(part(area, '.badge'))).toBe('0');
  });

  it('ignores a count that is not a number', () => {
    const area = show(contextFor(() => ({ ...actionsOf('go'), count: () => Number.NaN })));
    expect(area.querySelector('.badge')).toBeNull();
  });

  it('follows the signals its count and label read', () => {
    const matches = signal(3);
    const area = show(contextFor(() => ({ ...actionsOf('go'), count: () => matches.value })));
    expect(text(part(area, '.badge'))).toBe('3');
    matches.value = 4000;
    expect(text(part(area, '.badge'))).toBe(formatCount(4000));
  });

  it('runs on a click, with the context', async () => {
    const go = action('go');
    const ctx = contextFor(() => ({ options: [go], defaultId: 'go' }));
    const area = show(ctx);
    button(area).click();
    await flush();
    expect(go.run).toHaveBeenCalledTimes(1);
    expect(go.run).toHaveBeenCalledWith(ctx);
  });
});

describe('the tooltip', () => {
  it('shows why an action is disabled, and disables the button', () => {
    const area = show(
      contextFor(() => ({
        options: [action('go', { disabled: () => 'Choose a base folder first' })],
        defaultId: 'go',
      })),
    );
    expect(button(area).disabled).toBe(true);
    const wrapper = part(area, 'div.inline-flex');
    expect(wrapper.classList.contains('tooltip')).toBe(true);
    expect(wrapper.classList.contains('tooltip-left')).toBe(true);
    expect(wrapper.getAttribute('data-tip')).toBe('Choose a base folder first');
  });

  it('ties the reason to the button for a screen reader', () => {
    const area = show(
      contextFor(() => ({
        options: [action('go', { disabled: () => 'Nothing to move' })],
        defaultId: 'go',
      })),
    );
    const id = button(area).getAttribute('aria-describedby');
    expect(id).toBe('move-action-hint');
    const hint = part(area, `#${id}`);
    expect(hint.classList.contains('sr-only')).toBe(true);
    expect(text(hint)).toBe('Nothing to move');
  });

  it('shows the static hint of an enabled action', () => {
    const area = show(
      contextFor(() => ({
        options: [action('go', { hint: 'Moves every matched file' })],
        defaultId: 'go',
      })),
    );
    expect(button(area).disabled).toBe(false);
    expect(tipOf(area)).toBe('Moves every matched file');
    expect(button(area).getAttribute('aria-describedby')).toBe('move-action-hint');
  });

  it('prefers the reason to the hint', () => {
    const area = show(
      contextFor(() => ({
        options: [action('go', { hint: 'A hint', disabled: () => 'A reason' })],
        defaultId: 'go',
      })),
    );
    expect(tipOf(area)).toBe('A reason');
  });

  it('has no tooltip without a reason or a hint', () => {
    const area = show(contextFor(() => actionsOf('go')));
    const wrapper = part(area, 'div.inline-flex');
    expect(wrapper.classList.contains('tooltip')).toBe(false);
    expect(wrapper.hasAttribute('data-tip')).toBe(false);
    expect(button(area).hasAttribute('aria-describedby')).toBe(false);
    expect(area.querySelector('.sr-only')).toBeNull();
  });

  it('follows the signals the reason reads', () => {
    const ready = signal(false);
    const area = show(
      contextFor(() => ({
        options: [action('go', { disabled: () => (ready.value ? null : 'Not ready') })],
        defaultId: 'go',
      })),
    );
    expect(button(area).disabled).toBe(true);
    expect(tipOf(area)).toBe('Not ready');
    ready.value = true;
    expect(button(area).disabled).toBe(false);
    expect(part(area, 'div.inline-flex').hasAttribute('data-tip')).toBe(false);
  });
});

describe('running an action', () => {
  it('keeps the bar busy until the action settles, and ignores a second click', async () => {
    const pending = deferred();
    const go = action('go', { run: vi.fn(() => pending.promise) });
    const area = show(contextFor(() => ({ options: [go], defaultId: 'go' })));
    button(area).click();
    expect(button(area).disabled).toBe(true);
    expect(button(area).getAttribute('aria-busy')).toBe('true');
    expect(area.querySelector('.loading')).not.toBeNull();
    button(area).dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await flush();
    expect(go.run).toHaveBeenCalledTimes(1);
    pending.resolve();
    await flush();
    expect(button(area).disabled).toBe(false);
    expect(button(area).hasAttribute('aria-busy')).toBe(false);
    expect(area.querySelector('.loading')).toBeNull();
    expect(errorToasts()).toEqual([]);
  });

  it('keeps the busy state per context', () => {
    const pending = deferred();
    const go = action('go', { run: vi.fn(() => pending.promise) });
    const first = show(contextFor(() => ({ options: [go], defaultId: 'go' })));
    const second = show(contextFor(() => ({ options: [go], defaultId: 'go' })));
    button(first).click();
    expect(button(first).disabled).toBe(true);
    expect(button(second).disabled).toBe(false);
    pending.resolve();
  });

  it('reports a failure as one error toast with the message of the error', async () => {
    const go = action('go', { run: vi.fn(async () => Promise.reject(new Error('Disk is full'))) });
    const area = show(contextFor(() => ({ options: [go], defaultId: 'go' })));
    button(area).click();
    await flush();
    expect(errorToasts()).toEqual(['Disk is full']);
    expect(button(area).disabled).toBe(false);
  });

  it('can run again after a failure', async () => {
    const run = vi.fn<ToolAction['run']>().mockRejectedValueOnce(new Error('Nope'));
    const area = show(contextFor(() => ({ options: [action('go', { run })], defaultId: 'go' })));
    button(area).click();
    await flush();
    button(area).click();
    await flush();
    expect(run).toHaveBeenCalledTimes(2);
    expect(errorToasts()).toEqual(['Nope']);
  });

  it('says something even when the rejection has no message', async () => {
    const go = action('go', { run: vi.fn(async () => Promise.reject(undefined)) });
    const area = show(contextFor(() => ({ options: [go], defaultId: 'go' })));
    button(area).click();
    await flush();
    expect(errorToasts()).toEqual(['Something went wrong']);
  });

  it('stays silent when the action was aborted', async () => {
    const aborted = Object.assign(new Error('The operation was aborted'), { name: 'AbortError' });
    const go = action('go', { run: vi.fn(async () => Promise.reject(aborted)) });
    const area = show(contextFor(() => ({ options: [go], defaultId: 'go' })));
    button(area).click();
    await flush();
    expect(go.run).toHaveBeenCalledTimes(1);
    expect(toasts.value).toEqual([]);
    expect(button(area).disabled).toBe(false);
  });

  it('cannot run an action while its button is disabled', async () => {
    const go = action('go', { disabled: () => 'Not now' });
    const area = show(contextFor(() => ({ options: [go], defaultId: 'go' })));
    button(area).dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await flush();
    expect(go.run).not.toHaveBeenCalled();
  });
});

describe('several actions', () => {
  it('is a split button that remembers the choice per tool', async () => {
    const area = show(
      contextFor(() => ({
        options: [action('a', { icon: 'copy' }), action('b', { danger: true, hint: 'Careful' })],
        defaultId: 'b',
        count: () => 12,
      })),
    );
    await settle(area);
    const element = split(area);
    expect(element.getAttribute('persist-key')).toBe('move');
    expect(element.getAttribute('default-id')).toBe('b');
    expect(element.count).toBe(12);
    expect(element.busy).toBe(false);
    expect(element.options).toEqual([
      { id: 'a', label: 'Action a', icon: 'copy', disabled: false },
      { id: 'b', label: 'Action b', danger: true, disabled: false, hint: 'Careful' },
    ]);
  });

  it('shows the default entry until the user chose another', async () => {
    const area = show(contextFor(() => ({ ...actionsOf('a', 'b'), defaultId: 'b' })));
    await settle(area);
    expect(text(mainButton(area))).toBe('Action b');
    setLastAction('move', 'a');
    await settle(area);
    expect(text(mainButton(area))).toBe('Action a');
  });

  it('runs the shown entry when the main button is pressed', async () => {
    const a = action('a');
    const b = action('b');
    const ctx = contextFor(() => ({ options: [a, b], defaultId: 'a' }));
    setLastAction('move', 'b');
    const area = show(ctx);
    await settle(area);
    mainButton(area).click();
    await flush();
    expect(b.run).toHaveBeenCalledTimes(1);
    expect(b.run).toHaveBeenCalledWith(ctx);
    expect(a.run).not.toHaveBeenCalled();
  });

  it('only selects an entry from the menu: the user presses the main button to run it', async () => {
    const a = action('a');
    const b = action('b');
    const area = show(contextFor(() => ({ options: [a, b], defaultId: 'a' })));
    await settle(area);
    part<HTMLElement>(split(area), 'details > summary').click();
    await settle(area);
    const entry = [...split(area).querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]')];
    entry.find((candidate) => text(candidate) === 'Action b')?.click();
    await settle(area);
    await flush();
    expect(lastAction.value).toEqual({ move: 'b' });
    expect(a.run).not.toHaveBeenCalled();
    expect(b.run).not.toHaveBeenCalled();
    expect(text(mainButton(area))).toBe('Action b');
  });

  it('ignores an action event for an entry it does not know', async () => {
    const a = action('a');
    const area = show(contextFor(() => ({ options: [a, action('b')], defaultId: 'a' })));
    await settle(area);
    split(area).dispatchEvent(
      new CustomEvent('split-action', { detail: { optionId: 'zzz' }, bubbles: true }),
    );
    await flush();
    expect(a.run).not.toHaveBeenCalled();
  });

  it('does not run an entry that is disabled', async () => {
    const b = action('b', { disabled: () => 'Not now' });
    const area = show(contextFor(() => ({ options: [action('a'), b], defaultId: 'a' })));
    await settle(area);
    split(area).dispatchEvent(
      new CustomEvent('split-action', { detail: { optionId: 'b' }, bubbles: true }),
    );
    await flush();
    expect(b.run).not.toHaveBeenCalled();
  });

  it('gives a disabled entry its reason as the hint', async () => {
    const area = show(
      contextFor(() => ({
        options: [action('a'), action('b', { hint: 'A hint', disabled: () => 'A reason' })],
        defaultId: 'a',
      })),
    );
    await settle(area);
    expect(split(area).options[1]).toMatchObject({ id: 'b', disabled: true, hint: 'A reason' });
  });

  it('keeps the bar busy while an entry runs', async () => {
    const pending = deferred();
    const a = action('a', { run: vi.fn(() => pending.promise) });
    const area = show(contextFor(() => ({ options: [a, action('b')], defaultId: 'a' })));
    await settle(area);
    mainButton(area).click();
    await settle(area);
    expect(split(area).busy).toBe(true);
    expect(mainButton(area).disabled).toBe(true);
    pending.resolve();
    await flush();
    await settle(area);
    expect(split(area).busy).toBe(false);
  });

  it('reports a failure as one error toast', async () => {
    const a = action('a', { run: vi.fn(async () => Promise.reject(new Error('Disk is full'))) });
    const area = show(contextFor(() => ({ options: [a, action('b')], defaultId: 'a' })));
    await settle(area);
    mainButton(area).click();
    await flush();
    expect(errorToasts()).toEqual(['Disk is full']);
  });

  it('follows the signals the count and the entries read', async () => {
    const matches = signal(1);
    const open = signal(false);
    const area = show(
      contextFor(() => ({
        options: [action('a'), action('b', { disabled: () => (open.value ? null : 'Closed') })],
        defaultId: 'a',
        count: () => matches.value,
      })),
    );
    await settle(area);
    expect(split(area).count).toBe(1);
    expect(split(area).options[1]?.disabled).toBe(true);
    matches.value = 7;
    open.value = true;
    await settle(area);
    expect(split(area).count).toBe(7);
    expect(split(area).options[1]?.disabled).toBe(false);
  });
});

describe('a split button with nothing to run', () => {
  const reasons = () => ({
    options: ['a', 'b', 'c'].map((id) => action(id, { disabled: () => `Reason ${id}` })),
    defaultId: 'b',
  });

  it('explains the entry it shows: the default one first', async () => {
    const area = show(contextFor(reasons));
    await settle(area);
    expect(tipOf(area)).toBe('Reason b');
    expect(part(area, '#move-action-hint').textContent).toBe('Reason b');
  });

  it('explains the remembered entry when the user chose one', async () => {
    setLastAction('move', 'c');
    const area = show(contextFor(reasons));
    await settle(area);
    expect(tipOf(area)).toBe('Reason c');
  });

  it('falls back to the default when the remembered entry is gone', async () => {
    setLastAction('move', 'gone');
    const area = show(contextFor(reasons));
    await settle(area);
    expect(tipOf(area)).toBe('Reason b');
  });

  it('is gone once an entry can run', async () => {
    const open = signal(false);
    const area = show(
      contextFor(() => ({
        options: [
          action('a', { disabled: () => 'Reason a' }),
          action('b', { disabled: () => (open.value ? null : 'Reason b') }),
        ],
        defaultId: 'a',
      })),
    );
    await settle(area);
    expect(tipOf(area)).toBe('Reason a');
    open.value = true;
    await settle(area);
    expect(part(area, 'div.inline-flex').hasAttribute('data-tip')).toBe(false);
    expect(area.querySelector('#move-action-hint')).toBeNull();
  });
});

describe('a mistake in the actions', () => {
  it('renders nothing and logs it once', () => {
    const log = vi.spyOn(console, 'error').mockImplementation(() => {});
    const ctx = contextFor(() => ({ ...actionsOf('a', 'a') }));
    expect(renderActions(ctx)).toBeUndefined();
    expect(renderActions(ctx)).toBeUndefined();
    expect(log).toHaveBeenCalledTimes(1);
    expect(log).toHaveBeenCalledWith('Tool "move": action id "a" is used twice');
  });

  it('names a default that is not an action', () => {
    const log = vi.spyOn(console, 'error').mockImplementation(() => {});
    const ctx = contextFor(() => ({ ...actionsOf('a', 'b'), defaultId: 'zzz' }));
    expect(renderActions(ctx)).toBeUndefined();
    expect(log).toHaveBeenCalledWith('Tool "move": defaultId "zzz" is not the id of an action');
  });

  it('logs a new problem again, and a new context starts afresh', () => {
    const log = vi.spyOn(console, 'error').mockImplementation(() => {});
    let current: ToolActions = { ...actionsOf('a', 'a') };
    const ctx = contextFor(() => current);
    renderActions(ctx);
    current = { ...actionsOf('a', 'b'), defaultId: 'zzz' };
    renderActions(ctx);
    expect(log).toHaveBeenCalledTimes(2);
    renderActions(contextFor(() => current));
    expect(log).toHaveBeenCalledTimes(3);
  });

  it('renders again once the mistake is fixed', () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const fixed = signal(false);
    const area = show(contextFor(() => (fixed.value ? actionsOf('a') : actionsOf('a', 'a'))));
    expect(area.querySelector('button')).toBeNull();
    fixed.value = true;
    expect(button(area).textContent).toContain('Action a');
  });
});
