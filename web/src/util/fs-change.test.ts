import { describe, expect, it } from 'vitest';
import { type FsChangeScope, fsChangeAffects } from '@/util/fs-change';

const scope = (volume_id: number | null, dir: string, plan: unknown = null): FsChangeScope => ({
  volume_id,
  dir,
  plan,
});
const event = (volume_id: number, ...dirs: string[]) => ({ volume_id, dirs });

describe('fsChangeAffects: a listing of direct children', () => {
  it('matches an event for its own directory', () => {
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/b'))).toBe(true);
  });

  it('matches when any of several changed dirs is its own', () => {
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'x', 'a/b', 'y'))).toBe(true);
  });

  it('ignores other directories', () => {
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/c'))).toBe(false);
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'x', 'y/z'))).toBe(false);
  });

  it('ignores the parent, a child and a deep descendant', () => {
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a'))).toBe(false);
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/b/c'))).toBe(false);
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/b/c/d'))).toBe(false);
  });

  it('ignores a directory that merely shares the name prefix', () => {
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/bc'))).toBe(false);
  });

  it('compares dirs exactly: case and unicode forms are different directories', () => {
    expect(fsChangeAffects(scope(1, 'Photos'), event(1, 'photos'))).toBe(false);
    expect(fsChangeAffects(scope(1, 'caf\u00e9'), event(1, 'cafe\u0301'))).toBe(false);
    expect(fsChangeAffects(scope(1, 'cafe\u0301'), event(1, 'cafe\u0301'))).toBe(true);
  });

  it('ignores an event without directories', () => {
    expect(fsChangeAffects(scope(1, 'a'), event(1))).toBe(false);
    expect(fsChangeAffects(scope(null, ''), event(1))).toBe(false);
  });

  it('accepts a read-only list of dirs', () => {
    const dirs: readonly string[] = Object.freeze(['a']);
    expect(fsChangeAffects(scope(1, 'a'), { volume_id: 1, dirs })).toBe(true);
  });
});

describe('fsChangeAffects: the volume', () => {
  it('ignores another volume', () => {
    expect(fsChangeAffects(scope(1, 'a'), event(2, 'a'))).toBe(false);
  });

  it('listens to every volume when the scope has none', () => {
    expect(fsChangeAffects(scope(null, 'a'), event(1, 'a'))).toBe(true);
    expect(fsChangeAffects(scope(null, 'a'), event(7, 'a'))).toBe(true);
    expect(fsChangeAffects(scope(null, 'a'), event(7, 'b'))).toBe(false);
  });

  it('treats volume 0 as a real volume, not as "none"', () => {
    expect(fsChangeAffects(scope(0, 'a'), event(1, 'a'))).toBe(false);
    expect(fsChangeAffects(scope(0, 'a'), event(0, 'a'))).toBe(true);
  });
});

describe('fsChangeAffects: the volume root', () => {
  it('matches the root only for an event on the root', () => {
    expect(fsChangeAffects(scope(1, ''), event(1, ''))).toBe(true);
    expect(fsChangeAffects(scope(1, ''), event(1, 'a'))).toBe(false);
    expect(fsChangeAffects(scope(1, 'a'), event(1, ''))).toBe(false);
  });
});

describe('fsChangeAffects: an active plan', () => {
  const plan = { plan_id: 3, side: 'source' };

  it('matches every event, whatever the volume and directory', () => {
    expect(fsChangeAffects(scope(1, 'a', plan), event(2, 'elsewhere'))).toBe(true);
    expect(fsChangeAffects(scope(null, '', plan), event(9, 'x/y'))).toBe(true);
    expect(fsChangeAffects(scope(1, 'a', plan), event(1))).toBe(true);
  });

  it('is off for null and for a missing plan', () => {
    expect(fsChangeAffects(scope(1, 'a', null), event(2, 'a'))).toBe(false);
    expect(fsChangeAffects({ volume_id: 1, dir: 'a' }, event(2, 'a'))).toBe(false);
    expect(fsChangeAffects(scope(1, 'a', undefined), event(2, 'a'))).toBe(false);
  });

  it('takes a falsy but present plan as active', () => {
    expect(fsChangeAffects(scope(1, 'a', 0), event(2, 'b'))).toBe(true);
  });
});

describe('fsChangeAffects: subtree', () => {
  const subtree = { subtree: true };

  it('also matches the own directory', () => {
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/b'), subtree)).toBe(true);
  });

  it('matches a child and a deep descendant', () => {
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/b/c'), subtree)).toBe(true);
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/b/c/d/e'), subtree)).toBe(true);
  });

  it('does not match the parent, a sibling or a name that only shares the prefix', () => {
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a'), subtree)).toBe(false);
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/c'), subtree)).toBe(false);
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/bc'), subtree)).toBe(false);
    expect(fsChangeAffects(scope(1, 'a/b'), event(1, 'a/bc/d'), subtree)).toBe(false);
  });

  it('matches any directory for the volume root', () => {
    expect(fsChangeAffects(scope(1, ''), event(1, ''), subtree)).toBe(true);
    expect(fsChangeAffects(scope(1, ''), event(1, 'a'), subtree)).toBe(true);
    expect(fsChangeAffects(scope(1, ''), event(1, 'a/b/c'), subtree)).toBe(true);
  });

  it('never matches a parent of a non-root scope, not even the root', () => {
    expect(fsChangeAffects(scope(1, 'a'), event(1, ''), subtree)).toBe(false);
  });

  it('still honours the volume and an empty event', () => {
    expect(fsChangeAffects(scope(1, 'a'), event(2, 'a/b'), subtree)).toBe(false);
    expect(fsChangeAffects(scope(null, 'a'), event(2, 'a/b'), subtree)).toBe(true);
    expect(fsChangeAffects(scope(1, ''), event(1), subtree)).toBe(false);
  });

  it('is off by default and when false', () => {
    expect(fsChangeAffects(scope(1, 'a'), event(1, 'a/b'))).toBe(false);
    expect(fsChangeAffects(scope(1, 'a'), event(1, 'a/b'), { subtree: false })).toBe(false);
    expect(fsChangeAffects(scope(1, 'a'), event(1, 'a/b'), {})).toBe(false);
  });
});
