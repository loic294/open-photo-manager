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
  regexIssue,
  type SelectionRule,
  selectKeepers,
  selectWithReasons,
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
  pattern: '',
  folders: [],
  ...patch,
});

it('matches filename regexes and explains the winning rule', () => {
  const a = file(1, { item: fileItem({ id: 1, name: 'copy.jpg' }) });
  const b = file(2, { item: fileItem({ id: 2, name: 'original.JPG' }) });
  const group: DuplicateFileGroup = { files: [a, b], pairs: [[1, 2]] };
  const rules = [rule('regex', { pattern: '^original\\.JPG$' })];
  const selected = selectWithReasons(group, rules);
  expect([...selected.keep]).toEqual([2]);
  expect(selected.reasons.get(2)).toContain('Name matches regex');
  expect(selected.reasons.get(1)).toContain('original.JPG');
  expect(selectWithReasons(group, rules, 1).reasons.get(1)).toBe('Keep: Manual selection');
  expect(regexIssue('[')).not.toBeNull();
  expect(regexIssue('')).not.toBeNull();
  expect(regexIssue('^original\\.JPG$')).toBeNull();
  expect(compareFiles(a, b, [rule('regex', { pattern: 'original\\.jpg' })])).toBeLessThan(0);
});

it('badges all favorable enabled rules, distinguishing supporting evidence from deciding rules', () => {
  const a = file(1, {
    item: fileItem({ id: 1, name: 'original.jpg', size: 10, rating: 5, capture_ts: 50 }),
    has_description: true,
    tags: ['trip'],
  });
  const b = file(2, {
    item: fileItem({ id: 2, name: 'copy.jpg', size: 20, rating: 0, capture_ts: 10 }),
  });
  const group: DuplicateFileGroup = { files: [a, b], pairs: [[1, 2]] };
  const rules = [
    rule('size'),
    rule('rating'),
    rule('description'),
    rule('tags'),
    rule('capture'),
    rule('tag', { enabled: false, tag: 'trip' }),
  ];
  const selected = selectWithReasons(group, rules);
  expect([...selected.keep]).toEqual([1]);
  expect(selected.badges.get(1)?.map((badge) => badge.kind)).toEqual([
    'size',
    'rating',
    'description',
    'tags',
  ]);
  expect(selected.badges.get(1)?.[0]?.detail).toContain('Decisive against: copy.jpg');
  expect(selected.badges.get(1)?.[1]?.detail).toContain('Supporting preference over: copy.jpg');
  expect(selected.badges.has(2)).toBe(false);
  const manual = selectWithReasons(group, rules, 2);
  expect(manual.badges.get(2)?.map((badge) => badge.kind)).toEqual(['manual', 'capture']);
  expect(manual.badges.get(2)?.[1]?.detail).toContain('did not decide');
});

it('compares badge evidence only with direct removals, including a rule favoring only some of them', () => {
  const files = [10, 5, 20, 1, 2].map((size, index) =>
    file(index + 1, {
      item: fileItem({
        id: index + 1,
        name: `file-${index + 1}.jpg`,
        size,
        rating: index === 0 ? 5 : 0,
      }),
    }),
  );
  const group: DuplicateFileGroup = {
    files,
    pairs: [
      [1, 2],
      [1, 3],
      [3, 4],
      [4, 5],
    ],
  };
  const selected = selectWithReasons(group, [rule('rating'), rule('size')]);
  expect([...selected.keep]).toEqual([1, 4]);
  const size = selected.badges.get(1)?.find((badge) => badge.kind === 'size');
  expect(size).toBeDefined();
  // File 1 is smaller than removal 3 but larger than removal 2; 5 is not its direct match.
  expect(size?.detail).toContain(files[2]?.item.name);
  expect(size?.detail).not.toContain('file-2.jpg');
  expect(size?.detail).not.toContain('file-5.jpg');
  const onlyTied = selectWithReasons(
    {
      files: [file(1), file(2)],
      pairs: [[1, 2]],
    },
    [rule('size')],
  );
  expect(onlyTied.badges.get(1)?.map((badge) => badge.kind)).toEqual(['tie']);
});

it.each(['asc', 'desc'] as const)(
  'badges known dates before missing dates in %s order',
  (direction) => {
    const group: DuplicateFileGroup = {
      files: [
        file(1, { item: fileItem({ id: 1, capture_ts: null }) }),
        file(2, { item: fileItem({ id: 2, capture_ts: 10 }) }),
      ],
      pairs: [[1, 2]],
    };
    expect(
      selectWithReasons(group, [rule('capture', { direction })]).badges.get(2)?.[0],
    ).toMatchObject({
      kind: 'capture',
      detail: expect.stringContaining('known dates first'),
    });
  },
);

it('prefers shorter or longer Unicode filenames and preserves direct-match coverage', () => {
  const a = file(1, { item: fileItem({ id: 1, name: 'long-name.jpg' }) });
  const b = file(2, { item: fileItem({ id: 2, name: '📷.jpg' }) });
  expect(compareFiles(a, b, [rule('name_length')])).toBeGreaterThan(0);
  expect(compareFiles(a, b, [rule('name_length', { direction: 'desc' })])).toBeLessThan(0);
  const c = file(3, { item: fileItem({ id: 3, name: 'c.jpg' }) });
  expect(compareFiles(b, c, [rule('name_length')])).toBeLessThan(0);
  const selected = selectWithReasons(
    {
      files: [a, b, c],
      pairs: [
        [1, 2],
        [2, 3],
      ],
    },
    [rule('name_length', { direction: 'desc' })],
  );
  expect([...selected.keep]).toEqual([1, 3]);
  expect(selected.reasons.get(3)).toContain('direct-match coverage');
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
  expect(
    defaultRules()
      .slice(0, 4)
      .map((rule) => rule.kind),
  ).toEqual(['folder', 'subtree', 'regex', 'name_length']);
});
