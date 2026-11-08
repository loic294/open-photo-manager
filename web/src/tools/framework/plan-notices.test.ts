import { nothing, render } from 'lit';
import { expect, it } from 'vitest';
import { FakePlan, toolConfig } from '@/test/tool-config';
import { renderPendingNotice } from '@/tools/framework/plan-notices';
import { createToolContext } from '@/tools/framework/tool-context';

it('uses the caller code, text and title and retries from Check again', () => {
  const ctx = createToolContext(toolConfig(), document.createElement('div'));
  const plan = new FakePlan();
  Object.defineProperty(ctx, 'plan', { value: plan });
  const notice = {
    code: 'metadata_pending',
    text: (count: number) => `${count} still reading`,
    title: 'Caller tooltip',
  };
  expect(renderPendingNotice(ctx, notice)).toBe(nothing);
  plan.warningsSignal.value = [
    { code: 'unhashed_files', count: 99, message: 'Other warning' },
    { code: 'metadata_pending', count: 17, message: 'Pending' },
  ];
  const container = document.createElement('div');
  render(renderPendingNotice(ctx, notice), container);
  expect(container.querySelector('.badge-warning')?.textContent).toBe('17 still reading');
  expect(container.querySelector('[title]')?.getAttribute('title')).toBe('Caller tooltip');
  const button = container.querySelector('button');
  expect(button?.textContent).toContain('Check again');
  button?.click();
  expect(plan.retry).toHaveBeenCalledOnce();
});
