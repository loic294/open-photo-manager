import { describe, expect, it } from 'vitest';
import { type ActivationVia, type Decision, interpret } from '@/util/selection-rules';

interface Row {
  name: string;
  via: ActivationVia;
  shift: boolean;
  toggle: boolean;
  active: boolean;
  expected: Decision['type'];
}

const ROWS: Row[] = [
  // enter opens, always
  {
    name: 'enter opens',
    via: 'enter',
    shift: false,
    toggle: false,
    active: false,
    expected: 'open',
  },
  {
    name: 'enter opens while active',
    via: 'enter',
    shift: false,
    toggle: false,
    active: true,
    expected: 'open',
  },
  {
    name: 'enter ignores shift',
    via: 'enter',
    shift: true,
    toggle: false,
    active: true,
    expected: 'open',
  },
  {
    name: 'enter ignores toggle',
    via: 'enter',
    shift: false,
    toggle: true,
    active: true,
    expected: 'open',
  },
  {
    name: 'enter ignores both modifiers',
    via: 'enter',
    shift: true,
    toggle: true,
    active: false,
    expected: 'open',
  },
  // space
  {
    name: 'space toggles',
    via: 'space',
    shift: false,
    toggle: false,
    active: false,
    expected: 'toggle',
  },
  {
    name: 'space toggles while active',
    via: 'space',
    shift: false,
    toggle: false,
    active: true,
    expected: 'toggle',
  },
  {
    name: 'space with toggle toggles',
    via: 'space',
    shift: false,
    toggle: true,
    active: false,
    expected: 'toggle',
  },
  {
    name: 'shift+space ranges',
    via: 'space',
    shift: true,
    toggle: false,
    active: false,
    expected: 'range',
  },
  {
    name: 'shift+space ranges while active',
    via: 'space',
    shift: true,
    toggle: false,
    active: true,
    expected: 'range',
  },
  // checkbox
  {
    name: 'checkbox toggles',
    via: 'checkbox',
    shift: false,
    toggle: false,
    active: false,
    expected: 'toggle',
  },
  {
    name: 'checkbox toggles while active',
    via: 'checkbox',
    shift: false,
    toggle: false,
    active: true,
    expected: 'toggle',
  },
  {
    name: 'checkbox with toggle toggles',
    via: 'checkbox',
    shift: false,
    toggle: true,
    active: true,
    expected: 'toggle',
  },
  {
    name: 'shift+checkbox ranges',
    via: 'checkbox',
    shift: true,
    toggle: false,
    active: false,
    expected: 'range',
  },
  {
    name: 'shift+checkbox ranges while active',
    via: 'checkbox',
    shift: true,
    toggle: false,
    active: true,
    expected: 'range',
  },
  // card
  {
    name: 'shift+card ranges while inactive',
    via: 'card',
    shift: true,
    toggle: false,
    active: false,
    expected: 'range',
  },
  {
    name: 'shift+card ranges while active',
    via: 'card',
    shift: true,
    toggle: false,
    active: true,
    expected: 'range',
  },
  {
    name: 'shift wins over toggle',
    via: 'card',
    shift: true,
    toggle: true,
    active: true,
    expected: 'range',
  },
  {
    name: 'cmd/ctrl+card toggles while inactive',
    via: 'card',
    shift: false,
    toggle: true,
    active: false,
    expected: 'toggle',
  },
  {
    name: 'cmd/ctrl+card toggles while active',
    via: 'card',
    shift: false,
    toggle: true,
    active: true,
    expected: 'toggle',
  },
  {
    name: 'plain card toggles while active',
    via: 'card',
    shift: false,
    toggle: false,
    active: true,
    expected: 'toggle',
  },
  {
    name: 'plain card opens while inactive',
    via: 'card',
    shift: false,
    toggle: false,
    active: false,
    expected: 'open',
  },
];

describe('interpret', () => {
  it.each(ROWS)('$name', ({ via, shift, toggle, active, expected }) => {
    expect(interpret({ id: 7, via, shift, toggle }, { active })).toEqual({ type: expected, id: 7 });
  });

  it('keeps the id of the activation', () => {
    expect(
      interpret({ id: 123, via: 'card', shift: true, toggle: false }, { active: false }),
    ).toEqual({
      type: 'range',
      id: 123,
    });
    expect(
      interpret({ id: 0, via: 'enter', shift: false, toggle: false }, { active: false }),
    ).toEqual({
      type: 'open',
      id: 0,
    });
  });
});
