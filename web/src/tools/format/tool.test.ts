import { afterEach, expect, it, vi } from 'vitest';
import { locations, setLocation } from '@/stores/nav';
import { resetToolStateForTests } from '@/stores/tool-state';
import { NO_PLAN } from '@/tools/framework/plan-binding';
import { checkToolConfig } from '@/tools/framework/tool-config';
import { createToolContext } from '@/tools/framework/tool-context';
import { moveRules } from '@/tools/move/move-state';
import { formatRules } from './format-state';
import { formatTool } from './tool';

vi.mock('@/tools/framework/plan-binding', async (original) => ({
  ...(await original<typeof import('@/tools/framework/plan-binding')>()),
  createPlanBinding: () => NO_PLAN,
}));
vi.mock('@/tools/move/move-controls', async (original) => ({
  ...(await original<typeof import('@/tools/move/move-controls')>()),
  ensureSettings: () => undefined,
}));
afterEach(() => {
  resetToolStateForTests();
  formatRules.pattern.value = '{YYYY}/{mm}';
  moveRules.pattern.value = '{YYYY}/{mm}';
});

it('is a valid thin custom-move config with independent pattern state', () => {
  expect(checkToolConfig(formatTool)).toBeNull();
  expect(formatTool.panes.source.baseMode).toBe('current');
  expect(formatTool.panes.comparison.defaultBase).toEqual({
    from: 'source',
    label: 'Same as the source folder',
  });
  formatRules.pattern.value = '{camera}';
  expect(moveRules.pattern.value).toBe('{YYYY}/{mm}');
});

it('follows the source only while the default is active, resumes after reset, and cleans up', async () => {
  setLocation('source', { volume: 'photos', dir: 'first' });
  const ctx = createToolContext(formatTool, document.createElement('div'));
  const stop = formatTool.setup?.(ctx);
  expect(locations.comparison.value).toEqual(locations.source.value);
  ctx.setBase('comparison', { volume: 'backup', dir: 'pin' });
  setLocation('source', { volume: 'photos', dir: 'second' });
  await Promise.resolve();
  expect(locations.comparison.value.dir).toBe('first');
  ctx.setBase('comparison', null);
  setLocation('source', { volume: 'photos', dir: 'third' });
  await Promise.resolve();
  expect(locations.comparison.value.dir).toBe('third');
  stop?.();
  setLocation('source', { volume: 'photos', dir: 'fourth' });
  await Promise.resolve();
  expect(locations.comparison.value.dir).toBe('third');
});
