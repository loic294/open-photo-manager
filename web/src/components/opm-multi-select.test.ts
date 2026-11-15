import { afterEach, describe, expect, it, vi } from 'vitest';
import '@/components/opm-multi-select';
import type {
  MultiSelectChangeEvent,
  MultiSelectOption,
  OpmMultiSelect,
} from '@/components/opm-multi-select';
import { mount } from '@/test/helpers';
import { formatCount } from '@/util/format-count';

const CAMERAS: MultiSelectOption[] = [
  { value: 'Sony A7 IV', count: 1200 },
  { value: 'Canon EOS R5', count: 340 },
  { value: 'Nikon Z6', count: 12 },
  { value: 'Fujifilm X-T4', count: 3 },
];

const outsiders: HTMLElement[] = [];

afterEach(() => {
  vi.restoreAllMocks();
  for (const element of outsiders.splice(0)) element.remove();
});

function outside(): HTMLButtonElement {
  const button = document.createElement('button');
  document.body.append(button);
  outsiders.push(button);
  return button;
}

function details(host: OpmMultiSelect): HTMLDetailsElement {
  const element = host.querySelector('details');
  if (!element) throw new Error('no dropdown rendered');
  return element;
}

function trigger(host: OpmMultiSelect): HTMLElement {
  const summary = host.querySelector<HTMLElement>('details > summary');
  if (!summary) throw new Error('no trigger rendered');
  return summary;
}

function panel(host: OpmMultiSelect): HTMLElement {
  const element = host.querySelector<HTMLElement>('details > .dropdown-content');
  if (!element) throw new Error('no panel rendered');
  return element;
}

function search(host: OpmMultiSelect): HTMLInputElement {
  const input = host.querySelector<HTMLInputElement>('input[type="search"]');
  if (!input) throw new Error('the panel is closed');
  return input;
}

function boxes(host: OpmMultiSelect): HTMLInputElement[] {
  return Array.from(host.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'));
}

function rowLabels(host: OpmMultiSelect): string[] {
  return boxes(host).map((box) => box.closest('label')?.querySelector('span')?.textContent ?? '');
}

function box(host: OpmMultiSelect, value: string): HTMLInputElement {
  const index = rowLabels(host).indexOf(value);
  const found = boxes(host)[index];
  if (!found) throw new Error(`no row "${value}"`);
  return found;
}

function addRow(host: OpmMultiSelect): HTMLButtonElement | null {
  return host.querySelector<HTMLButtonElement>('button[data-add]');
}

function clearButton(host: OpmMultiSelect): HTMLButtonElement {
  const found = Array.from(host.querySelectorAll<HTMLButtonElement>('button')).find(
    (button) => button.textContent?.trim() === 'Clear',
  );
  if (!found) throw new Error('no Clear button');
  return found;
}

function chips(host: OpmMultiSelect): string[] {
  return Array.from(host.querySelectorAll('ul[role="list"] > li > span')).map(
    (span) => span.textContent ?? '',
  );
}

function chipRemove(host: OpmMultiSelect, value: string): HTMLButtonElement {
  const found = host.querySelector<HTMLButtonElement>(
    `ul[role="list"] button[aria-label="Remove ${value}"]`,
  );
  if (!found) throw new Error(`no chip "${value}"`);
  return found;
}

function record(host: OpmMultiSelect): string[][] {
  const seen: string[][] = [];
  host.addEventListener('multi-select-change', (event: MultiSelectChangeEvent) => {
    seen.push(event.detail);
  });
  return seen;
}

async function open(host: OpmMultiSelect): Promise<void> {
  trigger(host).click();
  await host.updateComplete;
}

async function type(host: OpmMultiSelect, text: string): Promise<void> {
  const input = search(host);
  input.value = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await host.updateComplete;
}

async function check(host: OpmMultiSelect, value: string): Promise<void> {
  box(host, value).click();
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

async function feed(host: OpmMultiSelect, selected: string[]): Promise<void> {
  host.selected = selected;
  await host.updateComplete;
}

async function mountOpen(props: Partial<OpmMultiSelect> = {}): Promise<OpmMultiSelect> {
  const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS, ...props });
  await open(host);
  return host;
}

describe('opm-multi-select structure', () => {
  it('is an inline wrapping host with a DaisyUI dropdown button', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    for (const name of ['inline-flex', 'flex-wrap', 'items-center', 'gap-1', 'max-w-full']) {
      expect(host.classList.contains(name)).toBe(true);
    }
    expect(host.querySelector('details.dropdown > summary.btn.btn-sm')).not.toBeNull();
    expect(host.querySelector('details.dropdown > .dropdown-content')).not.toBeNull();
  });

  it('shows the label and a chevron on the button', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    expect(trigger(host).textContent).toContain('Cameras');
    expect(trigger(host).querySelector('opm-icon[name="chevron-down"]')).not.toBeNull();
  });

  it('draws the icon when one is given', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', icon: 'camera' });

    expect(trigger(host).querySelector('opm-icon[name="camera"]')).not.toBeNull();
  });

  it('is closed at first and draws nothing in the panel', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    expect(details(host).open).toBe(false);
    expect(trigger(host).getAttribute('aria-expanded')).toBe('false');
    expect(host.querySelector('input')).toBeNull();
  });

  it('points the button at the panel', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    const id = trigger(host).getAttribute('aria-controls');
    expect(id).toBeTruthy();
    expect(panel(host).id).toBe(id);
  });

  it('gives every instance its own panel id', async () => {
    const first = await mount('opm-multi-select', { label: 'A' });
    const second = await mount('opm-multi-select', { label: 'B' });

    expect(panel(first).id).not.toBe(panel(second).id);
  });

  it('names the panel after the label', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras' });

    expect(panel(host).getAttribute('aria-label')).toBe('Cameras');
  });

  it('counts the chosen values on the button and in its name', async () => {
    const host = await mount('opm-multi-select', {
      label: 'Cameras',
      options: CAMERAS,
      selected: ['Nikon Z6', 'Sony A7 IV'],
    });

    expect(trigger(host).querySelector('.badge')?.textContent).toBe('2');
    expect(trigger(host).getAttribute('aria-label')).toBe('Cameras (2 selected)');
  });

  it('shows no count and keeps the plain name when nothing is chosen', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    expect(trigger(host).querySelector('.badge')).toBeNull();
    expect(trigger(host).hasAttribute('aria-label')).toBe(false);
  });

  it('reads allow-custom, max-visible and the label from attributes', async () => {
    const host = await mount('opm-multi-select');

    host.setAttribute('allow-custom', '');
    host.setAttribute('max-visible', '7');
    host.setAttribute('label', 'Tags');
    await host.updateComplete;

    expect(host.allowCustom).toBe(true);
    expect(host.maxVisible).toBe(7);
    expect(trigger(host).textContent).toContain('Tags');
  });
});

describe('opm-multi-select chips', () => {
  it('shows every chosen value as a chip outside the panel', async () => {
    const host = await mount('opm-multi-select', {
      label: 'Cameras',
      options: CAMERAS,
      selected: ['Nikon Z6', 'Sony A7 IV'],
    });

    expect(chips(host)).toEqual(['Nikon Z6', 'Sony A7 IV']);
    expect(details(host).contains(host.querySelector('ul[role="list"]'))).toBe(false);
    expect(host.querySelector('ul[role="list"]')?.getAttribute('aria-label')).toBe(
      'Selected Cameras',
    );
  });

  it('shows no list without chosen values', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    expect(host.querySelector('ul[role="list"]')).toBeNull();
  });

  it('shows a chosen value that is not in the options', async () => {
    const host = await mount('opm-multi-select', {
      label: 'Cameras',
      options: CAMERAS,
      selected: ['Leica Q3'],
    });

    expect(chips(host)).toEqual(['Leica Q3']);
  });

  it('truncates a long value and keeps it whole in a tooltip', async () => {
    const long = 'A'.repeat(120);
    const host = await mount('opm-multi-select', { label: 'Tags', selected: [long] });

    const label = host.querySelector('ul[role="list"] li span');
    expect(label?.classList.contains('truncate')).toBe(true);
    expect(label?.getAttribute('data-tooltip')).toBe(long);
  });

  it('removes a value with the x of its chip', async () => {
    const host = await mount('opm-multi-select', {
      label: 'Cameras',
      options: CAMERAS,
      selected: ['Nikon Z6', 'Sony A7 IV'],
    });
    const seen = record(host);

    chipRemove(host, 'Nikon Z6').click();

    expect(seen).toEqual([['Sony A7 IV']]);
  });

  it('removes the value without regard to case', async () => {
    const host = await mount('opm-multi-select', { label: 'Tags', selected: ['Sunset', 'beach'] });
    const seen = record(host);

    chipRemove(host, 'Sunset').click();

    expect(seen).toEqual([['beach']]);
  });

  it('moves the focus to the button after a removal', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', selected: ['Nikon Z6'] });

    chipRemove(host, 'Nikon Z6').click();

    expect(document.activeElement).toBe(trigger(host));
  });

  it('is controlled: the chip stays until the host feeds the change back', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', selected: ['Nikon Z6'] });

    chipRemove(host, 'Nikon Z6').click();
    await host.updateComplete;
    expect(chips(host)).toEqual(['Nikon Z6']);

    await feed(host, []);
    expect(chips(host)).toEqual([]);
  });

  it('keeps the chips in the order they were chosen', async () => {
    const host = await mount('opm-multi-select', {
      label: 'Tags',
      selected: ['zebra', 'apple', 'mango'],
    });

    expect(chips(host)).toEqual(['zebra', 'apple', 'mango']);
  });
});

describe('opm-multi-select opening and closing', () => {
  it('opens when the button is pressed and focuses the search box', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    await open(host);

    expect(details(host).open).toBe(true);
    expect(trigger(host).getAttribute('aria-expanded')).toBe('true');
    expect(document.activeElement).toBe(search(host));
  });

  it('closes when the button is pressed again and focuses it', async () => {
    const host = await mountOpen();

    await open(host);

    expect(details(host).open).toBe(false);
    expect(trigger(host).getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(trigger(host));
  });

  it('does not let the browser toggle the details by itself', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
    const event = new MouseEvent('click', { bubbles: true, cancelable: true });

    trigger(host).dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
  });

  it('closes on Escape, stops the key there and focuses the button', async () => {
    const host = await mountOpen();
    const pageHandler = vi.fn();
    document.addEventListener('keydown', pageHandler);

    const event = press(search(host), 'Escape');
    await host.updateComplete;
    document.removeEventListener('keydown', pageHandler);

    expect(event.defaultPrevented).toBe(true);
    expect(pageHandler).not.toHaveBeenCalled();
    expect(details(host).open).toBe(false);
    expect(document.activeElement).toBe(trigger(host));
  });

  it('leaves Escape alone while closed', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    const event = press(trigger(host), 'Escape');

    expect(event.defaultPrevented).toBe(false);
  });

  it('closes on a press outside, without taking the focus', async () => {
    const host = await mountOpen();
    const other = outside();
    other.focus();

    pointerDown(other);
    await host.updateComplete;

    expect(details(host).open).toBe(false);
    expect(document.activeElement).toBe(other);
  });

  it('stays open on a press inside the panel', async () => {
    const host = await mountOpen();

    pointerDown(box(host, 'Nikon Z6'));
    pointerDown(search(host));
    await host.updateComplete;

    expect(details(host).open).toBe(true);
  });

  it('stays open on a press on the button (its own click decides)', async () => {
    const host = await mountOpen();

    pointerDown(trigger(host));
    await host.updateComplete;

    expect(details(host).open).toBe(true);
  });

  it('closes when the focus moves to something outside', async () => {
    const host = await mountOpen();
    const other = outside();

    search(host).dispatchEvent(new FocusEvent('focusout', { bubbles: true, relatedTarget: other }));
    await host.updateComplete;

    expect(details(host).open).toBe(false);
  });

  it('stays open when the focus moves inside, or nowhere in particular', async () => {
    const host = await mountOpen();

    search(host).dispatchEvent(
      new FocusEvent('focusout', { bubbles: true, relatedTarget: box(host, 'Nikon Z6') }),
    );
    search(host).dispatchEvent(new FocusEvent('focusout', { bubbles: true, relatedTarget: null }));
    await host.updateComplete;

    expect(details(host).open).toBe(true);
  });

  it('stops listening for presses once closed', async () => {
    const host = await mountOpen();
    const remove = vi.spyOn(document, 'removeEventListener');

    await open(host);

    expect(remove.mock.calls.some(([type]) => type === 'pointerdown')).toBe(true);
  });

  it('forgets the search text when it closes', async () => {
    const host = await mountOpen();
    await type(host, 'nik');

    await open(host);
    await open(host);

    expect(search(host).value).toBe('');
    expect(rowLabels(host)).toHaveLength(4);
  });

  it('opens with ArrowDown on the button and not with ArrowUp', async () => {
    const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });

    const up = press(trigger(host), 'ArrowUp');
    await host.updateComplete;
    expect(up.defaultPrevented).toBe(false);
    expect(details(host).open).toBe(false);

    const down = press(trigger(host), 'ArrowDown');
    await host.updateComplete;
    expect(down.defaultPrevented).toBe(true);
    expect(details(host).open).toBe(true);
    expect(document.activeElement).toBe(search(host));
  });

  it('cleans up when it leaves the page while open', async () => {
    const host = await mountOpen();
    const remove = vi.spyOn(document, 'removeEventListener');

    host.remove();

    expect(remove.mock.calls.some(([type]) => type === 'pointerdown')).toBe(true);
  });

  describe('disabled', () => {
    it('does not open', async () => {
      const host = await mount('opm-multi-select', {
        label: 'Cameras',
        options: CAMERAS,
        disabled: true,
      });

      await open(host);
      press(trigger(host), 'ArrowDown');
      await host.updateComplete;

      expect(details(host).open).toBe(false);
    });

    it('looks and reads as disabled and leaves the tab order', async () => {
      const host = await mount('opm-multi-select', { label: 'Cameras', disabled: true });

      expect(trigger(host).classList.contains('btn-disabled')).toBe(true);
      expect(trigger(host).getAttribute('aria-disabled')).toBe('true');
      expect(trigger(host).getAttribute('tabindex')).toBe('-1');
    });

    it('closes an open panel', async () => {
      const host = await mountOpen();

      host.disabled = true;
      await host.updateComplete;

      expect(details(host).open).toBe(false);
    });

    it('disables the remove buttons of the chips', async () => {
      const host = await mount('opm-multi-select', {
        label: 'Cameras',
        selected: ['Nikon Z6'],
        disabled: true,
      });
      const seen = record(host);

      chipRemove(host, 'Nikon Z6').click();

      expect(chipRemove(host, 'Nikon Z6').disabled).toBe(true);
      expect(seen).toEqual([]);
    });

    it('can be enabled again', async () => {
      const host = await mount('opm-multi-select', { label: 'Cameras', disabled: true });

      host.disabled = false;
      await host.updateComplete;
      await open(host);

      expect(details(host).open).toBe(true);
      expect(trigger(host).classList.contains('btn-disabled')).toBe(false);
    });
  });

  describe('placement', () => {
    it('opens towards the right while it fits', async () => {
      const host = await mountOpen();

      expect(details(host).classList.contains('dropdown-end')).toBe(false);
    });

    it('opens towards the left when it would leave the window', async () => {
      const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
      vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
        this: HTMLElement,
      ) {
        const right = this.classList.contains('dropdown-content') ? window.innerWidth + 40 : 10;
        return { right } as DOMRect;
      });

      await open(host);
      await host.updateComplete;

      expect(details(host).classList.contains('dropdown-end')).toBe(true);
    });

    it('goes back to the default placement after closing', async () => {
      const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
      vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
        this: HTMLElement,
      ) {
        const right = this.classList.contains('dropdown-content') ? window.innerWidth + 40 : 10;
        return { right } as DOMRect;
      });
      await open(host);
      await host.updateComplete;

      await open(host);
      await host.updateComplete;

      expect(details(host).classList.contains('dropdown-end')).toBe(false);
    });

    it('flips without a second update, once the panel was measured', async () => {
      const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
      vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
        this: HTMLElement,
      ) {
        const right = this.classList.contains('dropdown-content') ? window.innerWidth + 40 : 10;
        return { right } as DOMRect;
      });

      trigger(host).click();

      // Lit warns when an update changes state, and resolves with false then.
      expect(await host.updateComplete).toBe(true);
      expect(details(host).classList.contains('dropdown-end')).toBe(true);
    });

    it('closes without scheduling another update, with a search typed too', async () => {
      const host = await mountOpen();
      await type(host, 'sony');

      press(search(host), 'Escape');

      expect(await host.updateComplete).toBe(true);
    });

    describe('while open', () => {
      // The panel sticks out of the window only while it is aligned to the left edge of the button.
      function stickOutWhile(room: { tooNarrow: boolean }): void {
        vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
          this: HTMLElement,
        ) {
          const flipped = this.closest('details')?.classList.contains('dropdown-end') === true;
          const sticksOut =
            this.classList.contains('dropdown-content') && room.tooNarrow && !flipped;
          return { right: sticksOut ? window.innerWidth + 40 : 10 } as DOMRect;
        });
      }

      function flipped(host: OpmMultiSelect): boolean {
        return details(host).classList.contains('dropdown-end');
      }

      it('measures the default placement again, whatever the last one was', async () => {
        const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
        stickOutWhile({ tooNarrow: true });
        await open(host);

        host.options = [...CAMERAS];
        await host.updateComplete;

        expect(flipped(host)).toBe(true);
      });

      it('goes back to the default placement when the button moves away from the edge', async () => {
        const room = { tooNarrow: true };
        const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
        stickOutWhile(room);
        await open(host);
        expect(flipped(host)).toBe(true);

        room.tooNarrow = false;
        await feed(host, ['Nikon Z6']);

        expect(flipped(host)).toBe(false);
      });

      it('flips when the button moves towards the edge', async () => {
        const room = { tooNarrow: false };
        const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
        stickOutWhile(room);
        await open(host);
        expect(flipped(host)).toBe(false);

        room.tooNarrow = true;
        await feed(host, ['Nikon Z6']);

        expect(flipped(host)).toBe(true);
      });

      it('places the panel again when the window is resized', async () => {
        const room = { tooNarrow: true };
        const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
        stickOutWhile(room);
        await open(host);
        expect(flipped(host)).toBe(true);

        room.tooNarrow = false;
        window.dispatchEvent(new Event('resize'));
        expect(flipped(host)).toBe(false);

        room.tooNarrow = true;
        window.dispatchEvent(new Event('resize'));
        expect(flipped(host)).toBe(true);
      });

      it('ignores the window once closed', async () => {
        const room = { tooNarrow: true };
        const host = await mount('opm-multi-select', { label: 'Cameras', options: CAMERAS });
        stickOutWhile(room);
        await open(host);
        const remove = vi.spyOn(window, 'removeEventListener');

        await open(host);
        window.dispatchEvent(new Event('resize'));

        expect(remove.mock.calls.some(([type]) => type === 'resize')).toBe(true);
        expect(details(host).open).toBe(false);
        expect(flipped(host)).toBe(false);
      });

      it('stops listening to the window when it leaves the page while open', async () => {
        const host = await mountOpen();
        const remove = vi.spyOn(window, 'removeEventListener');

        host.remove();

        expect(remove.mock.calls.some(([type]) => type === 'resize')).toBe(true);
      });
    });
  });
});

describe('opm-multi-select rows', () => {
  it('lists every option with a checkbox, in the given order', async () => {
    const host = await mountOpen();

    expect(rowLabels(host)).toEqual(['Sony A7 IV', 'Canon EOS R5', 'Nikon Z6', 'Fujifilm X-T4']);
    for (const each of boxes(host)) expect(each.classList.contains('checkbox-sm')).toBe(true);
  });

  it('lays the rows out as a small DaisyUI menu that scrolls', async () => {
    const host = await mountOpen();

    const list = host.querySelector('ul.menu.menu-sm');
    expect(list).not.toBeNull();
    expect(list?.classList.contains('overflow-y-auto')).toBe(true);
    expect(list?.classList.contains('flex-nowrap')).toBe(true);
  });

  it('shows the count of each option, formatted', async () => {
    const host = await mountOpen();

    const badges = Array.from(host.querySelectorAll('ul.menu .badge')).map((b) => b.textContent);
    expect(badges).toEqual([1200, 340, 12, 3].map((n) => formatCount(n)));
  });

  it('adds the count to the name of the checkbox', async () => {
    const host = await mountOpen();

    expect(box(host, 'Sony A7 IV').getAttribute('aria-label')).toBe(
      `Sony A7 IV (${formatCount(1200)})`,
    );
  });

  it('shows a zero count', async () => {
    const host = await mountOpen({ options: [{ value: 'Nikon Z6', count: 0 }] });

    expect(host.querySelector('ul.menu .badge')?.textContent).toBe('0');
  });

  it('shows no count for an option that has none', async () => {
    const host = await mountOpen({ options: [{ value: 'Nikon Z6' }] });

    expect(host.querySelector('ul.menu .badge')).toBeNull();
    expect(box(host, 'Nikon Z6').hasAttribute('aria-label')).toBe(false);
  });

  it('checks the chosen options', async () => {
    const host = await mountOpen({ selected: ['Nikon Z6', 'Sony A7 IV'] });

    expect(boxes(host).map((each) => each.checked)).toEqual([true, false, true, false]);
  });

  it('matches chosen values without regard to case', async () => {
    const host = await mountOpen({ selected: ['nikon z6'] });

    expect(box(host, 'Nikon Z6').checked).toBe(true);
  });

  it('lists a chosen value that is not in the options first, checked', async () => {
    const host = await mountOpen({ selected: ['Leica Q3'] });

    expect(rowLabels(host)).toEqual([
      'Leica Q3',
      'Sony A7 IV',
      'Canon EOS R5',
      'Nikon Z6',
      'Fujifilm X-T4',
    ]);
    expect(box(host, 'Leica Q3').checked).toBe(true);
  });

  it('lists such a value once even if it was chosen twice', async () => {
    const host = await mountOpen({ selected: ['Leica Q3', 'leica q3'] });

    expect(rowLabels(host).filter((label) => label.toLowerCase() === 'leica q3')).toHaveLength(1);
  });

  it('keeps a chosen option where it is: no reordering', async () => {
    const host = await mountOpen({ selected: ['Fujifilm X-T4'] });

    expect(rowLabels(host)[3]).toBe('Fujifilm X-T4');
  });

  it('puts a long value in a truncated row with a tooltip', async () => {
    const long = 'B'.repeat(150);
    const host = await mountOpen({ options: [{ value: long }] });

    const text = host.querySelector('ul.menu label span');
    expect(text?.classList.contains('truncate')).toBe(true);
    expect(text?.getAttribute('data-tooltip')).toBe(long);
  });

  it('follows the options when they change while open', async () => {
    const host = await mountOpen();

    host.options = [{ value: 'Only one' }];
    await host.updateComplete;

    expect(rowLabels(host)).toEqual(['Only one']);
  });

  describe('empty states', () => {
    it('says there are no values', async () => {
      const host = await mountOpen({ options: [] });

      expect(panel(host).textContent).toContain('No values');
    });

    it('says nothing matches the search', async () => {
      const host = await mountOpen();

      await type(host, 'zzz');

      expect(rowLabels(host)).toEqual([]);
      expect(panel(host).textContent).toContain('No match');
      expect(panel(host).textContent).not.toContain('No values');
    });

    it('says nothing when there are rows', async () => {
      const host = await mountOpen();

      expect(panel(host).textContent).not.toContain('No values');
      expect(panel(host).textContent).not.toContain('No match');
    });
  });

  describe('loading', () => {
    it('shows a spinner row and marks the panel busy', async () => {
      const host = await mountOpen({ loading: true });

      expect(panel(host).querySelector('[role="status"] .loading')).not.toBeNull();
      expect(panel(host).textContent).toContain('Loading');
      expect(panel(host).getAttribute('aria-busy')).toBe('true');
    });

    it('keeps the values already known', async () => {
      const host = await mountOpen({ loading: true });

      expect(rowLabels(host)).toHaveLength(4);
    });

    it('does not claim there are no values yet', async () => {
      const host = await mountOpen({ loading: true, options: [] });

      expect(panel(host).textContent).not.toContain('No values');
    });

    it('goes away when loading ends', async () => {
      const host = await mountOpen({ loading: true });

      host.loading = false;
      await host.updateComplete;

      expect(panel(host).querySelector('[role="status"]')).toBeNull();
      expect(panel(host).hasAttribute('aria-busy')).toBe(false);
    });
  });
});

describe('opm-multi-select search', () => {
  it('has a labelled search box with the placeholder', async () => {
    const host = await mountOpen({ placeholder: 'Search cameras' });

    expect(search(host).getAttribute('aria-label')).toBe('Search Cameras');
    expect(search(host).getAttribute('placeholder')).toBe('Search cameras');
    expect(search(host).getAttribute('maxlength')).toBe('200');
  });

  it('narrows the rows while typing, ignoring case', async () => {
    const host = await mountOpen();

    await type(host, 'NIK');

    expect(rowLabels(host)).toEqual(['Nikon Z6']);
  });

  it('matches anywhere in the value', async () => {
    const host = await mountOpen();

    await type(host, 'eos');

    expect(rowLabels(host)).toEqual(['Canon EOS R5']);
  });

  it('matches without regard to Unicode form', async () => {
    const host = await mountOpen({ options: [{ value: 'Caf\u00e9' }, { value: 'Tea' }] });

    await type(host, 'cafe\u0301');

    expect(rowLabels(host)).toEqual(['Caf\u00e9']);
  });

  it('shows all rows again for an empty or blank search', async () => {
    const host = await mountOpen();
    await type(host, 'nik');

    await type(host, '   ');

    expect(rowLabels(host)).toHaveLength(4);
  });

  it('never sends an event while typing', async () => {
    const host = await mountOpen();
    const seen = record(host);

    await type(host, 'nik');
    await type(host, 'nikon');

    expect(seen).toEqual([]);
  });

  it('also searches among the chosen values that are not in the options', async () => {
    const host = await mountOpen({ selected: ['Leica Q3'] });

    await type(host, 'leica');

    expect(rowLabels(host)).toEqual(['Leica Q3']);
  });

  it('keeps the checks while narrowed', async () => {
    const host = await mountOpen({ selected: ['Nikon Z6'] });

    await type(host, 'z6');

    expect(boxes(host).map((each) => each.checked)).toEqual([true]);
  });
});

describe('opm-multi-select cap', () => {
  const MANY: MultiSelectOption[] = Array.from({ length: 12 }, (_, index) => ({
    value: `Camera ${String(index).padStart(2, '0')}`,
  }));

  it('draws only maxVisible rows and says so', async () => {
    const host = await mountOpen({ options: MANY, maxVisible: 5 });

    expect(rowLabels(host)).toEqual(MANY.slice(0, 5).map((option) => option.value));
    expect(panel(host).textContent).toContain('Showing the first 5, type to search');
  });

  it('says nothing when everything is drawn', async () => {
    const host = await mountOpen({ options: MANY, maxVisible: 12 });

    expect(rowLabels(host)).toHaveLength(12);
    expect(panel(host).textContent).not.toContain('Showing the first');
  });

  it('reaches the rest by searching', async () => {
    const host = await mountOpen({ options: MANY, maxVisible: 5 });

    await type(host, '11');

    expect(rowLabels(host)).toEqual(['Camera 11']);
    expect(panel(host).textContent).not.toContain('Showing the first');
  });

  it('caps a search result too', async () => {
    const host = await mountOpen({ options: MANY, maxVisible: 3 });

    await type(host, 'camera');

    expect(rowLabels(host)).toHaveLength(3);
    expect(panel(host).textContent).toContain('Showing the first 3, type to search');
  });

  it('defaults to a hundred rows', async () => {
    const options = Array.from({ length: 130 }, (_, index) => ({ value: `Tag ${index}` }));
    const host = await mountOpen({ options });

    expect(rowLabels(host)).toHaveLength(100);
    expect(panel(host).textContent).toContain('Showing the first 100, type to search');
  });

  it.each([0, -3, Number.NaN])('falls back to the default for maxVisible %s', async (value) => {
    const host = await mountOpen({ options: MANY, maxVisible: value });

    expect(rowLabels(host)).toHaveLength(12);
  });

  it('draws everything for an infinite maxVisible', async () => {
    const host = await mountOpen({ options: MANY, maxVisible: Number.POSITIVE_INFINITY });

    expect(rowLabels(host)).toHaveLength(12);
  });
});

describe('opm-multi-select choosing', () => {
  it('adds a value to the list when its box is checked', async () => {
    const host = await mountOpen({ selected: ['Nikon Z6'] });
    const seen = record(host);

    await check(host, 'Sony A7 IV');

    expect(seen).toEqual([['Nikon Z6', 'Sony A7 IV']]);
  });

  it('removes a value from the list when its box is unchecked', async () => {
    const host = await mountOpen({ selected: ['Nikon Z6', 'Sony A7 IV'] });
    const seen = record(host);

    await check(host, 'Nikon Z6');

    expect(seen).toEqual([['Sony A7 IV']]);
  });

  it('removes every spelling of the value', async () => {
    const host = await mountOpen({ selected: ['nikon z6', 'NIKON Z6', 'Sony A7 IV'] });
    const seen = record(host);

    await check(host, 'Nikon Z6');

    expect(seen).toEqual([['Sony A7 IV']]);
  });

  it('sends a new list and leaves the given one alone', async () => {
    const selected = ['Nikon Z6'];
    const host = await mountOpen({ selected });
    const seen = record(host);

    await check(host, 'Sony A7 IV');

    expect(seen[0]).not.toBe(selected);
    expect(selected).toEqual(['Nikon Z6']);
  });

  it('unchecks a chosen value the options lack', async () => {
    const host = await mountOpen({ selected: ['Leica Q3'] });
    const seen = record(host);

    await check(host, 'Leica Q3');

    expect(seen).toEqual([[]]);
  });

  it('is controlled: the box returns to what `selected` says until it is fed back', async () => {
    const host = await mountOpen();

    await check(host, 'Nikon Z6');
    expect(box(host, 'Nikon Z6').checked).toBe(false);

    await feed(host, ['Nikon Z6']);
    expect(box(host, 'Nikon Z6').checked).toBe(true);
  });

  it('keeps the panel open after a choice', async () => {
    const host = await mountOpen();

    await check(host, 'Nikon Z6');

    expect(details(host).open).toBe(true);
  });

  it('bubbles the event', async () => {
    const host = await mountOpen();
    const seen: string[][] = [];
    document.body.addEventListener('multi-select-change', (event) => seen.push(event.detail), {
      once: true,
    });

    await check(host, 'Nikon Z6');

    expect(seen).toEqual([['Nikon Z6']]);
  });

  it('sends nothing once disabled, even from a box that was already on screen', async () => {
    const host = await mountOpen();
    const seen = record(host);
    const stale = box(host, 'Nikon Z6');

    host.disabled = true;
    await host.updateComplete;
    stale.dispatchEvent(new Event('change', { bubbles: true }));

    expect(seen).toEqual([]);
  });

  describe('Clear', () => {
    it('empties the list and keeps the panel open', async () => {
      const host = await mountOpen({ selected: ['Nikon Z6', 'Sony A7 IV'] });
      const seen = record(host);

      clearButton(host).click();
      await host.updateComplete;

      expect(seen).toEqual([[]]);
      expect(details(host).open).toBe(true);
    });

    it('is disabled when nothing is chosen', async () => {
      const host = await mountOpen();
      const seen = record(host);

      expect(clearButton(host).disabled).toBe(true);
      clearButton(host).click();

      expect(seen).toEqual([]);
    });
  });
});

describe('opm-multi-select custom values', () => {
  it('offers nothing to add by default', async () => {
    const host = await mountOpen();
    await type(host, 'brand new');
    const seen = record(host);

    expect(addRow(host)).toBeNull();
    press(search(host), 'Enter');

    expect(seen).toEqual([]);
  });

  it('names the search box "Search or add"', async () => {
    const host = await mountOpen({ label: 'Tags', allowCustom: true });

    expect(search(host).getAttribute('aria-label')).toBe('Search or add Tags');
  });

  it('shows an Add row for a text that matches nothing', async () => {
    const host = await mountOpen({ allowCustom: true });

    await type(host, 'brand new');

    expect(addRow(host)?.textContent).toContain('Add "brand new"');
  });

  it('shows no Add row for an empty search', async () => {
    const host = await mountOpen({ allowCustom: true });

    expect(addRow(host)).toBeNull();
    await type(host, '   ');
    expect(addRow(host)).toBeNull();
  });

  it('shows no Add row when an option has exactly that text, with any case', async () => {
    const host = await mountOpen({ allowCustom: true });

    await type(host, 'nikon z6');

    expect(addRow(host)).toBeNull();
    expect(rowLabels(host)).toEqual(['Nikon Z6']);
  });

  it('shows an Add row for a text that only starts like an option', async () => {
    const host = await mountOpen({ allowCustom: true });

    await type(host, 'Nikon');

    expect(addRow(host)).not.toBeNull();
  });

  it('shows no Add row for a chosen value the options lack', async () => {
    const host = await mountOpen({ allowCustom: true, selected: ['Leica Q3'] });

    await type(host, 'leica q3');

    expect(addRow(host)).toBeNull();
  });

  it('adds the text, trimmed, when the Add row is pressed, then clears the search', async () => {
    const host = await mountOpen({ allowCustom: true, selected: ['Nikon Z6'] });
    const seen = record(host);
    await type(host, '  brand new ');

    addRow(host)?.click();
    await host.updateComplete;

    expect(seen).toEqual([['Nikon Z6', 'brand new']]);
    expect(search(host).value).toBe('');
    expect(details(host).open).toBe(true);
  });

  it('adds the text with Enter in the search box', async () => {
    const host = await mountOpen({ allowCustom: true });
    const seen = record(host);
    await type(host, 'brand new');

    const event = press(search(host), 'Enter');
    await host.updateComplete;

    expect(event.defaultPrevented).toBe(true);
    expect(seen).toEqual([['brand new']]);
    expect(search(host).value).toBe('');
  });

  it('uses the spelling of the option that matches without case', async () => {
    const host = await mountOpen({ allowCustom: true });
    const seen = record(host);
    await type(host, 'nikon z6');

    press(search(host), 'Enter');

    expect(seen).toEqual([['Nikon Z6']]);
  });

  it('adds nothing for a value that is already chosen, but clears the search', async () => {
    const host = await mountOpen({ allowCustom: true, selected: ['Nikon Z6'] });
    const seen = record(host);
    await type(host, 'NIKON Z6');

    press(search(host), 'Enter');
    await host.updateComplete;

    expect(seen).toEqual([]);
    expect(search(host).value).toBe('');
  });

  it('adds nothing for blank text', async () => {
    const host = await mountOpen({ allowCustom: true });
    const seen = record(host);
    await type(host, '   ');

    press(search(host), 'Enter');

    expect(seen).toEqual([]);
  });

  it('ignores Enter while an input method is composing', async () => {
    const host = await mountOpen({ allowCustom: true });
    const seen = record(host);
    await type(host, 'brand new');

    press(search(host), 'Enter', { isComposing: true });

    expect(seen).toEqual([]);
  });

  it('ignores Enter pressed elsewhere in the panel', async () => {
    const host = await mountOpen({ allowCustom: true });
    const seen = record(host);
    await type(host, 'Nik');

    press(box(host, 'Nikon Z6'), 'Enter');

    expect(seen).toEqual([]);
  });

  it('does not add anything when disabled', async () => {
    const host = await mountOpen({ allowCustom: true });
    const seen = record(host);
    await type(host, 'brand new');
    const add = addRow(host);

    host.disabled = true;
    await host.updateComplete;
    add?.click();

    expect(seen).toEqual([]);
  });
});

describe('opm-multi-select keyboard', () => {
  it('moves down from the search box through the rows, without wrapping', async () => {
    const host = await mountOpen();
    const order = [search(host), ...boxes(host)];

    for (const expected of order.slice(1)) {
      expect(press(document.activeElement ?? search(host), 'ArrowDown').defaultPrevented).toBe(
        true,
      );
      expect(document.activeElement).toBe(expected);
    }
    press(document.activeElement ?? search(host), 'ArrowDown');
    expect(document.activeElement).toBe(order[order.length - 1]);
  });

  it('moves up and stops at the search box', async () => {
    const host = await mountOpen();
    box(host, 'Nikon Z6').focus();

    press(box(host, 'Nikon Z6'), 'ArrowUp');
    expect(document.activeElement).toBe(box(host, 'Canon EOS R5'));
    press(box(host, 'Canon EOS R5'), 'ArrowUp');
    press(box(host, 'Sony A7 IV'), 'ArrowUp');
    expect(document.activeElement).toBe(search(host));
    press(search(host), 'ArrowUp');
    expect(document.activeElement).toBe(search(host));
  });

  it('passes through the Add row between the search box and the rows', async () => {
    const host = await mountOpen({ allowCustom: true });
    await type(host, 'Nik');
    search(host).focus();

    press(search(host), 'ArrowDown');
    expect(document.activeElement).toBe(addRow(host));
    press(addRow(host) as HTMLElement, 'ArrowDown');
    expect(document.activeElement).toBe(box(host, 'Nikon Z6'));
  });

  it('focuses the search box when ArrowDown is pressed on the button of an open panel', async () => {
    const host = await mountOpen();
    trigger(host).focus();

    press(trigger(host), 'ArrowDown');

    expect(document.activeElement).toBe(search(host));
  });

  it('does nothing for ArrowUp on the button of an open panel', async () => {
    const host = await mountOpen();
    trigger(host).focus();

    press(trigger(host), 'ArrowUp');

    expect(document.activeElement).toBe(trigger(host));
  });

  it('leaves shortcuts with a modifier key alone', async () => {
    const host = await mountOpen();

    for (const init of [{ metaKey: true }, { ctrlKey: true }, { altKey: true }]) {
      expect(press(search(host), 'ArrowDown', init).defaultPrevented).toBe(false);
      expect(press(search(host), 'Escape', init).defaultPrevented).toBe(false);
    }
    expect(details(host).open).toBe(true);
  });

  it('toggles a focused checkbox like any native one', async () => {
    const host = await mountOpen();
    const seen = record(host);
    const target = box(host, 'Canon EOS R5');
    target.focus();

    target.click();

    expect(seen).toEqual([['Canon EOS R5']]);
  });
});
