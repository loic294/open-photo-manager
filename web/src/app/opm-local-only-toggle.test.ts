import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import '@/app/opm-local-only-toggle';
import { STORAGE_PREFIX } from '@/stores/persisted';
import { localOnlyChanges } from '@/stores/prefs';
import { mount } from '@/test/helpers';

beforeEach(() => {
  localOnlyChanges.value = false;
  localStorage.clear();
});

afterEach(() => {
  localOnlyChanges.value = false;
  localStorage.clear();
});

describe('opm-local-only-toggle', () => {
  it('names the operation mode and shows its current state', async () => {
    const host = await mount('opm-local-only-toggle');
    const button = host.querySelector('button');

    expect(button?.textContent).toContain('Local only changes');
    expect(button?.getAttribute('aria-pressed')).toBe('false');
    expect(button?.getAttribute('aria-label')).toBe('Local only changes: Off');
    expect(host.querySelector('opm-icon')?.getAttribute('name')).toBe('cloud-off');
  });

  it('toggles, persists and reacts to later changes', async () => {
    const host = await mount('opm-local-only-toggle');
    const button = host.querySelector('button');
    button?.click();
    await host.updateComplete;

    expect(localOnlyChanges.value).toBe(true);
    expect(button?.getAttribute('aria-pressed')).toBe('true');
    expect(button?.classList.contains('btn-warning')).toBe(true);
    expect(localStorage.getItem(`${STORAGE_PREFIX}localOnlyChanges`)).toBe('true');

    localOnlyChanges.value = false;
    await host.updateComplete;
    expect(button?.getAttribute('aria-pressed')).toBe('false');
  });
});
