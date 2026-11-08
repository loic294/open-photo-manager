import { render } from 'lit';
import { afterEach, expect, it, vi } from 'vitest';
import { FakePlan, plainPane, toolConfig } from '@/test/tool-config';
import { actionReason, renderActions } from '@/tools/framework/tool-actions';
import { createToolContext } from '@/tools/framework/tool-context';

vi.mock('@/tools/framework/plan-binding', async (original) => {
  const actual = await original<typeof import('@/tools/framework/plan-binding')>();
  return { ...actual, createPlanBinding: () => binding };
});
const binding = new FakePlan();
afterEach(() => {
  document.querySelector('#actions')?.remove();
  vi.restoreAllMocks();
});

it.each([
  ['none', 'Choose a base'],
  ['idle', 'Choose a base'],
  ['building', 'Matching...'],
  ['executing', 'Working...'],
  ['error', 'Narrow the filter'],
] as const)(
  'plan actions in %s are disabled before their own reason with the status tooltip',
  (state, reason) => {
    const disabled = vi.fn(() => 'Read-only volume');
    const action = { id: 'run', label: 'Run', disabled, run: vi.fn(async () => {}) };
    binding.statusSignal.value = state;
    binding.errorSignal.value = 'Narrow the filter';
    const ctx = createToolContext(
      toolConfig({
        plan: { kind: 'cleanup', params: () => ({ ready: false, reason: 'Choose a base' }) },
        panes: {
          source: { ...plainPane('Source'), planSide: 'source' },
          comparison: plainPane('Comparison'),
        },
        actions: () => ({ defaultId: 'run', options: [action] }),
      }),
      document.createElement('div'),
    );
    const container = document.createElement('div');
    container.id = 'actions';
    document.body.append(container);
    render(renderActions(ctx), container);
    expect(container.querySelector('button')?.disabled).toBe(true);
    expect(container.querySelector('[data-tip]')?.getAttribute('data-tip')).toBe(reason);
    expect(actionReason(ctx, action)).toBe(reason);
    expect(disabled).not.toHaveBeenCalled();
  },
);

it('a ready plan uses only the action reason, while no-plan tools stay unchanged', () => {
  const action = {
    id: 'run',
    label: 'Run',
    disabled: vi.fn(() => null),
    run: vi.fn(async () => {}),
  };
  binding.statusSignal.value = 'ready';
  const ctx = createToolContext(
    toolConfig({
      plan: { kind: 'cleanup', params: () => ({ ready: true, params: {} }) },
      panes: {
        source: { ...plainPane('Source'), planSide: 'source' },
        comparison: plainPane('Comparison'),
      },
    }),
    document.createElement('div'),
  );
  expect(actionReason(ctx, action)).toBeNull();
  const noPlan = createToolContext(toolConfig(), document.createElement('div'));
  binding.statusSignal.value = 'building';
  expect(actionReason(noPlan, action)).toBeNull();
});
