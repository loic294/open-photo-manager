import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import '@/components/apm-toast-host';
import { toast } from '@/components/toast';
import { clearToasts, toasts } from '@/stores/toasts';
import { mount } from '@/test/helpers';

beforeEach(() => {
  vi.useFakeTimers();
  clearToasts();
});

afterEach(() => {
  clearToasts();
  vi.useRealTimers();
});

async function mountHost() {
  return mount('apm-toast-host');
}

function items(host: HTMLElement) {
  return [...host.querySelectorAll<HTMLElement>('.alert')];
}

describe('apm-toast-host', () => {
  it('renders an empty polite live region when there are no toasts', async () => {
    const host = await mountHost();

    const region = host.querySelector('.toast');
    expect(region?.getAttribute('role')).toBe('status');
    expect(region?.getAttribute('aria-live')).toBe('polite');
    expect(region?.getAttribute('aria-atomic')).toBe('false');
    expect(items(host)).toHaveLength(0);
  });

  it('is a DaisyUI toast in the bottom end corner above the layout', async () => {
    const host = await mountHost();

    const classes = host.querySelector('.toast')?.classList;
    expect(classes?.contains('toast-end')).toBe(true);
    expect(classes?.contains('toast-bottom')).toBe(true);
    expect(classes?.contains('z-50')).toBe(true);
  });

  it.each([
    ['success', 'alert-success'],
    ['info', 'alert-info'],
    ['warning', 'alert-warning'],
    ['error', 'alert-error'],
  ] as const)('renders a %s toast as %s', async (kind, expected) => {
    const host = await mountHost();

    toast[kind]('Hello');
    await host.updateComplete;

    const [alert] = items(host);
    expect(alert?.classList.contains(expected)).toBe(true);
    expect(alert?.textContent).toContain('Hello');
    expect(alert?.querySelector('apm-icon')).not.toBeNull();
  });

  it('announces errors immediately and the rest politely', async () => {
    const host = await mountHost();

    toast.error('Broken');
    toast.info('FYI');
    await host.updateComplete;

    const [error, info] = items(host);
    expect(error?.getAttribute('role')).toBe('alert');
    expect(info?.hasAttribute('role')).toBe(false);
  });

  it('shows the message as text, never as markup', async () => {
    const host = await mountHost();

    toast.info('<img src=x onerror=alert(1)>');
    await host.updateComplete;

    expect(host.querySelector('.alert img')).toBeNull();
    expect(host.querySelector('.alert span')?.textContent).toBe('<img src=x onerror=alert(1)>');
  });

  it('keeps toasts in order and removes one with its dismiss button', async () => {
    const host = await mountHost();
    toast.info('first');
    toast.info('second');
    await host.updateComplete;

    const first = items(host)[0]?.querySelector<HTMLButtonElement>('button');
    expect(first?.getAttribute('aria-label')).toBe('Dismiss notification');
    first?.click();
    await host.updateComplete;

    expect(items(host).map((item) => item.textContent?.trim())).toEqual(['second']);
    expect(toasts.value).toHaveLength(1);
  });

  it('removes a toast when its time is up', async () => {
    const host = await mountHost();
    toast.success('bye');
    await host.updateComplete;

    vi.advanceTimersByTime(4000);
    await host.updateComplete;

    expect(items(host)).toHaveLength(0);
  });

  it('keeps a toast while the pointer is on it and restarts its time afterwards', async () => {
    const host = await mountHost();
    toast.info('reading');
    await host.updateComplete;
    const alert = items(host)[0] as HTMLElement;

    alert.dispatchEvent(new MouseEvent('mouseenter'));
    vi.advanceTimersByTime(60_000);
    expect(toasts.value).toHaveLength(1);

    alert.dispatchEvent(new MouseEvent('mouseleave'));
    vi.advanceTimersByTime(4000);
    expect(toasts.value).toHaveLength(0);
  });

  it('keeps a toast while it has keyboard focus', async () => {
    const host = await mountHost();
    toast.warning('focus me');
    await host.updateComplete;
    const alert = items(host)[0] as HTMLElement;

    alert.dispatchEvent(new FocusEvent('focusin', { bubbles: true }));
    vi.advanceTimersByTime(60_000);
    expect(toasts.value).toHaveLength(1);

    alert.dispatchEvent(new FocusEvent('focusout', { bubbles: true }));
    vi.advanceTimersByTime(6000);
    expect(toasts.value).toHaveLength(0);
  });

  it('wraps long messages instead of widening the toast', async () => {
    const host = await mountHost();

    toast.info('x'.repeat(500));
    await host.updateComplete;

    expect(items(host)[0]?.classList.contains('max-w-md')).toBe(true);
    expect(host.querySelector('.alert span')?.classList.contains('break-words')).toBe(true);
  });
});

describe('toast', () => {
  it('returns the id so a toast can be dismissed by code', () => {
    const id = toast.info('working', { timeout: 0 });

    expect(toasts.value.map((item) => item.id)).toEqual([id]);
    toast.dismiss(id);
    expect(toasts.value).toEqual([]);
  });

  it('passes options through', () => {
    toast.warning('slow', { timeout: 1234 });

    expect(toasts.value[0]).toMatchObject({ kind: 'warning', message: 'slow', timeout: 1234 });
  });

  it('clears everything', () => {
    toast.info('a');
    toast.error('b');

    toast.clear();

    expect(toasts.value).toEqual([]);
  });
});
