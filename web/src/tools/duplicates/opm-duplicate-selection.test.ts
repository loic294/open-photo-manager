import { afterEach, expect, it, vi } from 'vitest';
import { mount } from '@/test/helpers';
import './opm-duplicate-selection';

afterEach(() => vi.restoreAllMocks());

async function settle(el: Element) {
  for (let i = 0; i < 6; i++) {
    await Promise.resolve();
    await Promise.all(
      [el, ...el.querySelectorAll('*')].map((child) =>
        'updateComplete' in child ? child.updateComplete : undefined,
      ),
    );
  }
}

it('edits a draft, reorders by keyboard buttons and drag, and applies enabled rules only on confirmation', async () => {
  const el = await mount('opm-duplicate-selection', { open: true, tags: ['trip'] });
  const applied = vi.fn<(event: Event) => void>();
  el.addEventListener('selection-rules', applied);
  await settle(el);
  el.querySelector<HTMLButtonElement>('[aria-label="Move Modified date up"]')?.click();
  await settle(el);
  expect(el.querySelector('li')?.textContent).toContain('Modified date');
  const rows = [...el.querySelectorAll('li')];
  rows[2]?.dispatchEvent(new Event('dragstart', { bubbles: true }));
  rows[0]?.dispatchEvent(new Event('drop', { bubbles: true, cancelable: true }));
  await settle(el);
  expect(el.querySelector('li')?.textContent).toContain('Creation date');
  const enabled = el.querySelector<HTMLInputElement>('li input');
  if (!enabled) throw new Error('Missing rule toggle');
  enabled.checked = true;
  enabled.dispatchEvent(new Event('change'));
  await settle(el);
  expect(applied).not.toHaveBeenCalled();
  expect(el.rules.every((rule) => !rule.enabled)).toBe(true);
  [...el.querySelectorAll('button')]
    .find((button) => button.textContent?.trim() === 'Apply to all groups')
    ?.click();
  const event = applied.mock.calls[0]?.[0];
  expect(event instanceof CustomEvent && event.detail[0]).toMatchObject({
    kind: 'created',
    enabled: true,
  });
});

it('requires a value for an enabled specific-tag rule and discards cancelled edits', async () => {
  const el = await mount('opm-duplicate-selection', { open: true, tags: ['trip'] });
  await settle(el);
  const row = [...el.querySelectorAll('li')].find((entry) =>
    entry.textContent?.includes('Has specific tag'),
  );
  const checkbox = row?.querySelector<HTMLInputElement>('input');
  if (!checkbox) throw new Error('Missing tag rule');
  checkbox.checked = true;
  checkbox.dispatchEvent(new Event('change'));
  await settle(el);
  const apply = [...el.querySelectorAll('button')].find(
    (button) => button.textContent?.trim() === 'Apply to all groups',
  );
  expect(apply?.disabled).toBe(true);
  const select = el.querySelector<HTMLSelectElement>('[aria-label="Preferred tag"]');
  if (!select) throw new Error('Missing tag selector');
  select.value = 'trip';
  select.dispatchEvent(new Event('change'));
  await settle(el);
  expect(apply?.disabled).toBe(false);
  el.open = false;
  await settle(el);
  el.open = true;
  await settle(el);
  expect(el.querySelector<HTMLInputElement>('[aria-label="Preferred tag"]')?.value).toBe('');
});
