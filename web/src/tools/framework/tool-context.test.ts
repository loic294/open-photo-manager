import { effect } from '@preact/signals-core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { FooterCounts } from '@/api/generated/FooterCounts';
import { ApmPane } from '@/components/apm-pane';
import { filterScopes, resetFilters, setFilter, setScope } from '@/stores/filters';
import { type PaneLocation, ROOT_LOCATION, setLocation, syncFromLocation } from '@/stores/nav';
import { resetSelectionForTests, selection } from '@/stores/selection';
import { resetToolStateForTests, setRecursive } from '@/stores/tool-state';
import { loadVolumes } from '@/stores/volumes';
import { stubBrowseServer } from '@/test/browse-server';
import { FakePlan, plainPane, toolConfig } from '@/test/tool-config';
import type { PlanBinding } from '@/tools/framework/plan-binding';
import type { PaneSpec, ToolConfig } from '@/tools/framework/tool-config';
import { createToolContext, type ToolContext } from '@/tools/framework/tool-context';
import { EMPTY_FILTER } from '@/util/filter';

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

const TRIP: PaneLocation = { volume: 'photos', dir: 'trip' };
const OLD: PaneLocation = { volume: 'backup', dir: 'old' };
const NOWHERE: PaneLocation = { volume: 'nowhere', dir: '' };

const pinned = (patch: Partial<PaneSpec> = {}): PaneSpec => ({
  heading: 'Destination',
  baseMode: 'pinned',
  showMatches: false,
  planSide: null,
  ...patch,
});

const current = (heading = 'Source'): PaneSpec => ({ ...plainPane(heading), baseMode: 'current' });

/** Formatting in place: the destination follows the source folder until a folder is pinned. */
const FORMAT = toolConfig({
  id: 'format',
  panes: {
    source: current(),
    comparison: pinned({ defaultBase: { from: 'source', label: 'Same as the source folder' } }),
  },
});

const stops: (() => void)[] = [];

function context(config: ToolConfig = toolConfig()): ToolContext {
  return createToolContext(config, document.createElement('div'));
}

/** What `read` answered each time it ran: once now, again after every change it depends on. */
function track<T>(read: () => T): T[] {
  const seen: T[] = [];
  stops.push(
    effect(() => {
      seen.push(read());
    }),
  );
  return seen;
}

beforeEach(async () => {
  window.history.replaceState(null, '', '/');
  syncFromLocation();
  stubBrowseServer();
  await loadVolumes();
});

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  seam.make = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  resetSelectionForTests();
  resetToolStateForTests();
  resetFilters();
  filterScopes.value = {};
});

describe('identity', () => {
  it('exposes the config, the tool id and the host element', () => {
    const config = toolConfig({ id: 'cleanup', filter: { applies: 'none', reason: 'Absolute.' } });
    const host = document.createElement('section');
    const ctx = createToolContext(config, host);
    expect(ctx.config).toBe(config);
    expect(ctx.tool).toBe('cleanup');
    expect(ctx.host).toBe(host);
  });

  it('has no plan unless the plan client gives one', () => {
    const ctx = context();
    expect(ctx.plan.status).toBe('none');
    expect(ctx.plan.ref('source')).toBeNull();
  });

  it('asks for the plan binding last: the context it gets is complete', () => {
    let seen: unknown;
    seam.make = (ctx) => {
      const c = ctx as ToolContext;
      seen = {
        tool: c.tool,
        host: c.host,
        location: c.location('source'),
        scope: c.scope('source'),
        counts: c.counts('source'),
      };
      return new FakePlan();
    };
    const host = document.createElement('div');
    createToolContext(toolConfig({ id: 'move' }), host);
    expect(seen).toEqual({
      tool: 'move',
      host,
      location: ROOT_LOCATION,
      scope: null,
      counts: null,
    });
  });

  it('hands out the binding the plan client made', () => {
    const plan = new FakePlan();
    seam.make = () => plan;
    expect(context().plan).toBe(plan);
  });
});

describe('location', () => {
  it('is what the pane browses and follows the navigation store', () => {
    const ctx = context();
    const seen = track(() => ctx.location('source'));
    setLocation('source', TRIP);
    expect(ctx.location('source')).toEqual(TRIP);
    expect(ctx.location('comparison')).toEqual(ROOT_LOCATION);
    expect(seen).toEqual([ROOT_LOCATION, TRIP]);
  });

  it('starts from the address bar, so a deep link is where the pane begins', () => {
    window.history.replaceState(null, '', '/browse?src=photos/trip&cmp=backup/old');
    syncFromLocation();
    const ctx = context();
    expect(ctx.location('source')).toEqual(TRIP);
    expect(ctx.location('comparison')).toEqual(OLD);
  });
});

describe('scope', () => {
  it('is the browsed folder of a pane that has no base', () => {
    setLocation('source', TRIP);
    setLocation('comparison', OLD);
    const ctx = context();
    expect(ctx.scope('source')).toEqual({ volume_id: 1, dir: 'trip', recursive: true });
    expect(ctx.scope('comparison')).toEqual({ volume_id: 2, dir: 'old', recursive: true });
  });

  it('is null at the volumes root and for a volume that is not known', () => {
    const ctx = context();
    expect(ctx.scope('source')).toBeNull();
    setLocation('source', NOWHERE);
    expect(ctx.scope('source')).toBeNull();
  });

  it('is the base of a pane whose browsed folder is the base', () => {
    const ctx = context(FORMAT);
    setLocation('source', TRIP);
    expect(ctx.scope('source')).toEqual({ volume_id: 1, dir: 'trip', recursive: true });
  });

  it('is the pinned folder, not the folder the pane browses, for a pinned pane', () => {
    const ctx = context(FORMAT);
    setLocation('comparison', TRIP);
    expect(ctx.scope('comparison')).toBeNull();
    ctx.setBase('comparison', OLD);
    expect(ctx.scope('comparison')).toEqual({ volume_id: 2, dir: 'old', recursive: true });
  });

  it('is the scope of the default base until a folder is pinned', () => {
    const ctx = context(FORMAT);
    setLocation('source', TRIP);
    expect(ctx.scope('comparison')).toEqual({ volume_id: 1, dir: 'trip', recursive: true });
    setLocation('source', { volume: 'photos', dir: 'trip/day1' });
    expect(ctx.scope('comparison')).toEqual({ volume_id: 1, dir: 'trip/day1', recursive: true });
  });

  it('re-runs a reader when the folder changes', () => {
    const ctx = context();
    const seen = track(() => ctx.scope('source')?.dir ?? null);
    setLocation('source', TRIP);
    expect(seen).toEqual([null, 'trip']);
  });

  it('follows the Include subfolders switch only for a tool that has one', () => {
    setLocation('source', TRIP);
    const always = context(toolConfig({ id: 'move' }));
    setRecursive('move', false);
    expect(always.recursive).toBe(true);
    expect(always.scope('source')?.recursive).toBe(true);

    const toggled = context(toolConfig({ id: 'duplicates', recursive: 'toggle' }));
    expect(toggled.recursive).toBe(true);
    const seen = track(() => toggled.scope('source')?.recursive);
    setRecursive('duplicates', false);
    expect(toggled.recursive).toBe(false);
    expect(seen).toEqual([true, false]);
  });
});

describe('base', () => {
  it('is none for a pane that only browses', () => {
    setLocation('source', TRIP);
    expect(context().base('source')).toBeNull();
  });

  it('is the browsed folder for a pane in current mode', () => {
    const ctx = context(FORMAT);
    const seen = track(() => ctx.base('source'));
    setLocation('source', TRIP);
    expect(ctx.base('source')).toEqual(TRIP);
    expect(seen).toEqual([ROOT_LOCATION, TRIP]);
  });

  it('is none for a pinned pane until the user pins a folder', () => {
    const ctx = context(
      toolConfig({ id: 'move', panes: { source: current(), comparison: pinned() } }),
    );
    expect(ctx.base('comparison')).toBeNull();
    expect(ctx.baseIsDefault('comparison')).toBe(false);
    ctx.setBase('comparison', OLD);
    expect(ctx.base('comparison')).toEqual(OLD);
    ctx.setBase('comparison', null);
    expect(ctx.base('comparison')).toBeNull();
  });

  it('ignores setBase for a pane that is not pinned', () => {
    const ctx = context(FORMAT);
    ctx.setBase('source', OLD);
    expect(ctx.base('source')).toEqual(ROOT_LOCATION);
    const plain = context();
    plain.setBase('source', OLD);
    expect(plain.base('source')).toBeNull();
  });

  it('keeps a pinned base apart per tool and per pane', () => {
    const ctx = context(FORMAT);
    ctx.setBase('comparison', OLD);
    const other = context(
      toolConfig({ id: 'move', panes: { source: pinned(), comparison: pinned() } }),
    );
    expect(other.base('comparison')).toBeNull();
    expect(other.base('source')).toBeNull();
    expect(ctx.base('comparison')).toEqual(OLD);
  });

  describe('default', () => {
    it('follows the other pane while nothing is pinned', () => {
      const ctx = context(FORMAT);
      const seen = track(() => ctx.base('comparison')?.dir ?? null);
      expect(ctx.baseIsDefault('comparison')).toBe(true);
      setLocation('source', TRIP);
      expect(ctx.base('comparison')).toEqual(TRIP);
      expect(seen).toEqual(['', 'trip']);
    });

    it('lets a pinned folder win, and comes back with setBase(null)', () => {
      const ctx = context(FORMAT);
      setLocation('source', TRIP);
      ctx.setBase('comparison', OLD);
      expect(ctx.base('comparison')).toEqual(OLD);
      expect(ctx.baseIsDefault('comparison')).toBe(false);
      setLocation('source', { volume: 'photos', dir: 'other' });
      expect(ctx.base('comparison')).toEqual(OLD);
      ctx.setBase('comparison', null);
      expect(ctx.baseIsDefault('comparison')).toBe(true);
      expect(ctx.base('comparison')).toEqual({ volume: 'photos', dir: 'other' });
    });

    it('is never the default for a pane without one', () => {
      expect(context(FORMAT).baseIsDefault('source')).toBe(false);
      expect(context().baseIsDefault('comparison')).toBe(false);
    });

    it('is the pinned folder of the other pane when that is pinned', () => {
      const ctx = context(
        toolConfig({
          id: 'move',
          panes: {
            source: pinned({ heading: 'Source' }),
            comparison: pinned({ defaultBase: { from: 'source', label: 'Same as the source' } }),
          },
        }),
      );
      expect(ctx.base('comparison')).toBeNull();
      ctx.setBase('source', TRIP);
      expect(ctx.base('comparison')).toEqual(TRIP);
    });

    it('stops after one hop when a config points two panes at each other', () => {
      const ctx = context(
        toolConfig({
          id: 'move',
          panes: {
            source: pinned({ defaultBase: { from: 'comparison', label: 'a' } }),
            comparison: pinned({ defaultBase: { from: 'source', label: 'b' } }),
          },
        }),
      );
      expect(ctx.base('source')).toBeNull();
      ctx.setBase('comparison', OLD);
      expect(ctx.base('source')).toEqual(OLD);
      expect(ctx.base('comparison')).toEqual(OLD);
    });
  });
});

describe('filter', () => {
  const FILTER = { ...EMPTY_FILTER, cameras: ['Sony A7 IV'] };

  it('applies to both panes for applies: both', () => {
    const ctx = context(toolConfig({ filter: { applies: 'both' } }));
    expect(ctx.filterScope).toBe('both');
    setFilter('browse', FILTER);
    expect(ctx.toolFilter).toEqual(FILTER);
    expect(ctx.filter('source')).toBe(ctx.toolFilter);
    expect(ctx.filter('comparison')).toBe(ctx.toolFilter);
  });

  it('applies to the source only for applies: source', () => {
    const ctx = context(toolConfig({ id: 'move', filter: { applies: 'source' } }));
    expect(ctx.filterScope).toBe('source');
    setFilter('move', FILTER);
    expect(ctx.filter('source')).toEqual(FILTER);
    expect(ctx.filter('comparison')).toBe(EMPTY_FILTER);
    expect(ctx.toolFilter).toEqual(FILTER);
  });

  it('applies where the user chooses for applies: choice', () => {
    const ctx = context(toolConfig({ id: 'duplicates', filter: { applies: 'choice' } }));
    setFilter('duplicates', FILTER);
    expect(ctx.filterScope).toBe('both');
    expect(ctx.filter('comparison')).toEqual(FILTER);
    setScope('duplicates', 'comparison');
    expect(ctx.filterScope).toBe('comparison');
    expect(ctx.filter('source')).toBe(EMPTY_FILTER);
    expect(ctx.filter('comparison')).toEqual(FILTER);
    setScope('duplicates', 'source');
    expect(ctx.filter('source')).toEqual(FILTER);
    expect(ctx.filter('comparison')).toBe(EMPTY_FILTER);
  });

  it('starts a choice on its initial scope, then keeps the user choice', () => {
    const ctx = context(
      toolConfig({ id: 'duplicates', filter: { applies: 'choice', initial: 'source' } }),
    );
    expect(ctx.filterScope).toBe('source');
    setScope('duplicates', 'both');
    expect(ctx.filterScope).toBe('both');
  });

  it('applies to no pane for applies: none, whatever the store holds', () => {
    const ctx = context(toolConfig({ id: 'cleanup', filter: { applies: 'none', reason: 'x' } }));
    setFilter('cleanup', { ...EMPTY_FILTER, selection_id: 9 });
    expect(ctx.filterScope).toBeNull();
    expect(ctx.filter('source')).toBe(EMPTY_FILTER);
    expect(ctx.filter('comparison')).toBe(EMPTY_FILTER);
    expect(ctx.toolFilter.selection_id).toBe(9);
  });

  it('keeps the filter of each tool apart', () => {
    const ctx = context();
    setFilter('duplicates', FILTER);
    expect(ctx.toolFilter).toEqual(EMPTY_FILTER);
  });

  it('re-runs a reader when the filter or the scope changes', () => {
    const ctx = context(toolConfig({ id: 'duplicates', filter: { applies: 'choice' } }));
    const seen = track(() => ctx.filter('source').cameras.length);
    setFilter('duplicates', FILTER);
    setScope('duplicates', 'comparison');
    expect(seen).toEqual([0, 1, 0]);
  });
});

describe('counts', () => {
  const counts = (in_folder: number, in_subfolders = 0): FooterCounts => ({
    in_folder,
    in_subfolders,
  });

  it('is null until a pane reports', () => {
    const ctx = context();
    expect(ctx.counts('source')).toBeNull();
    expect(ctx.counts('comparison')).toBeNull();
  });

  it('keeps what each pane reported and tells readers', () => {
    const ctx = context();
    const seen = track(() => ctx.counts('source'));
    ctx.reportCounts('source', counts(3, 1));
    ctx.reportCounts('comparison', counts(8));
    expect(ctx.counts('source')).toEqual(counts(3, 1));
    expect(ctx.counts('comparison')).toEqual(counts(8));
    expect(seen).toEqual([null, counts(3, 1)]);
  });

  it('does not tell readers again for the same numbers in a new object', () => {
    const ctx = context();
    const seen = track(() => ctx.counts('source'));
    ctx.reportCounts('source', counts(3, 1));
    ctx.reportCounts('source', counts(3, 1));
    ctx.reportCounts('source', counts(4, 1));
    ctx.reportCounts('source', counts(4, 2));
    expect(seen).toEqual([null, counts(3, 1), counts(4, 1), counts(4, 2)]);
  });

  it('goes back to null while a pane reloads', () => {
    const ctx = context();
    const seen = track(() => ctx.counts('source'));
    ctx.reportCounts('source', counts(1));
    ctx.reportCounts('source', null);
    ctx.reportCounts('source', null);
    expect(ctx.counts('source')).toBeNull();
    expect(seen).toEqual([null, counts(1), null]);
  });
});

describe('selection', () => {
  it('is the selection of the pane', () => {
    const ctx = context();
    expect(ctx.selection('source')).toBe(selection.source);
    expect(ctx.selection('comparison')).toBe(selection.comparison);
  });

  it('clears both selections', () => {
    const ctx = context();
    selection.source.toggle(4);
    selection.comparison.toggle(7);
    ctx.clearSelections();
    expect(ctx.selection('source').toArray()).toEqual([]);
    expect(ctx.selection('comparison').toArray()).toEqual([]);
  });
});

describe('request', () => {
  const FILTER = { ...EMPTY_FILTER, tags: ['sunset'] };

  it('is what the pane shows: its folder, its effective filter and no plan', () => {
    setLocation('source', TRIP);
    setFilter('browse', FILTER);
    const ctx = context();
    expect(ctx.request('source')).toEqual({
      volume_id: 1,
      dir: 'trip',
      filter: FILTER,
      plan: null,
    });
  });

  it('lists every volume at the volumes root', () => {
    expect(context().request('source')).toEqual({
      volume_id: null,
      dir: '',
      filter: EMPTY_FILTER,
      plan: null,
    });
  });

  it('is null for a volume that is not known', () => {
    setLocation('comparison', NOWHERE);
    expect(context().request('comparison')).toBeNull();
  });

  it('carries the effective filter of that pane, not the tool filter', () => {
    setFilter('move', FILTER);
    const ctx = context(toolConfig({ id: 'move', filter: { applies: 'source' } }));
    expect(ctx.request('source')?.filter).toEqual(FILTER);
    expect(ctx.request('comparison')?.filter).toBe(EMPTY_FILTER);
  });

  it('carries the plan of the side the pane shows', () => {
    const plan = new FakePlan();
    seam.make = () => plan;
    const ctx = context(
      toolConfig({
        id: 'duplicates',
        plan: { kind: 'duplicates', params: () => ({ ready: false, reason: null }) },
        panes: {
          source: { ...plainPane('Source'), planSide: 'source' },
          comparison: { ...plainPane('Comparison'), planSide: 'comparison' },
        },
      }),
    );
    expect(ctx.request('source')?.plan).toBeNull();
    plan.planIdSignal.value = 12;
    expect(ctx.request('source')?.plan).toEqual({ plan_id: 12, side: 'source' });
    expect(ctx.request('comparison')?.plan).toEqual({ plan_id: 12, side: 'comparison' });
  });

  it('never carries a plan for a pane that does not show one', () => {
    const plan = new FakePlan();
    plan.planIdSignal.value = 12;
    seam.make = () => plan;
    const ctx = context(
      toolConfig({
        id: 'move',
        plan: { kind: 'move', params: () => ({ ready: false, reason: null }) },
        panes: {
          source: { ...plainPane('Source'), planSide: 'source' },
          comparison: plainPane('Destination'),
        },
      }),
    );
    expect(ctx.request('source')?.plan).toEqual({ plan_id: 12, side: 'source' });
    expect(ctx.request('comparison')?.plan).toBeNull();
  });

  it('re-runs a reader when the plan changes', () => {
    const plan = new FakePlan();
    seam.make = () => plan;
    const ctx = context(
      toolConfig({
        id: 'cleanup',
        plan: { kind: 'cleanup', params: () => ({ ready: false, reason: null }) },
        panes: {
          source: { ...plainPane('Source'), planSide: 'source' },
          comparison: plainPane('Comparison'),
        },
      }),
    );
    const seen = track(() => ctx.request('source')?.plan?.plan_id ?? null);
    plan.planIdSignal.value = 3;
    plan.planIdSignal.value = 4;
    expect(seen).toEqual([null, 3, 4]);
  });
});

describe('refresh', () => {
  it('asks every pane inside the host to reload, and nothing else', () => {
    const host = document.createElement('div');
    const first = document.createElement('apm-pane');
    const second = document.createElement('apm-pane');
    host.append(first, document.createElement('p'), second);
    const refresh = vi.spyOn(ApmPane.prototype, 'refresh').mockImplementation(() => {});
    const ctx = createToolContext(toolConfig(), host);
    ctx.refresh();
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(refresh.mock.contexts).toEqual([first, second]);
  });

  it('does nothing without panes', () => {
    const refresh = vi.spyOn(ApmPane.prototype, 'refresh').mockImplementation(() => {});
    expect(() => context().refresh()).not.toThrow();
    expect(refresh).not.toHaveBeenCalled();
  });
});

describe('dispose', () => {
  it('stops the plan binding, once', () => {
    const plan = new FakePlan();
    seam.make = () => plan;
    const ctx = context();
    ctx.dispose();
    ctx.dispose();
    expect(plan.dispose).toHaveBeenCalledTimes(1);
  });

  it('works for a binding with nothing to stop', () => {
    expect(() => context().dispose()).not.toThrow();
  });
});
