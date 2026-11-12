import { effect } from '@preact/signals-core';
import { render } from 'lit';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { setLocation, syncFromLocation } from '@/stores/nav';
import { FakePlan, plainPane, toolConfig } from '@/test/tool-config';
import type { PlanBinding } from '@/tools/framework/plan-binding';
import { renderPlanStatus } from '@/tools/framework/plan-status';
import type { PlanParamsResult } from '@/tools/framework/tool-config';
import { createToolContext, type ToolContext } from '@/tools/framework/tool-context';

const seam = vi.hoisted(() => ({ make: null as null | ((ctx: unknown) => unknown) }));

// The plan client is not part of these tests: a test hands the context its own binding.
vi.mock('@/tools/framework/plan-binding', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/tools/framework/plan-binding')>();
  return {
    ...actual,
    createPlanBinding: (ctx: ToolContext) =>
      (seam.make?.(ctx) as PlanBinding | undefined) ?? actual.createPlanBinding(ctx),
  };
});

const stops: (() => void)[] = [];
const containers: HTMLElement[] = [];

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  for (const target of containers.splice(0)) target.remove();
  seam.make = null;
  vi.restoreAllMocks();
  window.history.replaceState(null, '', '/');
  syncFromLocation();
});

/** A tool with a plan whose parameters are `params()`, over a plan the test can drive. */
function setup(
  params: (ctx: ToolContext) => PlanParamsResult = () => ({ ready: true, params: {} }),
) {
  const plan = new FakePlan();
  seam.make = () => plan;
  const ctx = createToolContext(
    toolConfig({
      id: 'duplicates',
      plan: { kind: 'duplicates', params },
      panes: {
        source: { ...plainPane('Source'), planSide: 'source' },
        comparison: { ...plainPane('Comparison'), planSide: 'comparison' },
      },
    }),
    document.createElement('div'),
  );
  return { plan, ctx };
}

/** Renders the status area now and again whenever a signal it read changes. */
function show(ctx: ToolContext): HTMLElement {
  const target = document.createElement('div');
  document.body.append(target);
  containers.push(target);
  stops.push(
    effect(() => {
      render(renderPlanStatus(ctx), target);
    }),
  );
  return target;
}

function part<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`No element matches "${selector}"`);
  return found;
}

const text = (element: Element | null | undefined) =>
  element?.textContent?.replace(/\s+/g, ' ').trim() ?? '';

const status = (root: ParentNode) => part(root, '[role="status"]');

describe('the live region', () => {
  it('is always there, empty when there is nothing to say', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    expect(text(status(area))).toBe('');
    for (const next of ['idle', 'building', 'ready', 'executing', 'error'] as const) {
      plan.statusSignal.value = next;
      expect(area.querySelectorAll('[role="status"]')).toHaveLength(1);
    }
  });

  it('is there for a tool without a plan too, and stays empty', () => {
    const ctx = createToolContext(toolConfig(), document.createElement('div'));
    const area = show(ctx);
    expect(text(status(area))).toBe('');
    expect(area.querySelector('[role="alert"]')).toBeNull();
  });
});

describe('building and executing', () => {
  it('shows a spinner and "Matching..." while the server matches', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.statusSignal.value = 'building';
    expect(text(status(area))).toBe('Matching...');
    expect(status(area).querySelector('.loading')).not.toBeNull();
  });

  it('shows "Working..." while a run is in progress', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.statusSignal.value = 'executing';
    expect(text(status(area))).toBe('Working...');
    expect(status(area).querySelector('.loading')).not.toBeNull();
  });

  it('prefers the progress to the reason the plan was not ready', () => {
    const { ctx, plan } = setup(() => ({ ready: false, reason: 'Choose a base folder' }));
    const area = show(ctx);
    expect(text(status(area))).toBe('Choose a base folder');
    plan.statusSignal.value = 'building';
    expect(text(status(area))).toBe('Matching...');
  });

  it('goes quiet when the plan is ready', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.statusSignal.value = 'building';
    plan.statusSignal.value = 'ready';
    expect(text(status(area))).toBe('');
    expect(status(area).querySelector('.loading')).toBeNull();
  });
});

describe('nothing to match yet', () => {
  it('shows why as quiet text, not as an alert', () => {
    const { ctx } = setup(() => ({ ready: false, reason: 'Choose a base folder in each pane' }));
    const area = show(ctx);
    expect(text(status(area))).toBe('Choose a base folder in each pane');
    expect(area.querySelector('[role="alert"]')).toBeNull();
    expect(area.querySelector('.alert')).toBeNull();
  });

  it('shows nothing when the plan gives no reason', () => {
    const { ctx } = setup(() => ({ ready: false, reason: null }));
    expect(text(status(show(ctx)))).toBe('');
  });

  it('shows nothing when the parameters are ready', () => {
    const { ctx } = setup(() => ({ ready: true, params: {} }));
    expect(text(status(show(ctx)))).toBe('');
  });

  it('follows the signals the parameters read', () => {
    const { ctx } = setup((c) =>
      c.location('source').dir === ''
        ? { ready: false, reason: 'Open a folder' }
        : { ready: true, params: {} },
    );
    const area = show(ctx);
    expect(text(status(area))).toBe('Open a folder');
    setLocation('source', { volume: 'photos', dir: 'trip' });
    expect(text(status(area))).toBe('');
  });

  it('is not shown after the first plan, whatever the parameters say', () => {
    const { ctx, plan } = setup(() => ({ ready: false, reason: 'Choose a base folder' }));
    const area = show(ctx);
    plan.statusSignal.value = 'ready';
    expect(text(status(area))).toBe('');
    plan.statusSignal.value = 'idle';
    expect(text(status(area))).toBe('Choose a base folder');
  });
});

describe('an error', () => {
  it('shows the message in an alert with a Retry button', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.errorSignal.value = 'The index is being rebuilt';
    plan.statusSignal.value = 'error';
    const alert = part(area, '[role="alert"]');
    expect(alert.classList.contains('alert-error')).toBe(true);
    expect(text(alert)).toBe('The index is being rebuilt Retry');
    part<HTMLButtonElement>(alert, 'button').click();
    expect(plan.retry).toHaveBeenCalledTimes(1);
  });

  it('says something even when the plan client gave no message', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.statusSignal.value = 'error';
    expect(text(part(area, '[role="alert"]'))).toContain('The plan could not be built.');
  });

  it('goes away with the error', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.statusSignal.value = 'error';
    plan.statusSignal.value = 'building';
    expect(area.querySelector('[role="alert"]')).toBeNull();
  });

  it('shows no alert for any other status', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    for (const next of ['idle', 'building', 'ready', 'executing'] as const) {
      plan.statusSignal.value = next;
      expect(area.querySelector('[role="alert"]')).toBeNull();
    }
  });
});

describe('warnings', () => {
  const WARNINGS = [
    { code: 'unreadable', message: 'Files that could not be read', count: 3 },
    { code: 'offline', message: 'A volume is offline', count: 1200 },
  ];

  it('shows nothing without warnings', () => {
    const { ctx } = setup();
    const area = show(ctx);
    expect(area.querySelector('.dropdown')).toBeNull();
  });

  it('counts the warnings, not the items they are about', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.warningsSignal.value = [WARNINGS[0] as (typeof WARNINGS)[number]];
    expect(text(part(area, '[role="button"]'))).toBe('1 warning');
    plan.warningsSignal.value = WARNINGS;
    expect(text(part(area, '[role="button"]'))).toBe('2 warnings');
  });

  it('keeps warning text readable and the warning icon coloured', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.warningsSignal.value = [WARNINGS[0] as (typeof WARNINGS)[number]];
    const trigger = part(area, '[role="button"]');
    expect(trigger.classList.contains('text-base-content')).toBe(true);
    expect(trigger.classList.contains('text-warning')).toBe(false);
    expect(part(trigger, 'opm-icon').classList.contains('text-warning')).toBe(true);
  });

  it('lists each message with its count', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.warningsSignal.value = WARNINGS;
    const rows = [...area.querySelectorAll('ul li')].map(text);
    expect(rows).toEqual(['3 Files that could not be read', '1200 A volume is offline']);
  });

  it('is a dropdown that opens on focus, so the bar does not have to clip it', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.warningsSignal.value = WARNINGS;
    const dropdown = part(area, '.dropdown');
    expect(dropdown.classList.contains('dropdown-end')).toBe(true);
    expect(part(dropdown, '[role="button"]').getAttribute('tabindex')).toBe('0');
    expect(part(dropdown, 'ul').classList.contains('dropdown-content')).toBe(true);
  });

  it('closes with Escape: the focus leaves the trigger and the page does not hear it', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.warningsSignal.value = WARNINGS;
    const trigger = part(area, '[role="button"]');
    const page = vi.fn();
    document.body.addEventListener('keydown', page);
    stops.push(() => document.body.removeEventListener('keydown', page));
    trigger.focus();
    expect(document.activeElement).toBe(trigger);
    const key = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    trigger.dispatchEvent(key);
    expect(document.activeElement).not.toBe(trigger);
    expect(key.defaultPrevented).toBe(true);
    expect(page).not.toHaveBeenCalled();
  });

  it('leaves Escape alone when the focus is not inside the dropdown', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.warningsSignal.value = WARNINGS;
    const outside = document.createElement('button');
    document.body.append(outside);
    stops.push(() => outside.remove());
    outside.focus();
    const page = vi.fn();
    document.body.addEventListener('keydown', page);
    stops.push(() => document.body.removeEventListener('keydown', page));
    const key = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    part(area, '[role="button"]').dispatchEvent(key);
    expect(document.activeElement).toBe(outside);
    expect(key.defaultPrevented).toBe(false);
    expect(page).toHaveBeenCalledTimes(1);
  });

  it('ignores every other key', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.warningsSignal.value = WARNINGS;
    const trigger = part(area, '[role="button"]');
    trigger.focus();
    const key = new KeyboardEvent('keydown', { key: 'a', bubbles: true, cancelable: true });
    trigger.dispatchEvent(key);
    expect(document.activeElement).toBe(trigger);
    expect(key.defaultPrevented).toBe(false);
  });

  it('is shown next to the progress and the error', () => {
    const { ctx, plan } = setup();
    const area = show(ctx);
    plan.warningsSignal.value = WARNINGS;
    plan.statusSignal.value = 'error';
    expect(area.querySelector('[role="alert"]')).not.toBeNull();
    expect(area.querySelector('.dropdown')).not.toBeNull();
  });
});
