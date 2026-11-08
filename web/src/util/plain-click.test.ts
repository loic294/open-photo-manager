import { describe, expect, it } from 'vitest';
import { isPlainClick } from '@/util/plain-click';

function click(init: MouseEventInit = {}, claim = false): MouseEvent {
  const event = new MouseEvent('click', { bubbles: true, cancelable: true, ...init });
  if (claim) event.preventDefault();
  return event;
}

describe('isPlainClick', () => {
  it('accepts the primary button on its own', () => {
    expect(isPlainClick(click())).toBe(true);
    expect(isPlainClick(click({ button: 0 }))).toBe(true);
  });

  it.each([
    ['middle button', { button: 1 }],
    ['secondary button', { button: 2 }],
    ['Cmd', { metaKey: true }],
    ['Ctrl', { ctrlKey: true }],
    ['Shift', { shiftKey: true }],
    ['Alt', { altKey: true }],
  ] as const)('leaves a %s click to the browser', (_name, init) => {
    expect(isPlainClick(click(init))).toBe(false);
  });

  it('leaves a click that another handler already claimed', () => {
    expect(isPlainClick(click({}, true))).toBe(false);
  });
});
