import { describe, expect, it } from 'vitest';
import '@/components/opm-chip-input';
import type { ChipValuesChange, OpmChipInput } from '@/components/opm-chip-input';
import { mount } from '@/test/helpers';

const extension = (raw: string) => raw.trim().replace(/^\./, '').toLowerCase();
const letters = (entry: string) => (/^[a-z0-9]+$/.test(entry) ? null : 'must be letters or digits');
const anyName = { normalizeEntry: (raw: string) => raw.trim(), validateEntry: () => null };

async function field(props: Partial<OpmChipInput> = {}) {
  const element = await mount('opm-chip-input', {
    label: 'Sidecar extensions',
    normalizeEntry: extension,
    validateEntry: letters,
    ...props,
  });
  const changes: string[][] = [];
  element.addEventListener('values-change', (event) => {
    changes.push((event as ChipValuesChange).detail.values);
  });
  return { element, changes };
}

const box = (element: HTMLElement) => element.querySelector('input') as HTMLInputElement;
const chipTexts = (element: HTMLElement) =>
  [...element.querySelectorAll('li')].map((chip) => chip.querySelector('span')?.textContent);
const messageOf = (element: HTMLElement) => element.querySelector('p');

function type(element: HTMLElement, text: string): void {
  const input = box(element);
  input.value = text;
  input.dispatchEvent(new InputEvent('input', { bubbles: true }));
}

function press(element: HTMLElement, key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
  box(element).dispatchEvent(event);
  return event;
}

function paste(element: HTMLElement, text: string): Event {
  const event = new Event('paste', { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'clipboardData', { value: { getData: () => text } });
  box(element).dispatchEvent(event);
  return event;
}

describe('opm-chip-input', () => {
  describe('markup', () => {
    it('shows the label for the text box, the chips and a remove button on each', async () => {
      const { element } = await field({ values: ['xmp', 'json'] });

      const label = element.querySelector('label');
      expect(label?.textContent).toBe('Sidecar extensions');
      expect(label?.getAttribute('for')).toBe(box(element).id);
      expect(chipTexts(element)).toEqual(['xmp', 'json']);
      expect(
        [...element.querySelectorAll('button')].map((button) => button.getAttribute('aria-label')),
      ).toEqual(['Remove xmp', 'Remove json']);
      expect(element.querySelector('ul')?.getAttribute('aria-label')).toBe('Sidecar extensions');
    });

    it('is a block, and each instance has its own ids', async () => {
      const { element: first } = await field();
      const { element: second } = await field({ hint: 'help' });

      expect(first.classList.contains('block')).toBe(true);
      expect(box(first).id).not.toBe(box(second).id);
    });

    it('still leaves the document when remove() is called on it', async () => {
      const { element, changes } = await field({ values: ['xmp'] });

      element.remove();

      expect(element.isConnected).toBe(false);
      expect(changes).toEqual([]);
    });

    it('shows the placeholder only while the list is empty', async () => {
      const { element } = await field({ placeholder: 'xmp' });
      expect(box(element).getAttribute('placeholder')).toBe('xmp');

      element.values = ['json'];
      await element.updateComplete;

      expect(box(element).hasAttribute('placeholder')).toBe(false);
    });

    it('shows the hint, tied to the text box, until there is a message', async () => {
      const { element } = await field({ hint: 'Without the dot.' });

      const hint = messageOf(element);
      expect(hint?.textContent).toBe('Without the dot.');
      expect(hint?.getAttribute('role')).toBeNull();
      expect(box(element).getAttribute('aria-describedby')).toBe(hint?.id);
      expect(box(element).hasAttribute('aria-invalid')).toBe(false);
    });

    it('has no message line without a hint, error or message', async () => {
      const { element } = await field();

      expect(messageOf(element)).toBeNull();
      expect(box(element).hasAttribute('aria-describedby')).toBe(false);
    });

    it('focuses the text box when focused or when its area is clicked', async () => {
      const { element } = await field({ values: ['xmp'] });

      element.focus();
      expect(document.activeElement).toBe(box(element));

      box(element).blur();
      (element.querySelector('div.input') as HTMLElement).click();
      expect(document.activeElement).toBe(box(element));
    });
  });

  describe('adding', () => {
    it('adds the typed text on Enter, normalised, and empties the box', async () => {
      const { element, changes } = await field({ values: ['xmp'] });

      type(element, ' .JSON ');
      const enter = press(element, 'Enter');
      await element.updateComplete;

      expect(enter.defaultPrevented).toBe(true);
      expect(changes).toEqual([['xmp', 'json']]);
      expect(chipTexts(element)).toEqual(['xmp', 'json']);
      expect(box(element).value).toBe('');
    });

    it('keeps Enter from submitting a form even when there is nothing to add', async () => {
      const { element, changes } = await field();

      const enter = press(element, 'Enter');

      expect(enter.defaultPrevented).toBe(true);
      expect(changes).toEqual([]);
    });

    it('ignores Enter while an input method is composing', async () => {
      const { element, changes } = await field();
      type(element, 'xmp');

      const enter = press(element, 'Enter', { isComposing: true });

      expect(enter.defaultPrevented).toBe(false);
      expect(changes).toEqual([]);
      expect(box(element).value).toBe('xmp');
    });

    it('adds an entry when a comma is typed and keeps what follows it', async () => {
      const { element, changes } = await field();

      type(element, 'xmp,');
      expect(box(element).value).toBe('');
      type(element, 'json,aa');
      await element.updateComplete;

      expect(changes).toEqual([['xmp'], ['xmp', 'json']]);
      expect(box(element).value).toBe('aa');
    });

    it('also ends an entry at a space, unless entries may contain spaces', async () => {
      const { element, changes } = await field();
      type(element, 'xmp ');
      expect(changes).toEqual([['xmp']]);

      const { element: names, changes: nameChanges } = await field({
        ...anyName,
        allowSpaces: true,
      });
      type(names, 'Photo Booth');
      expect(nameChanges).toEqual([]);
      expect(box(names).value).toBe('Photo Booth');
      press(names, 'Enter');
      expect(nameChanges).toEqual([['Photo Booth']]);
    });

    it('adds what is typed when the box loses focus', async () => {
      const { element, changes } = await field();
      type(element, 'xmp');

      box(element).dispatchEvent(new FocusEvent('blur'));
      await element.updateComplete;

      expect(changes).toEqual([['xmp']]);
      expect(box(element).value).toBe('');
    });

    it('does nothing when it loses focus with an empty box', async () => {
      const { element, changes } = await field();

      box(element).dispatchEvent(new FocusEvent('blur'));

      expect(changes).toEqual([]);
      expect(messageOf(element)).toBeNull();
    });

    it('adds every entry of pasted text that holds separators', async () => {
      const { element, changes } = await field();

      const event = paste(element, 'xmp, JSON\naae  dop');
      await element.updateComplete;

      expect(event.defaultPrevented).toBe(true);
      expect(changes).toEqual([['xmp', 'json', 'aae', 'dop']]);
      expect(box(element).value).toBe('');
    });

    it('puts pasted text at the caret, inside what was typed already', async () => {
      const { element, changes } = await field();
      const input = box(element);
      input.value = 'ab';
      input.setSelectionRange(1, 1);

      paste(element, 'x,y');

      expect(changes).toEqual([['ax', 'yb']]);
    });

    it('leaves a paste without separators to the browser', async () => {
      const { element, changes } = await field();

      const event = paste(element, 'xmp');

      expect(event.defaultPrevented).toBe(false);
      expect(changes).toEqual([]);
    });

    it('treats pasted spaces as part of a name when entries may contain spaces', async () => {
      const { element, changes } = await field({ ...anyName, allowSpaces: true });

      expect(paste(element, 'Photo Booth').defaultPrevented).toBe(false);
      paste(element, 'Thumbs.db,Photo Booth');

      expect(changes).toEqual([['Thumbs.db', 'Photo Booth']]);
    });

    it('skips blank entries without a message', async () => {
      const { element, changes } = await field();

      type(element, '.');
      press(element, 'Enter');
      type(element, ' , ,');

      expect(changes).toEqual([]);
      expect(messageOf(element)).toBeNull();
    });

    it('does not add what is in the list already, whatever its case, and says so', async () => {
      const { element, changes } = await field({ ...anyName, values: ['Photo'] });

      type(element, 'photo');
      press(element, 'Enter');
      await element.updateComplete;

      expect(changes).toEqual([]);
      expect(box(element).value).toBe('');
      const note = messageOf(element);
      expect(note?.textContent).toBe('"photo" is already in the list');
      expect(note?.classList.contains('validator-hint')).toBe(false);
      expect(box(element).hasAttribute('aria-invalid')).toBe(false);
    });

    it('does not add the same entry twice in one go', async () => {
      const { element, changes } = await field();

      paste(element, 'xmp, XMP, xmp');

      expect(changes).toEqual([['xmp']]);
    });
  });

  describe('refused entries', () => {
    it('stay in the box with the reason, and mark the field invalid', async () => {
      const { element, changes } = await field({ values: ['xmp'] });

      type(element, 'tar.gz');
      press(element, 'Enter');
      await element.updateComplete;

      expect(changes).toEqual([]);
      expect(box(element).value).toBe('tar.gz');
      const alert = messageOf(element);
      expect(alert?.textContent).toBe('"tar.gz" must be letters or digits');
      expect(alert?.getAttribute('role')).toBe('alert');
      expect(alert?.classList.contains('validator-hint')).toBe(true);
      expect(box(element).getAttribute('aria-invalid')).toBe('true');
      expect(box(element).getAttribute('aria-describedby')).toBe(alert?.id);
      expect(element.querySelector('div.input')?.classList.contains('validator')).toBe(true);
    });

    it('add the valid ones of a paste and keep the refused ones, as typed', async () => {
      const { element, changes } = await field();

      paste(element, 'xmp, a.b, json, c/d');
      await element.updateComplete;

      expect(changes).toEqual([['xmp', 'json']]);
      expect(box(element).value).toBe('a.b, c/d');
      expect(messageOf(element)?.textContent).toBe('"a.b" must be letters or digits');
    });

    it('clear their message as soon as the text changes', async () => {
      const { element } = await field();
      type(element, 'a.b');
      press(element, 'Enter');
      await element.updateComplete;
      expect(messageOf(element)).not.toBeNull();

      type(element, 'a.');
      await element.updateComplete;

      expect(messageOf(element)).toBeNull();
      expect(box(element).hasAttribute('aria-invalid')).toBe(false);
    });

    it('are retried from the text box and added once fixed', async () => {
      const { element, changes } = await field();
      type(element, 'a.b');
      press(element, 'Enter');

      type(element, 'ab');
      press(element, 'Enter');
      await element.updateComplete;

      expect(changes).toEqual([['ab']]);
      expect(messageOf(element)).toBeNull();
    });

    it('include the entries beyond the limit', async () => {
      const { element, changes } = await field({ values: ['a', 'b'], max: 3 });

      paste(element, 'c, d, e');
      await element.updateComplete;

      expect(changes).toEqual([['a', 'b', 'c']]);
      expect(box(element).value).toBe('d, e');
      expect(messageOf(element)?.textContent).toBe('At most 3 entries are allowed');
    });
  });

  describe('removing', () => {
    it('removes a chip with its button, tells the listener and focuses the text box', async () => {
      const { element, changes } = await field({ values: ['xmp', 'json', 'aae'] });

      (element.querySelectorAll('button')[1] as HTMLButtonElement).click();
      await element.updateComplete;

      expect(changes).toEqual([['xmp', 'aae']]);
      expect(chipTexts(element)).toEqual(['xmp', 'aae']);
      expect(document.activeElement).toBe(box(element));
    });

    it('removes the last chip with Backspace in an empty box only', async () => {
      const { element, changes } = await field({ values: ['xmp', 'json'] });
      type(element, 'a');
      press(element, 'Backspace');
      expect(changes).toEqual([]);

      type(element, '');
      press(element, 'Backspace');
      await element.updateComplete;

      expect(changes).toEqual([['xmp']]);
      expect(chipTexts(element)).toEqual(['xmp']);
    });

    it('does nothing on Backspace when the list is empty', async () => {
      const { element, changes } = await field();

      press(element, 'Backspace');

      expect(changes).toEqual([]);
    });
  });

  describe('values', () => {
    it('are never mutated: a change makes a new array and the event carries a copy', async () => {
      const original = Object.freeze(['xmp']) as readonly string[];
      const { element, changes } = await field({ values: original });

      type(element, 'json,');

      expect(original).toEqual(['xmp']);
      expect(element.values).toEqual(['xmp', 'json']);
      expect(element.values).not.toBe(original);
      expect(changes[0]).not.toBe(element.values);
    });

    it('follow the property when the parent sets it', async () => {
      const { element } = await field({ values: ['xmp'] });

      element.values = ['a', 'b', 'c'];
      await element.updateComplete;

      expect(chipTexts(element)).toEqual(['a', 'b', 'c']);
    });

    it('bubble out of the field as a values-change event', async () => {
      const { element } = await field();
      const heard: ChipValuesChange[] = [];
      document.body.addEventListener('values-change', (event) => {
        heard.push(event as ChipValuesChange);
      });

      type(element, 'xmp,');

      expect(heard).toHaveLength(1);
      expect(heard[0]?.detail.values).toEqual(['xmp']);
    });
  });

  describe('state from the parent', () => {
    it('shows an external error instead of the hint, and marks the field invalid', async () => {
      const { element } = await field({ hint: 'Without the dot.', error: '"a.b" is not valid' });

      const alert = messageOf(element);
      expect(alert?.textContent).toBe('"a.b" is not valid');
      expect(alert?.getAttribute('role')).toBe('alert');
      expect(box(element).getAttribute('aria-invalid')).toBe('true');
      expect(element.querySelector('div.input')?.classList.contains('validator')).toBe(true);
    });

    it('lets the external error win over a message of its own', async () => {
      const { element } = await field({ error: 'The server said no' });

      type(element, 'a.b');
      press(element, 'Enter');
      await element.updateComplete;

      expect(messageOf(element)?.textContent).toBe('The server said no');
    });

    it('marks the chips at errorIndexes', async () => {
      const { element } = await field({ values: ['xmp', 'a.b', 'json'], errorIndexes: [1] });

      const marked = [...element.querySelectorAll('li')].map((chip) =>
        chip.classList.contains('badge-error'),
      );
      expect(marked).toEqual([false, true, false]);
    });

    it('can be disabled: the text box and the remove buttons stop working', async () => {
      const { element } = await field({ values: ['xmp'], disabled: true });

      expect(box(element).disabled).toBe(true);
      expect((element.querySelector('button') as HTMLButtonElement).disabled).toBe(true);
      expect(element.hasAttribute('disabled')).toBe(true);
    });
  });
});
