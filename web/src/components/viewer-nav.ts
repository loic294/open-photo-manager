import type { FileItem } from '@/api/generated/FileItem';
import type { FileKind } from '@/api/generated/FileKind';

/**
 * The largest image whose `/preview` is preloaded for the next file. Its preview is the original
 * for browser-native formats, so a bigger one is not fetched until the user gets there: the NAS is
 * slow and the user may be moving on.
 */
export const PRELOAD_MAX_BYTES = 32 * 1024 * 1024;

/** While the user steps through files, a file's detail is asked for only after this pause. */
export const DETAIL_DEBOUNCE_MS = 120;

/**
 * Pointer actions in the viewer are ignored for this long after it opens (or its source is
 * replaced): the grid opens it on the first click of a double click, so the second click and the
 * `dblclick` land on the viewer and must not zoom or click anything.
 */
export const OPEN_GUARD_MS = 400;

export type NavAction = 'first' | 'last' | 'next' | 'previous';
export type ViewerKeyAction = NavAction | 'info' | 'zoom';

/** `1` moves towards the end of the listing, `-1` towards its start. */
export type Direction = 1 | -1;

type KeyInfo = Pick<KeyboardEvent, 'altKey' | 'ctrlKey' | 'key' | 'metaKey' | 'repeat'>;

/**
 * What a key press asks of the viewer, or `null` for any other key. Cmd, Ctrl and Alt combinations
 * belong to the browser and the app. Toggles ignore auto-repeat so holding the key does not
 * flicker the panel.
 */
export function viewerKeyAction(event: KeyInfo): ViewerKeyAction | null {
  if (event.ctrlKey || event.metaKey || event.altKey) return null;
  switch (event.key) {
    case 'ArrowLeft':
      return 'previous';
    case 'ArrowRight':
      return 'next';
    case 'Home':
      return 'first';
    case 'End':
      return 'last';
    default:
      break;
  }
  if (event.repeat) return null;
  switch (event.key.toLowerCase()) {
    case 'i':
      return 'info';
    case 'z':
      return 'zoom';
    default:
      return null;
  }
}

export function isNavAction(action: ViewerKeyAction | null): action is NavAction {
  return action === 'first' || action === 'last' || action === 'next' || action === 'previous';
}

/** True inside a `video` or `audio` element, where the arrow keys seek and Home and End jump. */
export function isInMedia(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest('video, audio') !== null;
}

/** The index `action` leads to, or `null` when it does not move: the ends do not wrap around. */
export function targetIndex(action: NavAction, index: number, count: number): number | null {
  if (count <= 0 || index < 0 || index >= count) return null;
  const target = {
    first: 0,
    last: count - 1,
    next: index + 1,
    previous: index - 1,
  }[action];
  return target >= 0 && target < count && target !== index ? target : null;
}

/**
 * Where the user is most likely to go next after `action`: on from a step, and, after a jump to
 * the first file, forward through the listing, after a jump to the last, back through it.
 */
export function directionAfter(action: NavAction): Direction {
  return action === 'previous' || action === 'last' ? -1 : 1;
}

/** The neighbour of `index` in `direction`, or `null` past either end. */
export function neighbourIndex(index: number, direction: Direction, count: number): number | null {
  const target = index + direction;
  return index >= 0 && target >= 0 && target < count ? target : null;
}

/** Whether `/preview` of this file is worth fetching before the user asks for it. */
export function shouldPreloadPreview(item: Pick<FileItem, 'kind' | 'size'>): boolean {
  return item.kind === 'image' && item.size <= PRELOAD_MAX_BYTES;
}

/** "12 / 340": the position of the file shown. */
export function positionLabel(index: number, count: number): string {
  return `${index + 1} / ${count}`;
}

const NOUN: Record<FileKind, string> = {
  image: 'Photo',
  video: 'Video',
  audio: 'File',
  sidecar: 'File',
  other: 'File',
};

/** The screen reader text for a file that was just shown: "Photo 12 of 340: IMG_0012.jpg". */
export function announcement(
  item: Pick<FileItem, 'kind' | 'name'>,
  position: { index: number; count: number } | null,
): string {
  const noun = NOUN[item.kind];
  return position
    ? `${noun} ${position.index + 1} of ${position.count}: ${item.name}`
    : `${noun}: ${item.name}`;
}
