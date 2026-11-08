import { afterEach, expect, it, vi } from 'vitest';

afterEach(() => {
  localStorage.clear();
  vi.resetModules();
});

it.each([
  ['rename.policy', 'replace'],
  ['rename.policy', 'unknown'],
  ['rename.matchMode', 'javascript'],
  ['rename.clean', 'true'],
  ['rename.countDigits', 1.5],
  ['rename.countDigits', -1],
  ['rename.countDigits', 10],
  ['rename.countDigits', '3'],
])('rejects invalid persisted %s=%s without enabling unsafe rules', async (key, value) => {
  vi.resetModules();
  localStorage.setItem(`apm.v1.${key}`, JSON.stringify(value));
  const state = await import('./rename-state');
  expect(state.policy.value).toBe('smart');
  expect(state.matchMode.value).toBe('all');
  expect(state.clean.value).toBe(false);
  expect(state.countDigits.value).toBe(0);
  expect(state.validation.value.status).toBe('idle');
});

it('loads valid rename preferences independently of move and format preferences', async () => {
  vi.resetModules();
  localStorage.setItem('apm.v1.rename.countDigits', '9');
  localStorage.setItem('apm.v1.rename.policy', '"skip"');
  localStorage.setItem('apm.v1.rename.pattern', '"{name}_{count}"');
  localStorage.setItem('apm.v1.move.pattern', '"untouched-move"');
  localStorage.setItem('apm.v1.format.pattern', '"untouched-format"');
  const state = await import('./rename-state');
  expect(state.currentRules()).toMatchObject({ count_digits: 9, pattern: '{name}_{count}' });
  expect(state.policy.value).toBe('skip');
  state.pattern.value = '{YYYY}_{count}';
  expect(localStorage.getItem('apm.v1.move.pattern')).toBe('"untouched-move"');
  expect(localStorage.getItem('apm.v1.format.pattern')).toBe('"untouched-format"');
});
