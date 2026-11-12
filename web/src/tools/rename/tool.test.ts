import { render } from 'lit';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api } from '@/api/client';
import { ApiError } from '@/api/errors';
import { routes } from '@/app/routes';
import { confirmDialog } from '@/components/confirm-dialog';
import { resetFilters, setFilter } from '@/stores/filters';
import { locations, setLocation } from '@/stores/nav';
import { resetPlanRunsForTests } from '@/stores/plan-runs';
import { resetSelectionForTests } from '@/stores/selection';
import { resetToolStateForTests, setRecursive } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { makeTask } from '@/test/task-fixtures';
import { FakePlan } from '@/test/tool-config';
import { checkToolConfig } from '@/tools/framework/tool-config';
import { createToolContext } from '@/tools/framework/tool-context';
import {
  clean,
  countDigits,
  currentRules,
  matchMode,
  matchPattern,
  pattern,
  policy,
  previewKey,
  renameParams,
  rewriteEnabled,
  rewriteFind,
  rewriteReplace,
  validation,
} from './rename-state';
import { renameTool } from './tool';

vi.mock('@/components/confirm-dialog', () => ({ confirmDialog: vi.fn(async () => true) }));
vi.mock('@/tools/framework/plan-binding', async (original) => ({
  ...(await original<typeof import('@/tools/framework/plan-binding')>()),
  createPlanBinding: () => binding,
}));
let binding: FakePlan & { executing: ReturnType<typeof vi.fn> };
beforeEach(async () => {
  binding = Object.assign(new FakePlan(), { executing: vi.fn() });
  binding.statusSignal.value = 'ready';
  binding.planIdSignal.value = 7;
  binding.statsSignal.value = { to_rename: 4, matched: 6, already_in_place: 2 };
  stubBrowseServer();
  await loadVolumes();
  setLocation('source', { volume: 'photos', dir: 'cafe\u0301' });
  validation.value = { key: '', status: 'idle', issues: [] };
  pattern.value = '{name}';
  clean.value = false;
  countDigits.value = 0;
  matchMode.value = 'all';
  matchPattern.value = '';
  rewriteEnabled.value = false;
  rewriteFind.value = '';
  rewriteReplace.value = '';
  policy.value = 'smart';
  vi.mocked(confirmDialog).mockResolvedValue(true);
});
afterEach(() => {
  resetToolStateForTests();
  resetFilters();
  resetSelectionForTests();
  resetPlanRunsForTests();
  vi.restoreAllMocks();
});
function context() {
  return createToolContext(renameTool, document.createElement('div'));
}
function validate(ctx: ReturnType<typeof context>) {
  validation.value = { key: previewKey(renameParams(ctx)), status: 'valid', issues: [] };
}

it('is a lazy thin tool using source filters and independent comparison browsing', async () => {
  expect(checkToolConfig(renameTool)).toBeNull();
  expect(routes.find((route) => route.path === '/rename')).toMatchObject({
    tag: 'opm-rename-page',
    label: 'Batch rename',
    icon: 'pencil',
    menu: 'tools',
  });
  const ctx = context();
  const stop = renameTool.setup?.(ctx);
  expect(locations.comparison.value).toEqual(locations.source.value);
  validate(ctx);
  const first = renameTool.plan?.params(ctx);
  setLocation('comparison', { volume: 'photos', dir: 'independent' });
  expect(renameTool.plan?.params(ctx)).toEqual(first);
  setLocation('source', { volume: 'photos', dir: 'next' });
  await Promise.resolve();
  expect(locations.comparison.value.dir).toBe('next');
  expect(renameTool.plan?.params(ctx).ready).toBe(false);
  stop?.();
});

it('sends complete rules, preserves rewrite drafts and gates every current revision', () => {
  const ctx = context();
  expect(renameTool.plan?.params(ctx).ready).toBe(false);
  validate(ctx);
  expect(renameTool.plan?.params(ctx)).toEqual({
    ready: true,
    params: {
      source: {
        scope: { volume_id: 1, dir: 'cafe\u0301', recursive: true },
        filter: ctx.filter('source'),
      },
      rules: currentRules(),
      conflict_policy: 'smart',
    },
  });
  clean.value = true;
  expect(renameTool.plan?.params(ctx).ready).toBe(false);
  validate(ctx);
  rewriteFind.value = 'IMG';
  rewriteReplace.value = '$1';
  expect(currentRules().rewrite).toBeNull();
  rewriteEnabled.value = true;
  expect(currentRules().rewrite).toEqual({ find: 'IMG', replace: '$1', ignore_case: true });
  expect(renameTool.plan?.params(ctx).ready).toBe(false);
  rewriteEnabled.value = false;
  expect(rewriteFind.value).toBe('IMG');
  validate(ctx);
  setRecursive('rename', false);
  expect(renameTool.plan?.params(ctx).ready).toBe(false);
  validate(ctx);
  setFilter('rename', { ...ctx.filter('source'), ratings: [5], selection_id: 42 });
  expect(renameTool.plan?.params(ctx).ready).toBe(false);
  expect(renameParams(ctx)?.source.filter).toMatchObject({ ratings: [5], selection_id: 42 });
  expect(ctx.filter('comparison').ratings).toEqual([]);
  validate(ctx);
  validation.value = { ...validation.value, status: 'idle' };
  expect(renameTool.plan?.params(ctx).ready).toBe(false);
});

it('offers no replace policy and renders truthful no-op and field issues', () => {
  const ctx = context();
  validate(ctx);
  binding.statsSignal.value = { matched: 2, already_in_place: 2, to_rename: 0 };
  const host = document.createElement('div');
  render(renameTool.controls?.(ctx), host);
  expect(host.textContent).toContain('No names would change');
  const select = host.querySelector('opm-select');
  expect(select?.options.some((option) => option.value === 'replace')).toBe(false);
  validation.value = {
    key: previewKey(renameParams(ctx)),
    status: 'invalid',
    issues: [
      {
        field: 'matcher',
        code: 'invalid_regex',
        message: 'Bad matcher',
        suggestion: null,
        start: 0,
        end: 1,
      },
    ],
  };
  render(renameTool.controls?.(ctx), host);
  expect(host.textContent).toContain('Bad matcher');
  expect(host.querySelector('opm-pattern-editor')?.requestKey).toBe(previewKey(renameParams(ctx)));
  const rulesPanel = host.querySelector(':scope > details.dropdown');
  expect(rulesPanel?.querySelector(':scope > .dropdown-content opm-pattern-editor')).not.toBeNull();
  expect(rulesPanel?.hasAttribute('open')).toBe(false);
});

it.each([false, true])(
  'confirms exact server counts for selected=%s and queues only the restricted source',
  async (selected) => {
    const ctx = context();
    validate(ctx);
    if (selected) ctx.selection('source').set([1, 2, 99]);
    const post = vi
      .spyOn(api, 'post')
      .mockResolvedValueOnce({ id: 8, item_count: 2, total_size: 1024 })
      .mockResolvedValueOnce(makeTask({ id: 10 }));
    await renameTool.actions?.(ctx)?.options[0]?.run(ctx);
    expect(post.mock.calls[0]?.[1]?.body).toEqual({
      side: 'source',
      restrict_ids: selected ? [1, 2, 99] : null,
    });
    expect(post.mock.calls[1]?.[1]?.body).toEqual({ options: {}, only: 8 });
    expect(vi.mocked(confirmDialog).mock.calls[0]?.[0].title).toBe('Rename 2 files?');
    const host = document.createElement('div');
    render(vi.mocked(confirmDialog).mock.calls[0]?.[0].message, host);
    expect(host.textContent).toContain('current folders');
    expect(host.textContent).toContain('sidecars');
    expect(binding.executing).toHaveBeenCalledWith(10);
    expect(ctx.selection('source').count.value).toBe(0);
  },
);

it.each(['empty', 'cancel', 'stale', 'failure'] as const)('never mutates for %s', async (mode) => {
  const ctx = context();
  validate(ctx);
  const post = vi
    .spyOn(api, 'post')
    .mockResolvedValue({ id: 8, item_count: mode === 'empty' ? 0 : 2, total_size: 10 });
  if (mode === 'cancel') vi.mocked(confirmDialog).mockResolvedValueOnce(false);
  if (mode === 'stale')
    vi.mocked(confirmDialog).mockImplementationOnce(async () => {
      binding.planIdSignal.value = 9;
      return true;
    });
  if (mode === 'failure')
    post.mockRejectedValueOnce(
      new ApiError({ status: 500, code: 'internal', message: 'Cannot queue' }),
    );
  const run = renameTool.actions?.(ctx)?.options[0]?.run(ctx);
  if (mode === 'failure') await expect(run).rejects.toThrow('Cannot queue');
  else await run;
  expect(post).toHaveBeenCalledOnce();
  expect(binding.executing).not.toHaveBeenCalled();
});
