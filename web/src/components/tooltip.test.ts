import { html, render } from 'lit';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { tooltip } from './tooltip';

const hosts: HTMLElement[] = [];
const originalPopover = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'showPopover');

beforeEach(() => {
  vi.useFakeTimers();
  Object.defineProperty(HTMLElement.prototype, 'showPopover', {
    configurable: true,
    value: vi.fn(),
  });
});

afterEach(() => {
  for (const host of hosts.splice(0)) {
    render(html``, host);
    host.remove();
  }
  vi.restoreAllMocks();
  vi.useRealTimers();
  if (originalPopover) Object.defineProperty(HTMLElement.prototype, 'showPopover', originalPopover);
  else Reflect.deleteProperty(HTMLElement.prototype, 'showPopover');
});

function trigger(text = 'Help <b>as text</b>') {
  const host = document.createElement('div');
  document.body.append(host);
  hosts.push(host);
  render(html`<button aria-describedby="existing" ${tooltip(text)}>Action</button>`, host);
  const button = host.querySelector('button');
  if (!button) throw new Error('Missing button');
  return { host, button };
}

it('shows escaped DaisyUI help in the top layer on hover and associates it with the trigger', () => {
  const { button } = trigger();
  const show = vi.spyOn(HTMLElement.prototype, 'showPopover');
  button.dispatchEvent(new Event('pointerenter'));
  const popup = document.querySelector('.tooltip[popover]');
  expect(popup?.classList.contains('tooltip-open')).toBe(true);
  expect(popup?.querySelector('[role="tooltip"]')?.textContent).toBe('Help <b>as text</b>');
  expect(popup?.querySelector('b')).toBeNull();
  expect(show).toHaveBeenCalledOnce();
  expect(button.getAttribute('aria-describedby')).toBe(`existing ${popup?.id}`);
  expect(button.hasAttribute('title')).toBe(false);
  button.dispatchEvent(new Event('pointerleave'));
  vi.advanceTimersByTime(100);
  expect(document.querySelector('.tooltip[popover]')).toBeNull();
  expect(button.getAttribute('aria-describedby')).toBe('existing');
});

it('supports focus, Escape, scrolling, live updates and disconnected triggers without leaking help', () => {
  const { host, button } = trigger('First');
  button.focus();
  expect(document.querySelector('[role="tooltip"]')?.textContent).toBe('First');
  const escapeKey = new KeyboardEvent('keydown', { key: 'Escape', cancelable: true });
  document.dispatchEvent(escapeKey);
  expect(escapeKey.defaultPrevented).toBe(false);
  expect(document.querySelector('[role="tooltip"]')).toBeNull();
  button.blur();
  button.focus();
  document.dispatchEvent(new Event('scroll'));
  expect(document.querySelector('[role="tooltip"]')?.textContent).toBe('First');
  render(html`<button ${tooltip('Second')}>Action</button>`, host);
  const second = host.querySelector('button');
  second?.focus();
  expect(document.querySelector('[role="tooltip"]')?.textContent).toBe('Second');
  render(html``, host);
  expect(document.querySelector('[role="tooltip"]')).toBeNull();
});

it('repositions focused help when focus scrolls its container, but dismisses hover help on scroll', () => {
  const { host, button } = trigger();
  let top = 100;
  vi.spyOn(button, 'getBoundingClientRect').mockImplementation(() => new DOMRect(100, top, 40, 20));
  button.focus();
  const popup = document.querySelector<HTMLElement>('.tooltip[popover]');
  expect(popup?.style.top).toBe('100px');
  top = 200;
  host.dispatchEvent(new Event('scroll'));
  expect(popup?.isConnected).toBe(true);
  expect(popup?.style.top).toBe('200px');
  button.blur();
  button.dispatchEvent(new Event('pointerenter'));
  host.dispatchEvent(new Event('scroll'));
  expect(document.querySelector('[role="tooltip"]')).toBeNull();
});

it('opens help in its modal and replaces any other active tooltip', () => {
  const { button } = trigger();
  button.focus();
  const host = document.createElement('div');
  document.body.append(host);
  hosts.push(host);
  render(html`<dialog open><span tabindex="0" ${tooltip('Modal help')}>Info</span></dialog>`, host);
  host.querySelector('span')?.dispatchEvent(new Event('pointerenter'));
  expect(document.querySelectorAll('[role="tooltip"]')).toHaveLength(1);
  expect(host.querySelector('dialog > .tooltip .tooltip-content')?.textContent).toBe('Modal help');
});

it('positions menu help outside its menu so it does not block the other actions', () => {
  const { host, button } = trigger();
  host.setAttribute('role', 'menu');
  vi.spyOn(host, 'getBoundingClientRect').mockReturnValue(new DOMRect(10, 300, 240, 200));
  vi.spyOn(button, 'getBoundingClientRect').mockReturnValue(new DOMRect(210, 460, 24, 24));
  button.focus();
  expect(document.querySelector<HTMLElement>('.tooltip[popover]')?.style.top).toBe('300px');
});

it('does not reintroduce native title tooltips in application templates', () => {
  const sources = import.meta.glob<string>(
    ['../**/*.ts', '!../**/*.test.ts', '!../api/generated/**'],
    {
      query: '?raw',
      import: 'default',
      eager: true,
    },
  );
  for (const [path, source] of Object.entries(sources)) {
    expect(source, path).not.toMatch(/(?<![\w.-])title\s*=\s*(?:["']|\$\{)/);
  }
});
