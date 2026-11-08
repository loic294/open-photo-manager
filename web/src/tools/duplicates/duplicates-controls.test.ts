import { render } from 'lit';
import { afterEach, expect, it } from 'vitest';
import { FakePlan, toolConfig } from '@/test/tool-config';
import { duplicateControls } from '@/tools/duplicates/duplicates-controls';
import { matchMode, strictPrefix } from '@/tools/duplicates/duplicates-state';
import { createToolContext } from '@/tools/framework/tool-context';
import { formatCount } from '@/util/format-count';

afterEach(() => {
  document.body.replaceChildren();
  matchMode.value = 'hash';
  strictPrefix.value = false;
});

function setup() {
  const container = document.createElement('div');
  document.body.append(container);
  const ctx = createToolContext(toolConfig(), container);
  const plan = new FakePlan();
  Object.defineProperty(ctx, 'plan', { value: plan });
  const update = () => render(duplicateControls(ctx), container);
  update();
  return { container, ctx, plan, update };
}

it('writes valid match-change details and ignores malformed changes', () => {
  const { container } = setup();
  const control = container.querySelector('apm-match-mode');
  control?.dispatchEvent(
    new CustomEvent('match-change', {
      detail: { mode: 'name_prefix', strict: true },
      bubbles: true,
    }),
  );
  expect(matchMode.value).toBe('name_prefix');
  expect(strictPrefix.value).toBe(true);
  for (const detail of [
    null,
    { mode: 'unknown', strict: false },
    { mode: 'hash', strict: 'yes' },
  ]) {
    control?.dispatchEvent(new CustomEvent('match-change', { detail, bubbles: true }));
    expect(matchMode.value).toBe('name_prefix');
    expect(strictPrefix.value).toBe(true);
  }
});

it('only summarizes ready stats, with sizes in the tooltip and a distinct empty state', () => {
  const { container, plan, update } = setup();
  expect(container.querySelector('[role="status"]')).toBeNull();
  plan.statusSignal.value = 'ready';
  plan.statsSignal.value = {
    comparison_matched: 2988,
    source_matched: 1,
    comparison_matched_bytes: 1024,
    source_matched_bytes: 42,
  };
  update();
  expect(container.querySelector('[role="status"]')?.textContent).toBe(
    `${formatCount(2988)} comparison and 1 source files have a match`,
  );
  expect(container.querySelector('[role="status"]')?.getAttribute('title')).toContain('1 KB');
  plan.statsSignal.value = { source_matched: 0, comparison_matched: 0 };
  update();
  expect(container.textContent).toContain('No duplicates found');
  plan.statusSignal.value = 'building';
  update();
  expect(container.querySelector('[role="status"]')).toBeNull();
});

it('always exposes unhashed counts and background explanation, and retries', () => {
  const { container, plan, update } = setup();
  plan.warningsSignal.value = [{ code: 'unhashed_files', message: 'Pending', count: 1234 }];
  update();
  expect(container.querySelector('.badge-warning')?.textContent).toBe(
    `${formatCount(1234)} files not hashed yet`,
  );
  expect(container.querySelector('[title]')?.getAttribute('title')).toContain(
    'matches update by themselves',
  );
  container.querySelector('button')?.click();
  expect(plan.retry).toHaveBeenCalledOnce();
  plan.warningsSignal.value = [];
  update();
  expect(container.querySelector('.badge-warning')).toBeNull();
});
