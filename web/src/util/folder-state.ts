import type { FolderEntry } from '@/api/generated/FolderEntry';

export type FolderState = 'show' | 'grey' | 'hide';

/**
 * Nothing in the folder matches: no item passes the filter (`count === 0`), or, on the comparison
 * side of a plan tool, nothing matches the source (`matches === 0`). `matches` is `null` outside a
 * plan and then never counts.
 */
export function isUnmatched(entry: Pick<FolderEntry, 'count' | 'matches'>): boolean {
  return entry.count === 0 || entry.matches === 0;
}

/**
 * How the folder list shows a folder. An unmatched folder is `hide` when the user chose to hide
 * those and `grey` (visible and clickable) otherwise. A folder the plan creates (`overlay.planned`)
 * or deletes (`overlay.will_delete`) is part of the plan the user is reviewing: it is never hidden,
 * so it is greyed at most.
 */
export function folderState(entry: FolderEntry, hideUnmatched: boolean): FolderState {
  if (!isUnmatched(entry)) return 'show';
  if (hideUnmatched && !entry.overlay.planned && !entry.overlay.will_delete) return 'hide';
  return 'grey';
}

export interface EmptyTextInput {
  folders: readonly FolderEntry[];
  hideUnmatched: boolean;
  /** The filter narrows the listing (the tool view's filter bar says so). */
  filterActive: boolean;
  /** The volumes root: its folders are volumes. */
  atRoot?: boolean;
  /** False until the folder list has answered (or after it failed); defaults to true. */
  foldersKnown?: boolean;
  /** Folders that remain navigable even when unmatched folders are hidden. */
  keepVisible?: (entry: FolderEntry) => boolean;
}

/**
 * What to say when a folder has no files to show, from what its folder list shows. The file grid
 * cannot tell why it is empty, the folder list can: a folder that only holds subfolders is not an
 * empty one, and a folder whose content the filters hide is not either. While the folder list is
 * unknown (`foldersKnown: false`) only the files are talked about, so the text never claims that
 * the folder is empty and then has to take it back.
 */
export function emptyTextFor(input: EmptyTextInput): string {
  const { folders, hideUnmatched, filterActive, atRoot = false, foldersKnown = true } = input;
  if (!foldersKnown) return filterActive ? 'No files match the filters' : 'No files in this folder';
  const hidden = folders.filter(
    (entry) => !input.keepVisible?.(entry) && folderState(entry, hideUnmatched) === 'hide',
  );
  const hasVisibleFolders = hidden.length < folders.length;
  if (hasVisibleFolders)
    return filterActive ? 'No files match the filters' : 'No files in this folder';
  if (filterActive) return 'No items match the filters';
  if (hidden.some((entry) => entry.total > 0)) {
    return 'Nothing to show: the folders without matches are hidden';
  }
  return atRoot ? 'No volumes are available' : 'This folder is empty';
}
