import { expect, it, vi } from 'vitest';
import { mount } from '@/test/helpers';
import '@/components/opm-save-bar';

it('renders the shared sticky status and Save/Discard events', async () => {
  const el = await mount('opm-save-bar');
  expect(el.querySelector('.navbar.sticky')).not.toBeNull();
  expect(el.querySelector('[role="status"]')?.textContent).toContain('Unsaved changes');
  const save = vi.fn();
  const discard = vi.fn();
  el.addEventListener('save', save);
  el.addEventListener('discard', discard);
  el.querySelector<HTMLButtonElement>('.btn-primary')?.click();
  el.querySelector<HTMLButtonElement>('.btn-ghost')?.click();
  expect(save).toHaveBeenCalledOnce();
  expect(discard).toHaveBeenCalledOnce();
});

it('disables Save for invalid values and both buttons while saving', async () => {
  const el = await mount('opm-save-bar', { invalid: true });
  expect(el.querySelector<HTMLButtonElement>('.btn-primary')?.disabled).toBe(true);
  expect(el.querySelector<HTMLButtonElement>('.btn-ghost')?.disabled).toBe(false);
  expect(el.textContent).toContain('Some values are not valid');
  el.invalid = false;
  el.message = 'Mapping changes';
  el.saving = true;
  await el.updateComplete;
  expect([...el.querySelectorAll('button')].every((button) => button.disabled)).toBe(true);
  expect(el.querySelector('.loading-spinner')).not.toBeNull();
  expect(el.textContent).toContain('Mapping changes');
});
