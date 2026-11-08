import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { filterScopes, resetFilters, setFilter, setScope } from '@/stores/filters';
import { locations, setLocation, syncFromLocation } from '@/stores/nav';
import { resetToolStateForTests, setRecursive } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { duplicateParams } from '@/tools/duplicates/duplicates-params';
import { matchMode, strictPrefix } from '@/tools/duplicates/duplicates-state';
import { duplicatesTool } from '@/tools/duplicates/tool';
import { NO_PLAN } from '@/tools/framework/plan-binding';
import { paramsKey } from '@/tools/framework/plan-controller';
import { createToolContext } from '@/tools/framework/tool-context';
import { EMPTY_FILTER } from '@/util/filter';
import { MATCH_MODES } from '@/util/match-modes';

vi.mock('@/tools/framework/plan-binding', async (original) => ({
  ...(await original<typeof import('@/tools/framework/plan-binding')>()),
  createPlanBinding: () => NO_PLAN,
}));

beforeEach(async () => {
  window.history.replaceState(null, '', '/duplicates?src=photos/trip&cmp=backup/old');
  syncFromLocation();
  stubBrowseServer();
  await loadVolumes();
});
afterEach(() => {
  resetFilters();
  filterScopes.value = {};
  resetToolStateForTests();
  matchMode.value = 'hash';
  strictPrefix.value = false;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
const context = () => createToolContext(duplicatesTool, document.createElement('div'));

it.each([null, 'unknown'])('is not ready for unresolved volume %s on either side', (volume) => {
  for (const pane of ['source', 'comparison'] as const) {
    const previous = locations[pane].value;
    setLocation(pane, { volume, dir: '' });
    expect(duplicateParams(context())).toEqual({
      ready: false,
      reason: 'Open a volume or folder in both panes to look for duplicates.',
    });
    setLocation(pane, previous);
  }
});

it.each(MATCH_MODES)('builds generated parameters for $value', ({ value }) => {
  matchMode.value = value;
  strictPrefix.value = true;
  expect(duplicateParams(context())).toEqual({
    ready: true,
    params: {
      mode: value,
      strict_prefix: value === 'name_prefix',
      source: { scope: { volume_id: 1, dir: 'trip', recursive: true }, filter: EMPTY_FILTER },
      comparison: { scope: { volume_id: 2, dir: 'old', recursive: true }, filter: EMPTY_FILTER },
    },
  });
});

it('ignores strict outside prefix for the plan key and obeys the recursive switch', () => {
  const ctx = context();
  const initial = duplicateParams(ctx);
  strictPrefix.value = true;
  expect(paramsKey(duplicateParams(ctx))).toBe(paramsKey(initial));
  setRecursive('duplicates', false);
  expect(duplicateParams(ctx)).toMatchObject({
    params: {
      source: { scope: { recursive: false } },
      comparison: { scope: { recursive: false } },
    },
  });
});

it.each(['both', 'source', 'comparison'] as const)(
  'uses effective %s filters, normalized',
  (scope) => {
    setScope('duplicates', scope);
    setFilter('duplicates', { ...EMPTY_FILTER, cameras: [' Canon ', 'Canon'], ratings: [5, 5, 1] });
    const filter = { ...EMPTY_FILTER, cameras: ['Canon'], ratings: [1, 5] };
    expect(duplicateParams(context())).toMatchObject({
      params: {
        source: { filter: scope === 'comparison' ? EMPTY_FILTER : filter },
        comparison: { filter: scope === 'source' ? EMPTY_FILTER : filter },
      },
    });
  },
);
