import { describe, expect, it } from 'vitest';
import { folderEntry } from '@/test/folder-entry';
import { emptyTextFor, folderState, isUnmatched } from '@/util/folder-state';

describe('isUnmatched', () => {
  it.each([
    ['items below, no plan', { count: 4, matches: null }, false],
    ['nothing passes the filter', { count: 0, matches: null }, true],
    ['nothing passes the filter, plan matches', { count: 0, matches: 3 }, true],
    ['no match with the source', { count: 4, matches: 0 }, true],
    ['matches with the source', { count: 4, matches: 2 }, false],
    ['empty and no match', { count: 0, matches: 0 }, true],
  ])('%s', (_name, entry, expected) => {
    expect(isUnmatched(entry)).toBe(expected);
  });
});

describe('folderState', () => {
  it.each([
    ['matching folder', { count: 4, matches: null }, false, 'show'],
    ['matching folder, hiding on', { count: 4, matches: null }, true, 'show'],
    ['empty by the filter', { count: 0 }, false, 'grey'],
    ['empty by the filter, hiding on', { count: 0 }, true, 'hide'],
    ['matches is null, never counted', { count: 2, matches: null }, true, 'show'],
    ['matches is 0, not null', { count: 2, matches: 0 }, false, 'grey'],
    ['matches is 0, hiding on', { count: 2, matches: 0 }, true, 'hide'],
    ['matches above 0', { count: 2, matches: 1 }, true, 'show'],
  ])('%s', (_name, fields, hideUnmatched, expected) => {
    expect(folderState(folderEntry(fields), hideUnmatched)).toBe(expected);
  });

  it('never hides a folder the plan creates, it greys it at most', () => {
    const planned = folderEntry({ count: 0, total: 0, overlay: { planned: true } });

    expect(folderState(planned, true)).toBe('grey');
    expect(folderState(planned, false)).toBe('grey');
    expect(folderState({ ...planned, count: 2, matches: null }, true)).toBe('show');
  });

  it('never hides a folder the plan deletes, it greys it at most', () => {
    const doomed = folderEntry({ count: 0, total: 0, matches: 0, overlay: { will_delete: true } });

    expect(folderState(doomed, true)).toBe('grey');
    expect(folderState(doomed, false)).toBe('grey');
  });

  it('ignores the matched flag, which a folder row does not use', () => {
    expect(folderState(folderEntry({ count: 0, overlay: { matched: true } }), true)).toBe('hide');
  });

  it.each(['Été', 'ré sumé', '.hidden', 'a.b.c', '日本語'])(
    'does not look at the name %s',
    (name) => {
      expect(folderState(folderEntry({ name, count: 0 }), true)).toBe('hide');
      expect(folderState(folderEntry({ name, count: 1 }), true)).toBe('show');
    },
  );
});

describe('emptyTextFor', () => {
  const base = { hideUnmatched: false, filterActive: false };

  it('says so when a folder only holds folders', () => {
    expect(emptyTextFor({ ...base, folders: [folderEntry()] })).toBe('No files in this folder');
  });

  it('blames the filters when they are on and the folders are still listed', () => {
    expect(
      emptyTextFor({ ...base, filterActive: true, folders: [folderEntry({ count: 0 })] }),
    ).toBe('No files match the filters');
  });

  it('says the folder is empty when there is nothing at all', () => {
    expect(emptyTextFor({ ...base, folders: [] })).toBe('This folder is empty');
  });

  it('blames the filters when nothing is listed and they are on', () => {
    expect(emptyTextFor({ ...base, filterActive: true, folders: [] })).toBe(
      'No items match the filters',
    );
  });

  it('tells a hidden folder with files from an empty one', () => {
    const hidden = folderEntry({ count: 2, total: 2, matches: 0 });

    expect(emptyTextFor({ ...base, hideUnmatched: true, folders: [hidden] })).toBe(
      'Nothing to show: the folders without matches are hidden',
    );
  });

  it('calls a folder of hidden empty folders empty', () => {
    const empty = folderEntry({ count: 0, total: 0 });

    expect(emptyTextFor({ ...base, hideUnmatched: true, folders: [empty] })).toBe(
      'This folder is empty',
    );
  });

  it('counts a greyed folder as listed', () => {
    const grey = folderEntry({ count: 0, total: 5 });

    expect(emptyTextFor({ ...base, folders: [grey] })).toBe('No files in this folder');
  });

  it('only talks about files while the folders are not known', () => {
    const unknown = { ...base, folders: [], foldersKnown: false };

    expect(emptyTextFor(unknown)).toBe('No files in this folder');
    expect(emptyTextFor({ ...unknown, filterActive: true })).toBe('No files match the filters');
  });

  it('has its own words at the volumes root', () => {
    expect(emptyTextFor({ ...base, atRoot: true, folders: [] })).toBe('No volumes are available');
    expect(emptyTextFor({ ...base, atRoot: true, filterActive: true, folders: [] })).toBe(
      'No items match the filters',
    );
  });
});
