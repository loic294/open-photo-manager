import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { events } from '@/api/sse';
import { confirmDialog } from '@/components/confirm-dialog';
import { setLocation, syncFromLocation } from '@/stores/nav';
import { resetPlanRunsForTests } from '@/stores/plan-runs';
import { resetSelectionForTests, selection } from '@/stores/selection';
import { resetToolStateForTests, setRecursive } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { mount } from '@/test/helpers';
import { makeTask } from '@/test/task-fixtures';
import { FakePlan } from '@/test/tool-config';
import { cleanupConfirmation, cleanupTool } from '@/tools/cleanup/tool';
import '@/tools/cleanup/opm-cleanup-page';
import { actionReason } from '@/tools/framework/tool-actions';
import type { ToolContext } from '@/tools/framework/tool-context';
import { createToolContext } from '@/tools/framework/tool-context';

vi.mock('@/components/confirm-dialog', () => ({ confirmDialog: vi.fn(async () => true) }));
vi.mock('@/components/opm-viewer', () => ({ openViewer: vi.fn() }));
vi.mock('@/tools/framework/plan-binding', async (original) => {
  const actual = await original<typeof import('@/tools/framework/plan-binding')>();
  return { ...actual, createPlanBinding: () => binding };
});

let binding: FakePlan & { executing: ReturnType<typeof vi.fn<(taskId: number) => void>> };
const stops: (() => void)[] = [];
let ctx: ToolContext;

async function settle(root: Element) {
  for (let i = 0; i < 6; i++) {
    await Promise.resolve();
    await Promise.all(
      [root, ...root.querySelectorAll('*')].map((element) =>
        'updateComplete' in element ? element.updateComplete : undefined,
      ),
    );
  }
}

beforeEach(async () => {
  binding = Object.assign(new FakePlan(), { executing: vi.fn<(taskId: number) => void>() });
  binding.statusSignal.value = 'ready';
  binding.planIdSignal.value = 3;
  binding.statsSignal.value = { empty_folders: 7, top_level_empty_folders: 2 };
  stubBrowseServer();
  await loadVolumes();
  window.history.replaceState(null, '', '/cleanup?source=photos%3Abase');
  syncFromLocation();
  setLocation('source', { volume: 'photos', dir: 'base' });
  ctx = createToolContext(cleanupTool, document.createElement('div'));
});

afterEach(() => {
  for (const page of document.querySelectorAll('opm-cleanup-page')) page.remove();
  for (const stop of stops.splice(0)) stop();
  events.stop();
  resetToolStateForTests();
  resetSelectionForTests();
  resetPlanRunsForTests();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.history.replaceState(null, '', '/');
  syncFromLocation();
});

describe('cleanup config', () => {
  it('takes the exact current source base and absolute scope, with the inverse toggle', () => {
    expect(cleanupTool.filter.applies).toBe('none');
    expect(cleanupTool.panes.source.baseMode).toBe('current');
    expect(cleanupTool.plan?.params(ctx)).toEqual({
      ready: true,
      params: { scope: { volume_id: 1, dir: 'base', recursive: true } },
    });
    setRecursive('cleanup', false);
    setLocation('source', { volume: 'photos', dir: 'base/été' });
    expect(cleanupTool.plan?.params(ctx)).toEqual({
      ready: true,
      params: { scope: { volume_id: 1, dir: 'base/été', recursive: false } },
    });
    setLocation('source', { volume: null, dir: '' });
    expect(cleanupTool.plan?.params(ctx).ready).toBe(false);
  });

  it('follows source moves but leaves comparison navigation independent in between', async () => {
    const stop = cleanupTool.setup?.(ctx);
    if (stop) stops.push(stop);
    expect(ctx.location('comparison')).toEqual(ctx.location('source'));
    setLocation('comparison', { volume: 'photos', dir: 'base/independent' });
    await Promise.resolve();
    expect(ctx.location('source').dir).toBe('base');
    expect(ctx.location('comparison').dir).toBe('base/independent');
    setLocation('source', { volume: 'photos', dir: 'new-base' });
    await Promise.resolve();
    expect(ctx.location('comparison').dir).toBe('new-base');
  });

  it('disables the sole action at zero and during building and names all confirmed folders', () => {
    const action = cleanupTool.actions?.(ctx)?.options[0];
    expect(action?.label).toBe('Delete empty folders (7)');
    expect(action && actionReason(ctx, action)).toBeNull();
    binding.statusSignal.value = 'building';
    expect(action && actionReason(ctx, action)).toBe('Matching...');
    binding.statusSignal.value = 'ready';
    binding.statsSignal.value = { empty_folders: 0 };
    expect(action && actionReason(ctx, action)).toBe('No empty folders');
    const confirmation = cleanupConfirmation(7, 2);
    expect(confirmation.title).toBe('Delete 7 empty folders?');
    expect(confirmation.message).toContain('2 top-level empty folders and 5 nested folders');
    expect(confirmation.message).toContain(
      'Leftover junk files (thumbnail caches, .DS_Store) go to the trash. Folders that contain anything else are left alone.',
    );
    expect(confirmation.message).toContain('base folder is kept');
  });

  it('uses the shared no-file-selection execution shape even with selected files', async () => {
    selection.source.set([11, 12]);
    selection.comparison.set([13]);
    const post = vi.spyOn(api, 'post').mockResolvedValue(makeTask({ id: 9 }));
    await cleanupTool.actions?.(ctx)?.options[0]?.run(ctx);
    expect(vi.mocked(confirmDialog)).toHaveBeenCalledWith(cleanupConfirmation(7, 2));
    expect(post).toHaveBeenCalledOnce();
    expect(post).toHaveBeenCalledWith('/api/plans/3/execute', {
      body: { options: {}, only: null },
      signal: undefined,
    });
    expect(binding.executing).toHaveBeenCalledWith(9);
    expect(selection.source.count.value).toBe(0);
  });

  it('renders the shared panes, one toggle, zero state and build errors with Retry', async () => {
    const page = await mount('opm-cleanup-page');
    await settle(page);
    expect(page.querySelectorAll('opm-pane')).toHaveLength(2);
    const toggle = page.querySelector<HTMLInputElement>('input.toggle');
    expect(toggle?.checked).toBe(false);
    if (!toggle) throw new Error('missing cleanup toggle');
    toggle.checked = true;
    toggle.dispatchEvent(new Event('change', { bubbles: true }));
    await settle(page);
    expect(cleanupTool.plan?.params(ctx)).toEqual({
      ready: true,
      params: { scope: { volume_id: 1, dir: 'base', recursive: false } },
    });
    binding.statsSignal.value = { empty_folders: 0 };
    await settle(page);
    expect(page.textContent).toContain('No empty folders');
    binding.statusSignal.value = 'error';
    binding.errorSignal.value = 'Pick a folder deeper in the tree.';
    await settle(page);
    expect(page.querySelector('[role=alert]')?.textContent).toContain('Pick a folder deeper');
    const retry = [...page.querySelectorAll('button')].find(
      (button) => button.textContent?.trim() === 'Retry',
    );
    retry?.click();
    expect(binding.retry).toHaveBeenCalledOnce();
  });
});
