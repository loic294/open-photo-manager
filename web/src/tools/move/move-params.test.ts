import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { resetFilters, setFilter } from '@/stores/filters';
import { setLocation, syncFromLocation } from '@/stores/nav';
import { resetToolStateForTests, setRecursive } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { formatRules } from '@/tools/format/format-state';
import { FORMAT_REASONS, formatTool } from '@/tools/format/tool';
import { NO_PLAN } from '@/tools/framework/plan-binding';
import { paramsKey } from '@/tools/framework/plan-controller';
import { createToolContext } from '@/tools/framework/tool-context';
import { EMPTY_FILTER } from '@/util/filter';
import { buildMoveParams, MOVE_REASONS, type MoveRules } from './move-params';
import { moveTool } from './tool';

vi.mock('@/tools/framework/plan-binding', async (original) => ({
  ...(await original<typeof import('@/tools/framework/plan-binding')>()),
  createPlanBinding: () => NO_PLAN,
}));

const rules: MoveRules = {
  moveType: 'keep_source',
  pattern: '{YYYY}/{mm}',
  patternStatus: 'valid',
  policy: 'default',
  matchMode: 'hash',
  strictPrefix: true,
};

beforeEach(async () => {
  window.history.replaceState(null, '', '/move?src=photos/trip&cmp=backup/old');
  syncFromLocation();
  stubBrowseServer();
  await loadVolumes();
});
afterEach(() => {
  resetFilters();
  resetToolStateForTests();
  formatRules.patternStatus.value = 'idle';
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function context() {
  const ctx = createToolContext(moveTool, document.createElement('div'));
  ctx.setBase('source', ctx.location('source'));
  ctx.setBase('comparison', ctx.location('comparison'));
  return ctx;
}

it.each(['keep_source', 'keep_destination', 'flat', 'custom'] as const)(
  'builds exact generated params for %s with irrelevant controls stripped',
  (moveType) => {
    const ctx = context();
    const result = buildMoveParams(ctx, {
      ...rules,
      moveType,
      matchMode: 'name_prefix',
      policy: 'replace',
    });
    expect(result).toEqual({
      ready: true,
      params: {
        move_type: moveType,
        source: { scope: { volume_id: 1, dir: 'trip', recursive: true }, filter: EMPTY_FILTER },
        destination: { volume_id: 2, dir: 'old' },
        pattern: moveType === 'custom' ? '{YYYY}/{mm}' : null,
        conflict_policy: 'replace',
        match_mode: moveType === 'keep_destination' ? 'name_prefix' : null,
        strict_prefix: moveType === 'keep_destination',
      },
    });
    if (moveType !== 'custom' && moveType !== 'keep_destination') {
      expect(
        paramsKey(
          buildMoveParams(ctx, { ...rules, moveType, pattern: 'unused', matchMode: 'name_prefix' }),
        ),
      ).toBe(paramsKey(buildMoveParams(ctx, { ...rules, moveType })));
    }
  },
);

it('uses source-only handoff filters and recursion, leaving default policy null', () => {
  const ctx = context();
  setFilter('move', { ...EMPTY_FILTER, cameras: [' Canon ', 'Canon'], selection_id: 32 });
  setRecursive('move', false);
  expect(buildMoveParams(ctx, rules)).toMatchObject({
    params: {
      source: { scope: { recursive: false }, filter: { cameras: ['Canon'], selection_id: 32 } },
      conflict_policy: null,
      match_mode: null,
      strict_prefix: false,
    },
  });
  expect(ctx.filter('comparison')).toEqual(EMPTY_FILTER);
});

it('explains missing and unresolved bases and custom checking/invalid states', () => {
  const ctx = context();
  ctx.setBase('source', null);
  expect(buildMoveParams(ctx, rules)).toEqual({ ready: false, reason: MOVE_REASONS.source });
  ctx.setBase('source', ctx.location('source'));
  ctx.setBase('comparison', { volume: 'vanished', dir: '' });
  expect(buildMoveParams(ctx, rules, { source: 'source', destination: 'missing' })).toEqual({
    ready: false,
    reason: 'missing',
  });
  ctx.setBase('comparison', ctx.location('comparison'));
  expect(buildMoveParams(ctx, { ...rules, moveType: 'custom', patternStatus: 'checking' })).toEqual(
    { ready: false, reason: 'Checking the pattern...' },
  );
  expect(
    buildMoveParams(ctx, { ...rules, moveType: 'custom', patternStatus: 'invalid' }).ready,
  ).toBe(false);
  expect(buildMoveParams(ctx, { ...rules, moveType: 'custom', patternStatus: 'idle' }).ready).toBe(
    true,
  );
});

it('formats in place by default, honors pin/reset, and locks custom without match state', () => {
  const ctx = createToolContext(formatTool, document.createElement('div'));
  expect(formatTool.plan?.params(ctx)).toMatchObject({
    ready: true,
    params: {
      move_type: 'custom',
      destination: { volume_id: 1, dir: 'trip' },
      match_mode: null,
      strict_prefix: false,
    },
  });
  ctx.setBase('comparison', { volume: 'backup', dir: 'elsewhere' });
  expect(formatTool.plan?.params(ctx)).toMatchObject({
    params: { destination: { volume_id: 2, dir: 'elsewhere' } },
  });
  ctx.setBase('comparison', null);
  expect(ctx.base('comparison')).toEqual(ctx.base('source'));
  setLocation('source', { volume: null, dir: '' });
  expect(formatTool.plan?.params(ctx)).toEqual({ ready: false, reason: FORMAT_REASONS.source });
});
