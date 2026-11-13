import { describe, expect, it } from 'vitest';
import type { DuplicateFile } from '@/api/generated/DuplicateFile';
import type { DuplicateFileGroup } from '@/api/generated/DuplicateFileGroup';
import { fileItem } from '@/test/file-item';
import {
  compareFiles,
  defaultRules,
  differingFields,
  inFolder,
  parentFolders,
  type RuleKind,
  type SelectionRule,
  selectKeepers,
} from './file-selection';

const file = (id: number, patch: Partial<DuplicateFile> = {}): DuplicateFile => ({
  item: fileItem({ id }),
  btime: null,
  has_description: false,
  tags: [],
  ...patch,
});

it('compares exact raw fields rather than rounded sizes or date display strings', () => {
  const a = file(1, { item: fileItem({ name: 'one.jpg', size: 102400, capture_ts: 100 }) });
  const b = file(2, { item: fileItem({ name: 'two.jpg', size: 102401, capture_ts: 101 }) });
  expect(differingFields({ files: [a, b], pairs: [[1, 2]] })).toEqual({
    name: true,
    size: true,
    capture_ts: true,
  });
  expect(
    differingFields({ files: [a, { ...a, item: { ...a.item, id: 2 } }], pairs: [[1, 2]] }),
  ).toEqual({ name: false, size: false, capture_ts: false });
  expect(
    differingFields({
      files: [a, { ...a, item: { ...a.item, capture_ts: null } }],
      pairs: [[1, 2]],
    }).capture_ts,
  ).toBe(true);
});
const rule = (kind: RuleKind, patch: Partial<SelectionRule> = {}): SelectionRule => ({
  kind,
  enabled: true,
  direction: 'asc',
  tag: '',
  folders: [],
  ...patch,
});

it('uses ordered enabled rules and deterministic ties, with missing dates last in both directions', () => {
  const a = file(1, { item: fileItem({ id: 1, size: 10, capture_ts: 50 }) });
  const b = file(2, { item: fileItem({ id: 2, size: 20, capture_ts: 10 }) });
  expect(compareFiles(a, b, [rule('capture'), rule('size')])).toBeGreaterThan(0);
  expect(compareFiles(a, b, [rule('size'), rule('capture')])).toBeLessThan(0);
  expect(compareFiles(a, b, [rule('capture', { enabled: false }), rule('size')])).toBeLessThan(0);
  for (const direction of ['asc', 'desc'] as const) {
    expect(
      compareFiles(file(1, { item: fileItem({ id: 1, capture_ts: null }) }), b, [
        rule('capture', { direction }),
      ]),
    ).toBeGreaterThan(0);
    expect(compareFiles(file(1), file(2), [rule('created', { direction })])).toBeLessThan(0);
  }
});

describe('selection criteria', () => {
  const cases: Array<[RuleKind, DuplicateFile]> = [
    ['capture', file(2, { item: fileItem({ id: 2, capture_ts: 10 }) })],
    ['modified', file(2, { item: fileItem({ id: 2, mtime: 1 }) })],
    ['created', file(2, { btime: 10 })],
    ['rating', file(2, { item: fileItem({ id: 2, rating: 4 }) })],
    ['description', file(2, { has_description: true })],
    ['flag', file(2, { item: fileItem({ id: 2, flag: -1 }) })],
    ['size', file(2, { item: fileItem({ id: 2, size: 1 }) })],
    ['tags', file(2, { tags: ['trip'] })],
    ['tag', file(2, { tags: ['trip'] })],
    ['folder', file(2, { item: fileItem({ id: 2, volume_id: 3, dir: 'trip' }) })],
    ['subtree', file(2, { item: fileItem({ id: 2, volume_id: 3, dir: 'trip/day1' }) })],
  ];
  it.each(cases)('prefers the requested %s criterion', (kind, preferred) => {
    const baseline = file(1, {
      item: fileItem({
        id: 1,
        rating: 0,
        flag: 0,
        size: 1000,
        capture_ts: null,
        mtime: 1000,
        dir: 'other',
        volume_id: 4,
      }),
    });
    expect(
      compareFiles(preferred, baseline, [
        rule(kind, {
          tag: 'trip',
          folders: [
            { volume_id: 7, dir: 'unrelated', recursive: true },
            { volume_id: 3, dir: 'trip', recursive: true },
          ],
        }),
      ]),
    ).toBeLessThan(0);
  });
  it.each(['capture', 'modified', 'created', 'size'] as const)(
    'supports both orders for %s',
    (kind) => {
      const a = file(1, { item: fileItem({ id: 1, capture_ts: 1, mtime: 1, size: 1 }), btime: 1 });
      const b = file(2, { item: fileItem({ id: 2, capture_ts: 2, mtime: 2, size: 2 }), btime: 2 });
      expect(compareFiles(a, b, [rule(kind)])).toBeLessThan(0);
      expect(compareFiles(a, b, [rule(kind, { direction: 'desc' })])).toBeGreaterThan(0);
    },
  );
});

it('keeps every uncovered member of a non-transitive graph after a manual choice', () => {
  const group: DuplicateFileGroup = {
    files: [file(1), file(2), file(3)],
    pairs: [
      [1, 2],
      [2, 3],
    ],
  };
  expect([...selectKeepers(group, [], 2)]).toEqual([2]);
  expect([...selectKeepers(group, [], 1)]).toEqual([1, 3]);
  expect([...selectKeepers({ ...group, files: [...group.files].reverse() }, [], 1)]).toEqual([
    1, 3,
  ]);
});

it('honours folder boundaries, volumes, root and exact on-disk Unicode when listing ancestors', () => {
  const scope = { volume_id: 3, dir: 'trip', recursive: true };
  expect(
    inFolder(file(1, { item: fileItem({ dir: 'trip.old', volume_id: 3 }) }), scope, true),
  ).toBe(false);
  expect(
    inFolder(file(1, { item: fileItem({ dir: 'trip/day', volume_id: 4 }) }), scope, true),
  ).toBe(false);
  expect(
    inFolder(file(1, { item: fileItem({ dir: 'trip/day', volume_id: 3 }) }), scope, false),
  ).toBe(false);
  const nested = file(1, { item: fileItem({ dir: 'trip/e\u0301/day', volume_id: 3 }) });
  expect(parentFolders(nested).map((folder) => folder.dir)).toEqual([
    'trip/e\u0301/day',
    'trip/e\u0301',
    'trip',
    '',
  ]);
  expect(inFolder(nested, { ...scope, dir: '' }, true)).toBe(true);
  expect(defaultRules()).toHaveLength(11);
});
