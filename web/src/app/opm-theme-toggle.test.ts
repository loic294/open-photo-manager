import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import '@/app/opm-theme-toggle';
import { STORAGE_PREFIX } from '@/stores/persisted';
import { theme } from '@/stores/prefs';
import { mount } from '@/test/helpers';

function input(host: HTMLElement): HTMLInputElement {
  const element = host.querySelector('input');
  if (!element) throw new Error('no checkbox rendered');
  return element;
}

async function click(host: HTMLElement & { updateComplete: Promise<unknown> }) {
  input(host).click();
  await host.updateComplete;
}

function stored(): string | null {
  return localStorage.getItem(`${STORAGE_PREFIX}theme`);
}

beforeEach(() => {
  theme.value = 'system';
  localStorage.clear();
});

afterEach(() => {
  theme.value = 'system';
});

describe('opm-theme-toggle', () => {
  it('is a DaisyUI swap with a tooltip, holding the three icons of the three themes', async () => {
    const host = await mount('opm-theme-toggle');

    expect(host.querySelector('.tooltip.tooltip-left')).not.toBeNull();
    expect(host.querySelector('label.swap.swap-rotate')).not.toBeNull();
    const icon = (state: string) => host.querySelector(`.${state}`)?.getAttribute('name');
    expect(icon('swap-off')).toBe('sun');
    expect(icon('swap-on')).toBe('moon');
    expect(icon('swap-indeterminate')).toBe('monitor');
  });

  it('shows the system theme as an indeterminate checkbox', async () => {
    const host = await mount('opm-theme-toggle');

    expect(input(host).indeterminate).toBe(true);
    expect(input(host).checked).toBe(false);
    expect(host.querySelector('.tooltip')?.getAttribute('data-tip')).toBe('Theme: System');
  });

  it('shows light as unchecked and dark as checked', async () => {
    theme.value = 'light';
    const host = await mount('opm-theme-toggle');
    expect(input(host).indeterminate).toBe(false);
    expect(input(host).checked).toBe(false);

    theme.value = 'dark';
    await host.updateComplete;
    expect(input(host).indeterminate).toBe(false);
    expect(input(host).checked).toBe(true);
  });

  it('cycles system, light, dark, system on every click and persists each choice', async () => {
    const host = await mount('opm-theme-toggle');

    await click(host);
    expect(theme.value).toBe('light');
    expect(stored()).toBe('"light"');
    expect(input(host).checked).toBe(false);
    expect(input(host).indeterminate).toBe(false);

    await click(host);
    expect(theme.value).toBe('dark');
    expect(stored()).toBe('"dark"');
    expect(input(host).checked).toBe(true);
    expect(input(host).indeterminate).toBe(false);

    await click(host);
    expect(theme.value).toBe('system');
    expect(stored()).toBe('"system"');
    expect(input(host).checked).toBe(false);
    expect(input(host).indeterminate).toBe(true);
  });

  it('tells assistive technology the current theme and what a click does', async () => {
    const host = await mount('opm-theme-toggle');
    expect(input(host).getAttribute('aria-label')).toBe('Theme: System. Switch to Light.');

    await click(host);
    expect(input(host).getAttribute('aria-label')).toBe('Theme: Light. Switch to Dark.');

    await click(host);
    expect(input(host).getAttribute('aria-label')).toBe('Theme: Dark. Switch to System.');
    expect(host.querySelector('.tooltip')?.getAttribute('data-tip')).toBe('Theme: Dark');
  });

  it('follows a change made elsewhere, such as another tab', async () => {
    const host = await mount('opm-theme-toggle');

    theme.value = 'dark';
    await host.updateComplete;

    expect(input(host).checked).toBe(true);
    expect(host.querySelector('.tooltip')?.getAttribute('data-tip')).toBe('Theme: Dark');
  });

  it('restores the checkbox state when the browser toggled it on its own', async () => {
    theme.value = 'dark';
    const host = await mount('opm-theme-toggle');

    // The browser flips `checked` and clears `indeterminate` before the change handler runs.
    await click(host);

    expect(theme.value).toBe('system');
    expect(input(host).checked).toBe(false);
    expect(input(host).indeterminate).toBe(true);
  });
});
