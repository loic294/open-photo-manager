import { expect, it, vi } from 'vitest';
import '@/components/opm-match-mode';
import type { MatchMode } from '@/api/generated/MatchMode';
import { mount } from '@/test/helpers';
import { MATCH_MODES } from '@/util/match-modes';

it('renders only the shared options with their titles and a visible label', async () => {
  const el = await mount('opm-match-mode');
  await el.querySelector('opm-select')?.updateComplete;
  expect(
    [...el.querySelectorAll('option')].map((option) => ({
      value: option.value,
      label: option.textContent,
    })),
  ).toEqual(MATCH_MODES.map(({ value, label }) => ({ value, label })));
  expect(el.querySelector('select')?.getAttribute('data-tooltip')).toBe(MATCH_MODES[0]?.title);
  expect(el.textContent).toContain('Match by');
  expect(el.querySelector('select')?.getAttribute('aria-label')).toBe('Match by');
});

it.each(MATCH_MODES)('a mode edit to $value preserves strict', async ({ value }) => {
  const el = await mount('opm-match-mode', { strict: true });
  const change = vi.fn();
  el.addEventListener('match-change', change);
  el.querySelector('opm-select')?.dispatchEvent(
    new CustomEvent('select-change', { detail: { value }, bubbles: true }),
  );
  expect(change).toHaveBeenCalledOnce();
  expect(change.mock.calls[0]?.[0].detail).toEqual({ mode: value, strict: true });
  expect(change.mock.calls[0]?.[0].bubbles).toBe(true);
  expect(el.mode).toBe('hash');
});

it('only prefix offers the toggle, with its label and explanation; editing keeps the mode', async () => {
  const el = await mount('opm-match-mode');
  expect(el.querySelector('input')).toBeNull();
  el.mode = 'name_prefix';
  await el.updateComplete;
  const toggle = el.querySelector('input');
  expect(toggle?.closest('label')?.textContent).toContain('Strict prefix');
  expect(toggle?.closest('label')?.getAttribute('data-tooltip')).toContain('IMG_00012');
  const change = vi.fn();
  el.addEventListener('match-change', change);
  if (!toggle) throw new Error('Missing toggle');
  toggle.checked = true;
  toggle.dispatchEvent(new Event('change', { bubbles: true }));
  expect(change.mock.calls[0]?.[0].detail).toEqual({ mode: 'name_prefix', strict: true });
  await el.updateComplete;
  expect(toggle.checked).toBe(false);
});

it('disables both controls and emits nothing, including forged/unknown select events', async () => {
  const el = await mount('opm-match-mode', { mode: 'name_prefix', disabled: true });
  await el.querySelector('opm-select')?.updateComplete;
  expect(el.querySelector('select')?.disabled).toBe(true);
  expect(el.querySelector('input')?.disabled).toBe(true);
  const change = vi.fn();
  el.addEventListener('match-change', change);
  el.querySelector('opm-select')?.dispatchEvent(
    new CustomEvent('select-change', { detail: { value: 'hash' }, bubbles: true }),
  );
  el.querySelector('input')?.dispatchEvent(new Event('change', { bubbles: true }));
  expect(change).not.toHaveBeenCalled();
  el.disabled = false;
  el.mode = 'unknown' as MatchMode;
  await el.updateComplete;
  await el.querySelector('opm-select')?.updateComplete;
  expect(el.querySelector('select')?.value).toBe('');
  el.querySelector('opm-select')?.dispatchEvent(
    new CustomEvent('select-change', { detail: { value: 'unknown' }, bubbles: true }),
  );
  expect(change).not.toHaveBeenCalled();
});
