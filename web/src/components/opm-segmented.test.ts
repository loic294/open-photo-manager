import { describe, expect, it } from 'vitest';
import '@/components/opm-segmented';
import type {
  OpmSegmented,
  SegmentedChangeEvent,
  SegmentedValue,
  SegmentOption,
} from '@/components/opm-segmented';
import { mount } from '@/test/helpers';
import { formatCount } from '@/util/format-count';

const RATINGS: SegmentOption[] = [
  { value: '0', label: 'Unrated' },
  { value: '1', label: '1', name: '1 star', icon: 'star' },
  { value: '2', label: '2', name: '2 stars', icon: 'star' },
  { value: '3', label: '3', name: '3 stars', icon: 'star' },
];

function buttons(host: OpmSegmented): HTMLButtonElement[] {
  return Array.from(host.querySelectorAll('button'));
}

function labelOf(button: HTMLElement): string {
  return button.querySelector('span')?.textContent ?? '';
}

function nameOf(button: HTMLElement): string {
  return button.getAttribute('aria-label') ?? labelOf(button);
}

function button(host: OpmSegmented, name: string): HTMLButtonElement {
  const found = buttons(host).find((candidate) => nameOf(candidate) === name);
  if (!found) throw new Error(`no button "${name}"`);
  return found;
}

function names(host: OpmSegmented): string[] {
  return buttons(host).map(nameOf);
}

function record(host: OpmSegmented): SegmentedValue[] {
  const seen: SegmentedValue[] = [];
  host.addEventListener('segmented-change', (event: SegmentedChangeEvent) => {
    seen.push(event.detail);
  });
  return seen;
}

function press(target: Element, key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

async function feed(host: OpmSegmented, value: SegmentedValue): Promise<void> {
  host.value = value;
  await host.updateComplete;
}

describe('opm-segmented structure', () => {
  it('is an inline DaisyUI join of small buttons', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, label: 'Rating' });

    expect(host.classList.contains('inline-flex')).toBe(true);
    const group = host.querySelector('div.join');
    expect(group).not.toBeNull();
    expect(buttons(host)).toHaveLength(4);
    for (const each of buttons(host)) {
      expect(each.parentElement).toBe(group);
      expect(each.classList.contains('btn')).toBe(true);
      expect(each.classList.contains('btn-sm')).toBe(true);
      expect(each.classList.contains('join-item')).toBe(true);
      expect(each.getAttribute('type')).toBe('button');
    }
  });

  it('renders an empty group without options', async () => {
    const host = await mount('opm-segmented');

    expect(host.querySelector('div.join')).not.toBeNull();
    expect(buttons(host)).toHaveLength(0);
  });

  it('shows the label of every option in order', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    expect(buttons(host).map(labelOf)).toEqual(['Unrated', '1', '2', '3']);
  });

  it('draws the icon of an option, decorative', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    expect(buttons(host)[0]?.querySelector('opm-icon')).toBeNull();
    const icon = buttons(host)[1]?.querySelector('opm-icon');
    expect(icon?.getAttribute('name')).toBe('star');
  });

  it('names the group', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, label: 'Rating' });

    expect(host.querySelector('.join')?.getAttribute('aria-label')).toBe('Rating');
  });

  it('adds no name when none is given', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    expect(host.querySelector('.join')?.hasAttribute('aria-label')).toBe(false);
  });

  it('puts a native tooltip on an option that has one', async () => {
    const host = await mount('opm-segmented', {
      options: [
        { value: 'a', label: 'A', title: 'The first' },
        { value: 'b', label: 'B' },
      ],
    });

    expect(buttons(host)[0]?.getAttribute('title')).toBe('The first');
    expect(buttons(host)[1]?.hasAttribute('title')).toBe(false);
  });

  it('follows the options when they change', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    host.options = [{ value: 'x', label: 'Only' }];
    await host.updateComplete;

    expect(names(host)).toEqual(['Only']);
  });
});

describe('opm-segmented counts and accessible names', () => {
  it('shows a count as a superscript and adds it to the name', async () => {
    const host = await mount('opm-segmented', {
      options: [{ value: '3', label: '3', name: '3 stars', count: 1234 }],
    });

    const only = buttons(host)[0];
    expect(only?.querySelector('sup')?.textContent).toBe(formatCount(1234));
    expect(only?.getAttribute('aria-label')).toBe(`3 stars (${formatCount(1234)})`);
  });

  it('uses the label as the name when there is a count but no name', async () => {
    const host = await mount('opm-segmented', {
      options: [{ value: 'u', label: 'Unrated', count: 7 }],
    });

    expect(buttons(host)[0]?.getAttribute('aria-label')).toBe('Unrated (7)');
  });

  it('shows a zero count', async () => {
    const host = await mount('opm-segmented', {
      options: [{ value: 'u', label: 'Unrated', count: 0 }],
    });

    expect(buttons(host)[0]?.querySelector('sup')?.textContent).toBe('0');
    expect(buttons(host)[0]?.getAttribute('aria-label')).toBe('Unrated (0)');
  });

  it('ignores a count that is not a number', async () => {
    const host = await mount('opm-segmented', {
      options: [
        { value: 'a', label: 'A', count: Number.NaN },
        { value: 'b', label: 'B', count: undefined },
      ],
    });

    for (const each of buttons(host)) {
      expect(each.querySelector('sup')).toBeNull();
      expect(each.hasAttribute('aria-label')).toBe(false);
    }
  });

  it('uses the name alone when there is no count', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    expect(buttons(host)[2]?.getAttribute('aria-label')).toBe('2 stars');
    expect(buttons(host)[2]?.querySelector('sup')).toBeNull();
  });

  it('leaves the name to the visible text when there is neither', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    expect(buttons(host)[0]?.hasAttribute('aria-label')).toBe(false);
  });
});

describe('opm-segmented single mode', () => {
  it('is a radio group of radios', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '1' });

    expect(host.querySelector('.join')?.getAttribute('role')).toBe('radiogroup');
    for (const each of buttons(host)) {
      expect(each.getAttribute('role')).toBe('radio');
      expect(each.hasAttribute('aria-pressed')).toBe(false);
    }
  });

  it('marks the chosen option', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '2' });

    expect(buttons(host).map((each) => each.getAttribute('aria-checked'))).toEqual([
      'false',
      'false',
      'true',
      'false',
    ]);
    expect(buttons(host).map((each) => each.classList.contains('btn-active'))).toEqual([
      false,
      false,
      true,
      false,
    ]);
    expect(button(host, '2 stars').classList.contains('btn-primary')).toBe(true);
  });

  it('is single mode unless told otherwise', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    expect(host.mode).toBe('single');
  });

  it('reads the mode from the attribute', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    host.setAttribute('mode', 'multi');
    await host.updateComplete;

    expect(host.querySelector('.join')?.getAttribute('role')).toBe('group');
  });

  it('chooses the pressed option by sending its value', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '0' });
    const seen = record(host);

    button(host, '3 stars').click();

    expect(seen).toEqual(['3']);
  });

  it('bubbles the event', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '0' });
    const seen: SegmentedValue[] = [];
    document.body.addEventListener('segmented-change', (event) => seen.push(event.detail), {
      once: true,
    });

    button(host, '1 star').click();

    expect(seen).toEqual(['1']);
  });

  it('sends nothing for the option already chosen', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '2' });
    const seen = record(host);

    button(host, '2 stars').click();

    expect(seen).toEqual([]);
  });

  it('is controlled: the choice shows only once the host feeds it back', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '0' });

    button(host, '3 stars').click();
    await host.updateComplete;
    expect(button(host, 'Unrated').getAttribute('aria-checked')).toBe('true');
    expect(button(host, '3 stars').getAttribute('aria-checked')).toBe('false');

    await feed(host, '3');
    expect(button(host, 'Unrated').getAttribute('aria-checked')).toBe('false');
    expect(button(host, '3 stars').getAttribute('aria-checked')).toBe('true');
  });

  it('can have nothing chosen', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '' });

    expect(buttons(host).every((each) => each.getAttribute('aria-checked') === 'false')).toBe(true);
  });

  it('chooses nothing for a value it does not know', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: 'bogus' });

    expect(buttons(host).every((each) => each.getAttribute('aria-checked') === 'false')).toBe(true);
  });

  it('uses the first entry of a list given by mistake', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: ['2', '3'] });

    expect(button(host, '2 stars').getAttribute('aria-checked')).toBe('true');
    expect(button(host, '3 stars').getAttribute('aria-checked')).toBe('false');
  });

  describe('tab stop', () => {
    it('is the chosen option', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '2' });

      expect(buttons(host).map((each) => each.getAttribute('tabindex'))).toEqual([
        '-1',
        '-1',
        '0',
        '-1',
      ]);
    });

    it('is the first option when nothing is chosen', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '' });

      expect(buttons(host).map((each) => each.getAttribute('tabindex'))).toEqual([
        '0',
        '-1',
        '-1',
        '-1',
      ]);
    });

    it('skips a disabled option', async () => {
      const options = RATINGS.map((option, index) => ({ ...option, disabled: index < 2 }));
      const host = await mount('opm-segmented', { options, value: '' });

      expect(buttons(host).map((each) => each.getAttribute('tabindex'))).toEqual([
        '-1',
        '-1',
        '0',
        '-1',
      ]);
    });

    it('is the first enabled option when the chosen one is disabled', async () => {
      const options = RATINGS.map((option) => ({ ...option, disabled: option.value === '2' }));
      const host = await mount('opm-segmented', { options, value: '2' });

      expect(buttons(host).map((each) => each.getAttribute('tabindex'))).toEqual([
        '0',
        '-1',
        '-1',
        '-1',
      ]);
    });
  });

  describe('keyboard', () => {
    it('moves to the next option with ArrowRight and chooses it', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '0' });
      const seen = record(host);
      button(host, 'Unrated').focus();

      const event = press(button(host, 'Unrated'), 'ArrowRight');

      expect(event.defaultPrevented).toBe(true);
      expect(seen).toEqual(['1']);
      expect(document.activeElement).toBe(button(host, '1 star'));
    });

    it('does the same with ArrowDown', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '1' });
      const seen = record(host);

      press(button(host, '1 star'), 'ArrowDown');

      expect(seen).toEqual(['2']);
    });

    it('moves to the previous option with ArrowLeft and ArrowUp', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '2' });
      const seen = record(host);

      press(button(host, '2 stars'), 'ArrowLeft');
      press(button(host, '2 stars'), 'ArrowUp');

      expect(seen).toEqual(['1', '1']);
    });

    it('wraps around at both ends', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '3' });
      const seen = record(host);

      press(button(host, '3 stars'), 'ArrowRight');
      await feed(host, '0');
      press(button(host, 'Unrated'), 'ArrowLeft');

      expect(seen).toEqual(['0', '3']);
    });

    it('jumps to the first and last option with Home and End', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '2' });
      const seen = record(host);

      press(button(host, '2 stars'), 'Home');
      press(button(host, '2 stars'), 'End');

      expect(seen).toEqual(['0', '3']);
    });

    it('skips disabled options', async () => {
      const options = RATINGS.map((option) => ({ ...option, disabled: option.value === '1' }));
      const host = await mount('opm-segmented', { options, value: '0' });
      const seen = record(host);

      press(button(host, 'Unrated'), 'ArrowRight');
      press(button(host, 'Unrated'), 'ArrowLeft');

      expect(seen).toEqual(['2', '3']);
    });

    it('moves focus without sending when the option is already chosen', async () => {
      const host = await mount('opm-segmented', {
        options: [
          { value: 'a', label: 'A' },
          { value: 'b', label: 'B' },
        ],
        value: 'a',
      });
      const seen = record(host);
      button(host, 'A').focus();

      press(button(host, 'A'), 'Home');

      expect(seen).toEqual([]);
      expect(document.activeElement).toBe(button(host, 'A'));
    });

    it('stays put with a single enabled option', async () => {
      const host = await mount('opm-segmented', {
        options: [{ value: 'a', label: 'A' }],
        value: 'a',
      });
      const seen = record(host);

      press(button(host, 'A'), 'ArrowRight');

      expect(seen).toEqual([]);
    });

    it('leaves other keys and shortcuts alone', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '0' });
      const seen = record(host);

      expect(press(button(host, 'Unrated'), 'a').defaultPrevented).toBe(false);
      expect(press(button(host, 'Unrated'), 'Tab').defaultPrevented).toBe(false);
      expect(press(button(host, 'Unrated'), 'ArrowRight', { metaKey: true }).defaultPrevented).toBe(
        false,
      );
      expect(
        press(button(host, 'Unrated'), 'ArrowRight', { shiftKey: true }).defaultPrevented,
      ).toBe(false);
      expect(press(button(host, 'Unrated'), 'ArrowRight', { altKey: true }).defaultPrevented).toBe(
        false,
      );
      expect(press(button(host, 'Unrated'), 'ArrowRight', { ctrlKey: true }).defaultPrevented).toBe(
        false,
      );
      expect(seen).toEqual([]);
    });

    it('does nothing for a key pressed on the group itself', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '0' });
      const seen = record(host);

      const group = host.querySelector('.join');
      if (!group) throw new Error('no group');
      press(group, 'ArrowRight');

      expect(seen).toEqual([]);
    });

    it('does nothing when the whole group is disabled', async () => {
      const host = await mount('opm-segmented', { options: RATINGS, value: '0', disabled: true });
      const seen = record(host);

      press(button(host, 'Unrated'), 'ArrowRight');

      expect(seen).toEqual([]);
    });
  });
});

describe('opm-segmented multi mode', () => {
  const RATING_VALUES = RATINGS;

  it('is a group of toggle buttons', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATING_VALUES,
      value: ['1'],
    });

    expect(host.querySelector('.join')?.getAttribute('role')).toBe('group');
    for (const each of buttons(host)) {
      expect(each.hasAttribute('role')).toBe(false);
      expect(each.hasAttribute('aria-checked')).toBe(false);
      expect(each.hasAttribute('tabindex')).toBe(false);
    }
  });

  it('marks every chosen option', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATING_VALUES,
      value: ['0', '2'],
    });

    expect(buttons(host).map((each) => each.getAttribute('aria-pressed'))).toEqual([
      'true',
      'false',
      'true',
      'false',
    ]);
    expect(buttons(host).map((each) => each.classList.contains('btn-active'))).toEqual([
      true,
      false,
      true,
      false,
    ]);
  });

  it('adds an option to the list when pressed', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATING_VALUES,
      value: ['0'],
    });
    const seen = record(host);

    button(host, '2 stars').click();

    expect(seen).toEqual([['0', '2']]);
  });

  it('removes a chosen option when pressed', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATING_VALUES,
      value: ['0', '2', '3'],
    });
    const seen = record(host);

    button(host, '2 stars').click();

    expect(seen).toEqual([['0', '3']]);
  });

  it('can empty the list', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATING_VALUES,
      value: ['3'],
    });
    const seen = record(host);

    button(host, '3 stars').click();

    expect(seen).toEqual([[]]);
  });

  it('sends a new list and leaves the given one alone', async () => {
    const value = ['0'];
    const host = await mount('opm-segmented', { mode: 'multi', options: RATING_VALUES, value });
    const seen = record(host);

    button(host, '1 star').click();

    expect(seen[0]).not.toBe(value);
    expect(value).toEqual(['0']);
  });

  it('is controlled: the choice shows only once the host feeds it back', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATING_VALUES,
      value: [],
    });

    button(host, '1 star').click();
    await host.updateComplete;
    expect(button(host, '1 star').getAttribute('aria-pressed')).toBe('false');

    await feed(host, ['1']);
    expect(button(host, '1 star').getAttribute('aria-pressed')).toBe('true');
  });

  it('treats a lone string as a list of one', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATING_VALUES,
      value: '1',
    });
    const seen = record(host);

    expect(button(host, '1 star').getAttribute('aria-pressed')).toBe('true');
    button(host, '2 stars').click();

    expect(seen).toEqual([['1', '2']]);
  });

  it('leaves the arrow keys alone', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATING_VALUES,
      value: [],
    });
    const seen = record(host);

    const event = press(button(host, 'Unrated'), 'ArrowRight');

    expect(event.defaultPrevented).toBe(false);
    expect(seen).toEqual([]);
  });
});

describe('opm-segmented disabled', () => {
  it('disables every button and sends nothing', async () => {
    const host = await mount('opm-segmented', {
      mode: 'multi',
      options: RATINGS,
      value: [],
      disabled: true,
    });
    const seen = record(host);

    for (const each of buttons(host)) {
      expect(each.disabled).toBe(true);
      each.click();
    }

    expect(seen).toEqual([]);
  });

  it('disables every radio in single mode too', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '0', disabled: true });
    const seen = record(host);

    for (const each of buttons(host)) {
      expect(each.disabled).toBe(true);
      each.click();
    }

    expect(seen).toEqual([]);
  });

  it('reads the disabled attribute', async () => {
    const host = await mount('opm-segmented', { options: RATINGS });

    host.toggleAttribute('disabled', true);
    await host.updateComplete;

    expect(buttons(host).every((each) => each.disabled)).toBe(true);
  });

  it('disables one option and leaves the others', async () => {
    const options = RATINGS.map((option) => ({ ...option, disabled: option.value === '2' }));
    const host = await mount('opm-segmented', { mode: 'multi', options, value: [] });
    const seen = record(host);

    button(host, '2 stars').click();
    button(host, '1 star').click();

    expect(button(host, '2 stars').disabled).toBe(true);
    expect(button(host, '1 star').disabled).toBe(false);
    expect(seen).toEqual([['1']]);
  });

  it('can be enabled again', async () => {
    const host = await mount('opm-segmented', { options: RATINGS, value: '0', disabled: true });
    const seen = record(host);

    host.disabled = false;
    await host.updateComplete;
    button(host, '1 star').click();

    expect(seen).toEqual(['1']);
  });
});
