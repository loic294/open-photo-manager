import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@/components/opm-split-button';
import type { OpmSplitButton, SplitOption } from '@/components/opm-split-button';
import { STORAGE_PREFIX } from '@/stores/persisted';
import { lastAction } from '@/stores/prefs';
import { mount } from '@/test/helpers';
import { formatCount } from '@/util/format-count';

const OPTIONS: SplitOption[] = [
  { id: 'delete-source', label: 'Delete source items', icon: 'trash-2', danger: true },
  { id: 'move', label: 'Move files', icon: 'folder-input' },
  { id: 'copy', label: 'Copy files', icon: 'copy' },
];

it('keeps the action chooser reachable beside a long label without losing its accessible text', async () => {
  const el = await mount('opm-split-button', {
    options: [{ id: 'delete-comparison', label: 'Delete comparison items', danger: true }],
  });
  expect(el.classList.contains('max-w-[calc(100vw-6rem)]')).toBe(true);
  const button = el.querySelector('.join > button');
  expect(button?.classList.contains('flex-1')).toBe(true);
  expect(button?.getAttribute('title')).toBe('Delete comparison items');
  expect(button?.querySelector('.truncate')?.textContent).toBe('Delete comparison items');
  expect(el.querySelector('details')?.classList.contains('shrink-0')).toBe(true);
});

const outsiders: HTMLElement[] = [];

beforeEach(() => {
  lastAction.value = {};
  localStorage.clear();
});

afterEach(() => {
  vi.restoreAllMocks();
  for (const element of outsiders.splice(0)) element.remove();
  lastAction.value = {};
});

function outside(): HTMLButtonElement {
  const button = document.createElement('button');
  document.body.append(button);
  outsiders.push(button);
  return button;
}

function mainButton(host: OpmSplitButton): HTMLButtonElement {
  const button = host.querySelector<HTMLButtonElement>('.join > button');
  if (!button) throw new Error('no main button rendered');
  return button;
}

function chevron(host: OpmSplitButton): HTMLElement {
  const summary = host.querySelector<HTMLElement>('details > summary');
  if (!summary) throw new Error('no chevron rendered');
  return summary;
}

function details(host: OpmSplitButton): HTMLDetailsElement {
  const element = host.querySelector('details');
  if (!element) throw new Error('no dropdown rendered');
  return element;
}

function items(host: OpmSplitButton): HTMLButtonElement[] {
  return Array.from(host.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]'));
}

function item(host: OpmSplitButton, label: string): HTMLButtonElement {
  const found = items(host).find((entry) => labelOf(entry) === label);
  if (!found) throw new Error(`no menu entry "${label}"`);
  return found;
}

/** The label of a button, without its icon, spinner or badge. */
function labelOf(button: HTMLElement): string | undefined {
  return button.querySelector('span.truncate, span:not([class])')?.textContent ?? undefined;
}

async function openMenu(host: OpmSplitButton): Promise<void> {
  chevron(host).click();
  await host.updateComplete;
}

async function choose(host: OpmSplitButton, label: string): Promise<void> {
  item(host, label).click();
  await host.updateComplete;
}

function press(target: Element, key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

function pointerDown(target: Element): void {
  target.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true }));
}

function recordEvents(host: OpmSplitButton) {
  const actions: string[] = [];
  const selections: string[] = [];
  host.addEventListener('split-action', (event) => actions.push(event.detail.optionId));
  host.addEventListener('split-select', (event) => selections.push(event.detail.optionId));
  return { actions, selections };
}

function stored(): unknown {
  const raw = localStorage.getItem(`${STORAGE_PREFIX}lastAction`);
  return raw === null ? null : JSON.parse(raw);
}

describe('opm-split-button structure', () => {
  it('renders nothing without options', async () => {
    const host = await mount('opm-split-button');

    expect(host.querySelector('button')).toBeNull();
    expect(host.querySelector('.join')).toBeNull();
  });

  it('is a DaisyUI join of a main button and a dropdown that opens upwards', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });

    expect(host.classList.contains('inline-flex')).toBe(true);
    const main = host.querySelector('.join > button.btn.btn-sm.join-item');
    expect(main).not.toBeNull();
    const dropdown = host.querySelector('.join > details.dropdown.dropdown-top.dropdown-end');
    expect(dropdown).not.toBeNull();
    expect(
      dropdown?.querySelector(':scope > summary.btn.btn-sm.btn-square.join-item'),
    ).not.toBeNull();
    expect(dropdown?.querySelector(':scope > ul.menu.dropdown-content')).not.toBeNull();
  });

  it('shows the default option on the main button', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, defaultId: 'move' });

    expect(labelOf(mainButton(host))).toBe('Move files');
    expect(mainButton(host).querySelector('opm-icon')?.getAttribute('name')).toBe('folder-input');
  });

  it('shows the first option when there is no usable default', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    expect(labelOf(mainButton(host))).toBe('Delete source items');

    host.defaultId = 'gone';
    await host.updateComplete;
    expect(labelOf(mainButton(host))).toBe('Delete source items');
  });

  it('reads the default from the default-id attribute', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    host.setAttribute('default-id', 'copy');
    await host.updateComplete;

    expect(labelOf(mainButton(host))).toBe('Copy files');
  });

  it('lists every option in the menu and checks the selected one', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, defaultId: 'move' });

    expect(items(host).map(labelOf)).toEqual(['Delete source items', 'Move files', 'Copy files']);
    expect(items(host).map((entry) => entry.getAttribute('aria-checked'))).toEqual([
      'false',
      'true',
      'false',
    ]);
    const selected = item(host, 'Move files');
    expect(selected.classList.contains('menu-active')).toBe(true);
    expect(selected.querySelector('opm-icon[name="check"]')).not.toBeNull();
    expect(item(host, 'Copy files').querySelector('opm-icon[name="check"]')).toBeNull();
    expect(item(host, 'Copy files').classList.contains('menu-active')).toBe(false);
  });

  it('draws an icon only for the options that have one', async () => {
    const host = await mount('opm-split-button', { options: [{ id: 'plain', label: 'Plain' }] });

    expect(mainButton(host).querySelector('opm-icon')).toBeNull();
    expect(items(host)[0]?.querySelector('opm-icon[name="check"]')).not.toBeNull();
    expect(items(host)[0]?.querySelectorAll('opm-icon')).toHaveLength(1);
  });

  it('gives the menu of every instance its own id', async () => {
    const first = await mount('opm-split-button', { options: OPTIONS });
    const second = await mount('opm-split-button', { options: OPTIONS });

    const menuId = (host: OpmSplitButton) => host.querySelector('ul')?.id;
    expect(menuId(first)).toBeTruthy();
    expect(menuId(first)).not.toBe(menuId(second));
    expect(chevron(first).getAttribute('aria-controls')).toBe(menuId(first));
  });

  it('describes the controls for assistive technology', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });

    const summary = chevron(host);
    expect(summary.getAttribute('aria-label')).toBe('Choose action');
    expect(summary.getAttribute('aria-haspopup')).toBe('menu');
    expect(summary.getAttribute('aria-expanded')).toBe('false');
    const menu = host.querySelector('ul');
    expect(menu?.getAttribute('role')).toBe('menu');
    expect(menu?.getAttribute('aria-label')).toBe('Choose action');
    expect(host.querySelectorAll('li[role="none"]')).toHaveLength(3);
    expect(summary.querySelector('opm-icon')?.getAttribute('name')).toBe('chevron-up');
  });
});

describe('opm-split-button colours, count and states', () => {
  it('turns both buttons red while a danger option is selected', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });

    for (const button of [mainButton(host), chevron(host)]) {
      expect(button.classList.contains('btn-error')).toBe(true);
      expect(button.classList.contains('btn-primary')).toBe(false);
    }

    await choose(host, 'Move files');
    for (const button of [mainButton(host), chevron(host)]) {
      expect(button.classList.contains('btn-primary')).toBe(true);
      expect(button.classList.contains('btn-error')).toBe(false);
    }
  });

  it('marks a danger option that is not selected in red in the menu', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, defaultId: 'move' });

    expect(item(host, 'Delete source items').classList.contains('text-error')).toBe(true);
    expect(item(host, 'Copy files').classList.contains('text-error')).toBe(false);
  });

  it('shows the count as a badge with grouped digits', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, count: 1234567 });

    const badge = mainButton(host).querySelector('.badge.badge-neutral');
    expect(badge?.textContent).toBe(formatCount(1234567));
    expect(badge?.textContent).not.toContain('1234567');
  });

  it('shows a count of zero and hides the badge when there is no count', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, count: 0 });
    expect(mainButton(host).querySelector('.badge')?.textContent).toBe('0');

    host.count = undefined;
    await host.updateComplete;
    expect(mainButton(host).querySelector('.badge')).toBeNull();
  });

  it('reads the count from the count attribute', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    host.setAttribute('count', '42');
    await host.updateComplete;

    expect(host.count).toBe(42);
    expect(mainButton(host).querySelector('.badge')?.textContent).toBe(formatCount(42));
  });

  it('shows a spinner and locks both buttons while busy, keeping the label', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, busy: true });
    const events = recordEvents(host);

    const main = mainButton(host);
    expect(main.querySelector('.loading.loading-spinner')).not.toBeNull();
    expect(main.querySelector('opm-icon')).toBeNull();
    expect(labelOf(main)).toBe('Delete source items');
    expect(main.getAttribute('aria-busy')).toBe('true');
    expect(main.disabled).toBe(true);
    const summary = chevron(host);
    expect(summary.classList.contains('btn-disabled')).toBe(true);
    expect(summary.getAttribute('aria-disabled')).toBe('true');
    expect(summary.getAttribute('tabindex')).toBe('-1');

    main.click();
    summary.click();
    await host.updateComplete;
    expect(events.actions).toEqual([]);
    expect(details(host).open).toBe(false);
  });

  it('locks both buttons when disabled, without a spinner', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, disabled: true });
    const events = recordEvents(host);

    expect(mainButton(host).disabled).toBe(true);
    expect(mainButton(host).querySelector('.loading')).toBeNull();
    expect(mainButton(host).getAttribute('aria-busy')).toBeNull();
    expect(mainButton(host).querySelector('opm-icon')).not.toBeNull();
    expect(chevron(host).classList.contains('btn-disabled')).toBe(true);

    mainButton(host).click();
    chevron(host).click();
    await host.updateComplete;
    expect(events.actions).toEqual([]);
    expect(details(host).open).toBe(false);
  });

  it('unlocks again when the flags are cleared', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, busy: true, disabled: true });

    host.busy = false;
    host.disabled = false;
    await host.updateComplete;

    expect(mainButton(host).disabled).toBe(false);
    expect(chevron(host).classList.contains('btn-disabled')).toBe(false);
    expect(chevron(host).getAttribute('aria-disabled')).toBeNull();
    expect(chevron(host).getAttribute('tabindex')).toBeNull();
  });

  it('shows the first option greyed out and locked when no option is enabled', async () => {
    const options = OPTIONS.map((option) => ({ ...option, disabled: true }));
    const host = await mount('opm-split-button', { options });
    const events = recordEvents(host);

    expect(labelOf(mainButton(host))).toBe('Delete source items');
    expect(mainButton(host).disabled).toBe(true);
    expect(chevron(host).classList.contains('btn-disabled')).toBe(true);

    mainButton(host).click();
    chevron(host).click();
    await host.updateComplete;
    expect(events.actions).toEqual([]);
    expect(details(host).open).toBe(false);
  });
});

describe('opm-split-button events', () => {
  it('emits split-action with the selected option from the main button only', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const onBody = vi.fn();
    document.body.addEventListener('split-action', onBody);
    const events = recordEvents(host);

    mainButton(host).click();

    expect(events.actions).toEqual(['delete-source']);
    expect(events.selections).toEqual([]);
    expect(onBody).toHaveBeenCalledOnce();
    document.body.removeEventListener('split-action', onBody);
  });

  it('carries the optionId in the event detail', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, defaultId: 'copy' });
    const received: CustomEvent[] = [];
    host.addEventListener('split-action', (event) => received.push(event));

    mainButton(host).click();

    expect(received).toHaveLength(1);
    expect(received[0]?.detail).toEqual({ optionId: 'copy' });
    expect(received[0]?.bubbles).toBe(true);
  });

  it('runs the remembered option rather than the default', async () => {
    lastAction.value = { duplicates: 'copy' };
    const host = await mount('opm-split-button', {
      options: OPTIONS,
      defaultId: 'move',
      persistKey: 'duplicates',
    });
    const events = recordEvents(host);

    mainButton(host).click();

    expect(events.actions).toEqual(['copy']);
  });

  it('does not remember anything when the main button is pressed', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });

    mainButton(host).click();

    expect(lastAction.value).toEqual({});
    expect(stored()).toBeNull();
  });

  it('selects an entry with split-select and never triggers split-action', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const events = recordEvents(host);
    await openMenu(host);

    await choose(host, 'Move files');

    expect(events.selections).toEqual(['move']);
    expect(events.actions).toEqual([]);
    expect(labelOf(mainButton(host))).toBe('Move files');
    expect(mainButton(host).querySelector('opm-icon')?.getAttribute('name')).toBe('folder-input');
  });

  it('emits split-select again when the selected entry is chosen again', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const events = recordEvents(host);

    await openMenu(host);
    await choose(host, 'Delete source items');

    expect(events.selections).toEqual(['delete-source']);
    expect(events.actions).toEqual([]);
  });

  it('then runs the newly selected option from the main button', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const events = recordEvents(host);

    await choose(host, 'Copy files');
    mainButton(host).click();

    expect(events.actions).toEqual(['copy']);
  });

  it('ignores a disabled entry', async () => {
    const options = OPTIONS.map((option) => ({ ...option, disabled: option.id === 'move' }));
    const host = await mount('opm-split-button', { options });
    const events = recordEvents(host);
    await openMenu(host);

    const entry = item(host, 'Move files');
    expect(entry.disabled).toBe(true);
    entry.click();
    await host.updateComplete;

    expect(events.selections).toEqual([]);
    expect(labelOf(mainButton(host))).toBe('Delete source items');
    expect(details(host).open).toBe(true);
  });

  it('skips a disabled default and shows the first enabled option', async () => {
    const options = OPTIONS.map((option) => ({ ...option, disabled: option.id !== 'copy' }));
    const host = await mount('opm-split-button', { options, defaultId: 'delete-source' });

    expect(labelOf(mainButton(host))).toBe('Copy files');
    expect(mainButton(host).disabled).toBe(false);
  });
});

describe('opm-split-button remembered choice', () => {
  it('uses the remembered choice of its persist-key over the default', async () => {
    lastAction.value = { duplicates: 'copy' };
    const host = await mount('opm-split-button', {
      options: OPTIONS,
      defaultId: 'move',
      persistKey: 'duplicates',
    });

    expect(labelOf(mainButton(host))).toBe('Copy files');
  });

  it('ignores the remembered choice of another key', async () => {
    lastAction.value = { move: 'copy' };
    const host = await mount('opm-split-button', {
      options: OPTIONS,
      defaultId: 'move',
      persistKey: 'duplicates',
    });

    expect(labelOf(mainButton(host))).toBe('Move files');
  });

  it('reads the persist-key from its attribute', async () => {
    lastAction.value = { duplicates: 'copy' };
    const host = await mount('opm-split-button', { options: OPTIONS });
    host.setAttribute('persist-key', 'duplicates');
    await host.updateComplete;

    expect(labelOf(mainButton(host))).toBe('Copy files');
  });

  it('remembers a chosen entry in the lastAction preference, next to the other keys', async () => {
    lastAction.value = { other: 'x' };
    const host = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });

    await choose(host, 'Move files');

    expect(lastAction.value).toEqual({ other: 'x', duplicates: 'move' });
    expect(stored()).toEqual({ other: 'x', duplicates: 'move' });
  });

  it('survives a reload: a new element starts from the remembered choice', async () => {
    const first = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });
    await choose(first, 'Copy files');
    first.remove();

    const second = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });

    expect(labelOf(mainButton(second))).toBe('Copy files');
  });

  it('keeps two elements with the same persist-key in step', async () => {
    const first = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });
    const second = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });

    await choose(first, 'Move files');
    await second.updateComplete;

    expect(labelOf(mainButton(second))).toBe('Move files');
  });

  it('follows a change of the preference made elsewhere', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });

    lastAction.value = { duplicates: 'move' };
    await host.updateComplete;

    expect(labelOf(mainButton(host))).toBe('Move files');
  });

  it('follows a change made in another tab', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });
    const key = `${STORAGE_PREFIX}lastAction`;
    const newValue = JSON.stringify({ duplicates: 'copy' });

    localStorage.setItem(key, newValue);
    window.dispatchEvent(
      new StorageEvent('storage', { key, newValue, storageArea: window.localStorage }),
    );
    await host.updateComplete;

    expect(labelOf(mainButton(host))).toBe('Copy files');
  });

  it('falls back to the default for an unknown remembered id, without forgetting it', async () => {
    lastAction.value = { duplicates: 'gone' };
    const host = await mount('opm-split-button', {
      options: OPTIONS,
      defaultId: 'move',
      persistKey: 'duplicates',
    });

    expect(labelOf(mainButton(host))).toBe('Move files');
    expect(lastAction.value).toEqual({ duplicates: 'gone' });
  });

  it('skips a remembered option that is disabled and takes it back once it is enabled', async () => {
    lastAction.value = { duplicates: 'copy' };
    const disabledCopy = OPTIONS.map((option) => ({ ...option, disabled: option.id === 'copy' }));
    const host = await mount('opm-split-button', {
      options: disabledCopy,
      defaultId: 'move',
      persistKey: 'duplicates',
    });

    expect(labelOf(mainButton(host))).toBe('Move files');
    expect(lastAction.value).toEqual({ duplicates: 'copy' });

    host.options = OPTIONS;
    await host.updateComplete;
    expect(labelOf(mainButton(host))).toBe('Copy files');
  });

  it('keeps the choice in the element without a persist-key and stores nothing', async () => {
    const first = await mount('opm-split-button', { options: OPTIONS });
    const second = await mount('opm-split-button', { options: OPTIONS });

    await choose(first, 'Copy files');

    expect(labelOf(mainButton(first))).toBe('Copy files');
    expect(labelOf(mainButton(second))).toBe('Delete source items');
    expect(lastAction.value).toEqual({});
    expect(stored()).toBeNull();
  });

  it('does not write the preference again for a choice that is already remembered', async () => {
    lastAction.value = { duplicates: 'move' };
    const before = lastAction.value;
    const host = await mount('opm-split-button', { options: OPTIONS, persistKey: 'duplicates' });

    await choose(host, 'Move files');

    expect(lastAction.value).toBe(before);
  });
});

describe('opm-split-button menu', () => {
  it('opens and closes from the chevron', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    expect(details(host).open).toBe(false);

    await openMenu(host);
    expect(details(host).open).toBe(true);
    expect(chevron(host).getAttribute('aria-expanded')).toBe('true');

    await openMenu(host);
    expect(details(host).open).toBe(false);
    expect(chevron(host).getAttribute('aria-expanded')).toBe('false');
  });

  it('keeps the browser from toggling the details by itself', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const click = new MouseEvent('click', { bubbles: true, cancelable: true });

    chevron(host).dispatchEvent(click);

    expect(click.defaultPrevented).toBe(true);
  });

  it('moves the focus to the selected entry when it opens', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, defaultId: 'move' });

    await openMenu(host);

    expect(document.activeElement).toBe(item(host, 'Move files'));
  });

  it('closes after a choice and puts the focus back on the chevron', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);

    await choose(host, 'Move files');

    expect(details(host).open).toBe(false);
    expect(document.activeElement).toBe(chevron(host));
  });

  it('closes on Escape, puts the focus back on the chevron and keeps the key to itself', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const reachedDocument = vi.fn();
    document.addEventListener('keydown', reachedDocument);
    await openMenu(host);

    const event = press(item(host, 'Delete source items'), 'Escape');
    await host.updateComplete;

    expect(details(host).open).toBe(false);
    expect(document.activeElement).toBe(chevron(host));
    expect(event.defaultPrevented).toBe(true);
    expect(reachedDocument).not.toHaveBeenCalled();
    document.removeEventListener('keydown', reachedDocument);
  });

  it('leaves Escape alone while the menu is closed', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const reachedDocument = vi.fn();
    document.addEventListener('keydown', reachedDocument);

    const event = press(chevron(host), 'Escape');

    expect(event.defaultPrevented).toBe(false);
    expect(reachedDocument).toHaveBeenCalledOnce();
    document.removeEventListener('keydown', reachedDocument);
  });

  it('closes on a press outside without taking the focus back', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const elsewhere = outside();
    await openMenu(host);

    pointerDown(elsewhere);
    await host.updateComplete;

    expect(details(host).open).toBe(false);
    expect(document.activeElement).not.toBe(chevron(host));
  });

  it('closes on a press in the page body', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);

    pointerDown(document.body);
    await host.updateComplete;

    expect(details(host).open).toBe(false);
  });

  it('treats the main button as outside', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);

    pointerDown(mainButton(host));
    await host.updateComplete;

    expect(details(host).open).toBe(false);
  });

  it('stays open for a press inside the dropdown', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);

    pointerDown(item(host, 'Copy files'));
    pointerDown(chevron(host));
    pointerDown(host.querySelector('ul') as HTMLElement);
    await host.updateComplete;

    expect(details(host).open).toBe(true);
  });

  it('closes when the focus moves to something outside', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    const elsewhere = outside();
    await openMenu(host);

    elsewhere.focus();
    await host.updateComplete;

    expect(details(host).open).toBe(false);
    expect(document.activeElement).toBe(elsewhere);
  });

  it('stays open while the focus moves inside the dropdown or is unknown', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);
    const focusOut = (relatedTarget: EventTarget | null) =>
      item(host, 'Delete source items').dispatchEvent(
        new FocusEvent('focusout', { bubbles: true, relatedTarget }),
      );

    focusOut(item(host, 'Move files'));
    focusOut(chevron(host));
    focusOut(null);
    await host.updateComplete;

    expect(details(host).open).toBe(true);
  });

  it('closes when it becomes busy, disabled or empty while open', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });

    await openMenu(host);
    host.busy = true;
    await host.updateComplete;
    expect(details(host).open).toBe(false);
    expect(chevron(host).getAttribute('aria-expanded')).toBe('false');

    host.busy = false;
    await openMenu(host);
    expect(details(host).open).toBe(true);
    host.disabled = true;
    await host.updateComplete;
    expect(details(host).open).toBe(false);

    host.disabled = false;
    await openMenu(host);
    expect(details(host).open).toBe(true);
    host.options = OPTIONS.map((option) => ({ ...option, disabled: true }));
    await host.updateComplete;
    expect(details(host).open).toBe(false);
  });

  it('does not steal the focus when it closes because it became busy', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);

    host.busy = true;
    await host.updateComplete;

    expect(document.activeElement).not.toBe(chevron(host));
  });

  it('only listens to presses outside while open', async () => {
    const add = vi.spyOn(document, 'addEventListener');
    const remove = vi.spyOn(document, 'removeEventListener');
    const host = await mount('opm-split-button', { options: OPTIONS });
    const pointerCalls = (spy: typeof add) =>
      spy.mock.calls.filter(([type]) => type === 'pointerdown');
    expect(pointerCalls(add)).toHaveLength(0);

    await openMenu(host);
    expect(pointerCalls(add)).toHaveLength(1);
    expect(pointerCalls(add)[0]?.[2]).toBe(true);

    await openMenu(host);
    expect(pointerCalls(remove)).toHaveLength(1);
    expect(pointerCalls(remove)[0]?.[1]).toBe(pointerCalls(add)[0]?.[1]);
    expect(pointerCalls(remove)[0]?.[2]).toBe(true);
  });

  it('removes its listener from the document when it is removed while open', async () => {
    const add = vi.spyOn(document, 'addEventListener');
    const remove = vi.spyOn(document, 'removeEventListener');
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);
    const handler = add.mock.calls.find(([type]) => type === 'pointerdown')?.[1];
    expect(handler).toBeTypeOf('function');

    host.remove();

    expect(remove).toHaveBeenCalledWith('pointerdown', handler, true);
  });

  it('forgets an open menu when it is attached again', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);

    host.remove();
    document.body.append(host);
    await host.updateComplete;

    expect(details(host).open).toBe(false);
  });
});

describe('opm-split-button keyboard', () => {
  it('opens on ArrowDown or ArrowUp from the chevron, on the selected entry', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, defaultId: 'move' });

    const down = press(chevron(host), 'ArrowDown');
    await host.updateComplete;
    expect(down.defaultPrevented).toBe(true);
    expect(details(host).open).toBe(true);
    expect(document.activeElement).toBe(item(host, 'Move files'));

    await openMenu(host);
    expect(details(host).open).toBe(false);

    press(chevron(host), 'ArrowUp');
    await host.updateComplete;
    expect(details(host).open).toBe(true);
    expect(document.activeElement).toBe(item(host, 'Move files'));
  });

  it('moves between the entries with the arrows and wraps around', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });
    await openMenu(host);
    const focused = () => labelOf(document.activeElement as HTMLElement);
    expect(focused()).toBe('Delete source items');

    press(document.activeElement as Element, 'ArrowDown');
    expect(focused()).toBe('Move files');
    press(document.activeElement as Element, 'ArrowDown');
    expect(focused()).toBe('Copy files');
    press(document.activeElement as Element, 'ArrowDown');
    expect(focused()).toBe('Delete source items');
    press(document.activeElement as Element, 'ArrowUp');
    expect(focused()).toBe('Copy files');
    press(document.activeElement as Element, 'ArrowUp');
    expect(focused()).toBe('Move files');
  });

  it('jumps to the first and the last entry with Home and End', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, defaultId: 'move' });
    await openMenu(host);

    press(document.activeElement as Element, 'End');
    expect(document.activeElement).toBe(item(host, 'Copy files'));
    press(document.activeElement as Element, 'Home');
    expect(document.activeElement).toBe(item(host, 'Delete source items'));
  });

  it('skips disabled entries', async () => {
    const options = OPTIONS.map((option) => ({ ...option, disabled: option.id === 'move' }));
    const host = await mount('opm-split-button', { options });
    await openMenu(host);

    press(document.activeElement as Element, 'ArrowDown');
    expect(document.activeElement).toBe(item(host, 'Copy files'));
    press(document.activeElement as Element, 'ArrowUp');
    expect(document.activeElement).toBe(item(host, 'Delete source items'));
    press(document.activeElement as Element, 'End');
    expect(document.activeElement).toBe(item(host, 'Copy files'));
  });

  it('enters the list from the chevron when the focus went back to it', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS, defaultId: 'move' });
    await openMenu(host);

    chevron(host).focus();
    press(chevron(host), 'ArrowDown');
    expect(document.activeElement).toBe(item(host, 'Delete source items'));

    chevron(host).focus();
    press(chevron(host), 'ArrowUp');
    expect(document.activeElement).toBe(item(host, 'Copy files'));
  });

  it('leaves Home and End alone while the menu is closed', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });

    expect(press(chevron(host), 'Home').defaultPrevented).toBe(false);
    expect(press(chevron(host), 'End').defaultPrevented).toBe(false);
  });

  it('ignores the arrows with a modifier and while locked', async () => {
    const host = await mount('opm-split-button', { options: OPTIONS });

    for (const modifier of ['ctrlKey', 'altKey', 'metaKey'] as const) {
      press(chevron(host), 'ArrowDown', { [modifier]: true });
    }
    await host.updateComplete;
    expect(details(host).open).toBe(false);

    host.busy = true;
    await host.updateComplete;
    const event = press(chevron(host), 'ArrowDown');
    await host.updateComplete;
    expect(event.defaultPrevented).toBe(false);
    expect(details(host).open).toBe(false);
  });
});

describe('opm-split-button hints', () => {
  const hinted: SplitOption[] = [
    { id: 'move', label: 'Move files', hint: 'Moves the matches to the target folder' },
    { id: 'copy', label: 'Copy files' },
    { id: 'link', label: 'Link files', hint: 'Not available here', disabled: true },
  ];

  it('puts a hint in a DaisyUI tooltip on the entry', async () => {
    const host = await mount('opm-split-button', { options: hinted });

    const tip = items(host)[0]?.closest('li');
    expect(tip?.classList.contains('tooltip')).toBe(true);
    expect(tip?.classList.contains('tooltip-left')).toBe(true);
    expect(tip?.getAttribute('data-tip')).toBe('Moves the matches to the target folder');
  });

  it('adds no tooltip to an entry without a hint', async () => {
    const host = await mount('opm-split-button', { options: hinted });

    const plain = item(host, 'Copy files').closest('li');
    expect(plain?.classList.contains('tooltip')).toBe(false);
    expect(plain?.hasAttribute('data-tip')).toBe(false);
    expect(item(host, 'Copy files').hasAttribute('aria-describedby')).toBe(false);
  });

  it('also reads the hint to assistive technology', async () => {
    const host = await mount('opm-split-button', { options: hinted });

    const entry = item(host, 'Move files');
    const describedBy = entry.getAttribute('aria-describedby');
    expect(describedBy).toBeTruthy();
    const description = host.querySelector(`[id="${describedBy}"]`);
    expect(description?.textContent).toBe('Moves the matches to the target folder');
    expect(description?.classList.contains('sr-only')).toBe(true);
  });

  it('keeps the tooltip on a disabled entry, which explains why it is unavailable', async () => {
    const host = await mount('opm-split-button', { options: hinted });

    const entry = item(host, 'Link files');
    expect(entry.disabled).toBe(true);
    expect(entry.closest('li')?.getAttribute('data-tip')).toBe('Not available here');
  });

  it('treats an empty hint as no hint', async () => {
    const host = await mount('opm-split-button', { options: [{ id: 'a', label: 'A', hint: '' }] });

    expect(host.querySelector('li')?.classList.contains('tooltip')).toBe(false);
    expect(host.querySelector('li')?.hasAttribute('data-tip')).toBe(false);
  });
});
