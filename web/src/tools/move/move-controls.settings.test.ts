import type { signal } from '@preact/signals-core';
import { render } from 'lit';
import { afterEach, expect, it, vi } from 'vitest';
import type { SettingsDto } from '@/api/generated/SettingsDto';
import { toast } from '@/components/toast';
import { loadSettings, settings } from '@/stores/settings';
import { sampleSettings } from '@/test/settings-fixtures';
import { ensureSettings, renderConflictPolicy } from './move-controls';
import { createRuleState } from './move-state';

vi.mock('@/stores/settings', async () => {
  const { signal } = await import('@preact/signals-core');
  return { settings: signal<SettingsDto | null>(null), loadSettings: vi.fn() };
});
afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.mocked(loadSettings).mockReset();
  (settings as ReturnType<typeof signal<SettingsDto | null>>).value = null;
});

it('keeps Default usable before settings load and shows the shared policy label afterwards', () => {
  const rules = createRuleState('policy-controls');
  const container = document.createElement('div');
  document.body.append(container);
  render(renderConflictPolicy(rules), container);
  expect(container.querySelector('opm-select')?.options[0]?.label).toBe('Default');
  (settings as ReturnType<typeof signal<SettingsDto | null>>).value = sampleSettings({
    default_conflict_policy: 'replace',
  });
  render(renderConflictPolicy(rules), container);
  expect(container.querySelector('opm-select')?.options[0]?.label).toBe('Default (Replace)');
  container
    .querySelector('opm-select')
    ?.dispatchEvent(new CustomEvent('select-change', { detail: { value: 'rename' } }));
  expect(rules.policy.value).toBe('rename');
});

it('reports unexpected load errors rather than silently using a browser default', async () => {
  const error = vi.spyOn(toast, 'error').mockReturnValue(1);
  vi.mocked(loadSettings).mockRejectedValue(new Error('server unavailable'));
  ensureSettings();
  await Promise.resolve();
  expect(error).toHaveBeenCalledWith('server unavailable');
  (settings as ReturnType<typeof signal<SettingsDto | null>>).value = sampleSettings();
  ensureSettings();
  expect(loadSettings).toHaveBeenCalledOnce();
});
