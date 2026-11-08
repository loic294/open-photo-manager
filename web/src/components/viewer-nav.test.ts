import { describe, expect, it } from 'vitest';
import {
  announcement,
  DETAIL_DEBOUNCE_MS,
  directionAfter,
  isInMedia,
  isNavAction,
  neighbourIndex,
  OPEN_GUARD_MS,
  PRELOAD_MAX_BYTES,
  positionLabel,
  shouldPreloadPreview,
  targetIndex,
  viewerKeyAction,
} from '@/components/viewer-nav';

const key = (name: string, modifiers: Partial<KeyboardEvent> = {}) => ({
  key: name,
  altKey: false,
  ctrlKey: false,
  metaKey: false,
  repeat: false,
  ...modifiers,
});

describe('constants', () => {
  it('are the documented numbers', () => {
    expect(PRELOAD_MAX_BYTES).toBe(32 * 1024 * 1024);
    expect(DETAIL_DEBOUNCE_MS).toBe(120);
    expect(OPEN_GUARD_MS).toBe(400);
  });
});

describe('viewerKeyAction', () => {
  it.each([
    ['ArrowLeft', 'previous'],
    ['ArrowRight', 'next'],
    ['Home', 'first'],
    ['End', 'last'],
    ['i', 'info'],
    ['I', 'info'],
    ['z', 'zoom'],
    ['Z', 'zoom'],
  ])('maps %s to %s', (name, action) => {
    expect(viewerKeyAction(key(name))).toBe(action);
  });

  it.each(['ArrowLeft', 'ArrowRight', 'Home', 'End', 'i', 'z'])(
    'ignores %s with Cmd, Ctrl or Alt held',
    (name) => {
      expect(viewerKeyAction(key(name, { metaKey: true }))).toBeNull();
      expect(viewerKeyAction(key(name, { ctrlKey: true }))).toBeNull();
      expect(viewerKeyAction(key(name, { altKey: true }))).toBeNull();
    },
  );

  it('keeps navigating while an arrow key repeats, but toggles once per press', () => {
    expect(viewerKeyAction(key('ArrowRight', { repeat: true }))).toBe('next');
    expect(viewerKeyAction(key('End', { repeat: true }))).toBe('last');
    expect(viewerKeyAction(key('i', { repeat: true }))).toBeNull();
    expect(viewerKeyAction(key('z', { repeat: true }))).toBeNull();
  });

  it('lets Shift through', () => {
    expect(viewerKeyAction(key('ArrowRight', { shiftKey: true }))).toBe('next');
  });

  it.each(['a', 'Enter', ' ', 'Escape', 'Tab', 'ArrowUp', 'ArrowDown', 'PageDown'])(
    'does not claim %s',
    (name) => {
      expect(viewerKeyAction(key(name))).toBeNull();
    },
  );
});

describe('isNavAction', () => {
  it('is true for the four moves only', () => {
    expect(isNavAction('first')).toBe(true);
    expect(isNavAction('last')).toBe(true);
    expect(isNavAction('next')).toBe(true);
    expect(isNavAction('previous')).toBe(true);
    expect(isNavAction('info')).toBe(false);
    expect(isNavAction('zoom')).toBe(false);
    expect(isNavAction(null)).toBe(false);
  });
});

describe('isInMedia', () => {
  it('recognises a video, an audio and what is inside them', () => {
    const video = document.createElement('video');
    const audio = document.createElement('audio');
    const source = document.createElement('source');
    audio.append(source);
    expect(isInMedia(video)).toBe(true);
    expect(isInMedia(audio)).toBe(true);
    expect(isInMedia(source)).toBe(true);
  });

  it('is false elsewhere', () => {
    expect(isInMedia(document.createElement('button'))).toBe(false);
    expect(isInMedia(document.createElement('img'))).toBe(false);
    expect(isInMedia(window)).toBe(false);
    expect(isInMedia(null)).toBe(false);
  });
});

describe('targetIndex', () => {
  it('steps by one', () => {
    expect(targetIndex('next', 3, 10)).toBe(4);
    expect(targetIndex('previous', 3, 10)).toBe(2);
  });

  it('jumps to the ends', () => {
    expect(targetIndex('first', 3, 10)).toBe(0);
    expect(targetIndex('last', 3, 10)).toBe(9);
  });

  it('does not wrap around and does not move to where it already is', () => {
    expect(targetIndex('next', 9, 10)).toBeNull();
    expect(targetIndex('previous', 0, 10)).toBeNull();
    expect(targetIndex('first', 0, 10)).toBeNull();
    expect(targetIndex('last', 9, 10)).toBeNull();
  });

  it('goes nowhere without a usable listing', () => {
    expect(targetIndex('next', 0, 0)).toBeNull();
    expect(targetIndex('next', -1, 5)).toBeNull();
    expect(targetIndex('first', 7, 5)).toBeNull();
    expect(targetIndex('next', 0, 1)).toBeNull();
  });
});

describe('directionAfter', () => {
  it('follows the step, and reads on from a jump to either end', () => {
    expect(directionAfter('next')).toBe(1);
    expect(directionAfter('previous')).toBe(-1);
    expect(directionAfter('first')).toBe(1);
    expect(directionAfter('last')).toBe(-1);
  });
});

describe('neighbourIndex', () => {
  it('is the file beside it in the direction of travel', () => {
    expect(neighbourIndex(4, 1, 10)).toBe(5);
    expect(neighbourIndex(4, -1, 10)).toBe(3);
  });

  it('is null past either end or without a position', () => {
    expect(neighbourIndex(9, 1, 10)).toBeNull();
    expect(neighbourIndex(0, -1, 10)).toBeNull();
    expect(neighbourIndex(-1, 1, 10)).toBeNull();
    expect(neighbourIndex(0, 1, 0)).toBeNull();
  });
});

describe('shouldPreloadPreview', () => {
  it('accepts an image up to the cap, inclusive', () => {
    expect(shouldPreloadPreview({ kind: 'image', size: 1 })).toBe(true);
    expect(shouldPreloadPreview({ kind: 'image', size: PRELOAD_MAX_BYTES })).toBe(true);
  });

  it('refuses a larger image', () => {
    expect(shouldPreloadPreview({ kind: 'image', size: PRELOAD_MAX_BYTES + 1 })).toBe(false);
  });

  it.each(['video', 'audio', 'sidecar', 'other'] as const)('never preloads a %s', (kind) => {
    expect(shouldPreloadPreview({ kind, size: 10 })).toBe(false);
  });
});

describe('positionLabel and announcement', () => {
  it('count from one', () => {
    expect(positionLabel(0, 340)).toBe('1 / 340');
    expect(positionLabel(11, 340)).toBe('12 / 340');
  });

  it('name the kind and the position', () => {
    const position = { index: 11, count: 340 };
    expect(announcement({ kind: 'image', name: 'IMG_1.jpg' }, position)).toBe(
      'Photo 12 of 340: IMG_1.jpg',
    );
    expect(announcement({ kind: 'video', name: 'a.mp4' }, position)).toBe('Video 12 of 340: a.mp4');
    for (const kind of ['audio', 'sidecar', 'other'] as const) {
      expect(announcement({ kind, name: 'x' }, position)).toBe('File 12 of 340: x');
    }
  });

  it('drops the counter without navigation', () => {
    expect(announcement({ kind: 'image', name: 'IMG_1.jpg' }, null)).toBe('Photo: IMG_1.jpg');
  });
});
