import { afterEach, expect, it } from 'vitest';
import type { SelectionDto } from '@/api/generated/SelectionDto';
import { filterFor, resetFilters, setFilter } from '@/stores/filters';
import { locations, setLocation, startNavSync, syncFromLocation } from '@/stores/nav';
import { handoffFor, pinnedBase, resetToolStateForTests } from '@/stores/tool-state';
import { plainPane, toolConfig } from '@/test/tool-config';
import { handOffSelection } from '@/tools/framework/hand-off';
import { EMPTY_FILTER } from '@/util/filter';

const selection: SelectionDto = {
  id: 3,
  item_count: 1204,
  total_size: 50,
  label: 'From Duplicate finder',
  created_at: 0,
  expires_at: 86400,
};
afterEach(() => {
  resetFilters();
  resetToolStateForTests();
  window.history.replaceState(null, '', '/');
  syncFromLocation();
});

it('replaces the target filter, sets the label, preserves exact origin and pins the move source', () => {
  const stop = startNavSync();
  setLocation('comparison', { volume: 'Photos', dir: 'E\u0301te\u0301' });
  setLocation('source', { volume: 'Other', dir: 'different' });
  setFilter('move', { ...EMPTY_FILTER, cameras: ['Old camera'] });
  handOffSelection({
    target: 'move',
    origin: 'comparison',
    selection,
    label: '1,204 matched files',
  });
  expect(filterFor('move')).toEqual({ ...EMPTY_FILTER, selection_id: 3 });
  expect(handoffFor('move')).toEqual({ label: '1,204 matched files' });
  expect(locations.source.value).toEqual({ volume: 'Photos', dir: 'E\u0301te\u0301' });
  expect(pinnedBase('move', 'source')).toEqual(locations.source.value);
  expect(window.location.pathname).toBe('/move');
  expect(new URLSearchParams(window.location.search).get('src')).toBe('Photos/E\u0301te\u0301');
  stop();
});

it('falls back to selection label, then grouped file count', () => {
  handOffSelection({ target: 'move', origin: 'source', selection });
  expect(handoffFor('move')?.label).toBe('From Duplicate finder');
  handOffSelection({ target: 'move', origin: 'source', selection: { ...selection, label: '' } });
  expect(handoffFor('move')?.label).toBe('1,204 files');
});

it('accepts a target config to honor its source base mode without guessing', () => {
  setLocation('source', { volume: 'Photos', dir: 'a' });
  const target = toolConfig({
    id: 'format',
    panes: {
      source: { ...plainPane('Source'), baseMode: 'current' },
      comparison: plainPane('Comparison'),
    },
  });
  handOffSelection({ target, origin: 'source', selection });
  expect(pinnedBase('format', 'source')).toBeNull();
  target.panes.source.baseMode = 'pinned';
  handOffSelection({ target, origin: 'source', selection });
  expect(pinnedBase('format', 'source')).toEqual({ volume: 'Photos', dir: 'a' });
});
