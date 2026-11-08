import { afterEach, expect, it, vi } from 'vitest';

let state: typeof import('@/tools/duplicates/duplicates-state') | undefined;
afterEach(() => {
  state?.matchMode.dispose();
  state?.strictPrefix.dispose();
  localStorage.clear();
  vi.resetModules();
});

it('defaults to hash and false, then persists both under their specified keys', async () => {
  state = await import('@/tools/duplicates/duplicates-state');
  expect(state.matchMode.value).toBe('hash');
  expect(state.strictPrefix.value).toBe(false);
  state.matchMode.value = 'name_prefix';
  state.strictPrefix.value = true;
  expect(localStorage.getItem('apm.v1.duplicates.mode')).toBe('"name_prefix"');
  expect(localStorage.getItem('apm.v1.duplicates.strict')).toBe('true');
});

it('restores valid values', async () => {
  localStorage.setItem('apm.v1.duplicates.mode', '"name_different_time"');
  localStorage.setItem('apm.v1.duplicates.strict', 'true');
  state = await import('@/tools/duplicates/duplicates-state');
  expect(state.matchMode.value).toBe('name_different_time');
  expect(state.strictPrefix.value).toBe(true);
});

it.each(['"nope"', '"toString"', '1', '{broken'])(
  'falls back from invalid mode %s',
  async (value) => {
    localStorage.setItem('apm.v1.duplicates.mode', value);
    localStorage.setItem('apm.v1.duplicates.strict', '"yes"');
    state = await import('@/tools/duplicates/duplicates-state');
    expect(state.matchMode.value).toBe('hash');
    expect(state.strictPrefix.value).toBe(false);
  },
);
