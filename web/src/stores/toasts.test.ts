import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  clearToasts,
  DEFAULT_TIMEOUT_MS,
  dismissToast,
  holdToast,
  MAX_TOASTS,
  pushToast,
  releaseToast,
  toasts,
} from '@/stores/toasts';

beforeEach(() => {
  vi.useFakeTimers();
  clearToasts();
});

afterEach(() => {
  clearToasts();
  vi.useRealTimers();
});

describe('pushToast', () => {
  it('shows the toast and returns its id', () => {
    const id = pushToast('success', 'Saved');

    expect(toasts.value).toEqual([{ id, kind: 'success', message: 'Saved', timeout: 4000 }]);
  });

  it('keeps toasts in the order they were pushed, with unique ids', () => {
    const a = pushToast('info', 'a');
    const b = pushToast('info', 'b');

    expect(a).not.toBe(b);
    expect(toasts.value.map((toast) => toast.message)).toEqual(['a', 'b']);
  });

  it.each(['success', 'info', 'warning', 'error'] as const)(
    'removes a %s toast after its default time',
    (kind) => {
      pushToast(kind, 'x');

      vi.advanceTimersByTime(DEFAULT_TIMEOUT_MS[kind] - 1);
      expect(toasts.value).toHaveLength(1);

      vi.advanceTimersByTime(1);
      expect(toasts.value).toHaveLength(0);
    },
  );

  it('keeps errors on screen longer than successes', () => {
    expect(DEFAULT_TIMEOUT_MS.error).toBeGreaterThan(DEFAULT_TIMEOUT_MS.success);
  });

  it('honours an explicit timeout', () => {
    pushToast('info', 'quick', { timeout: 500 });

    vi.advanceTimersByTime(500);

    expect(toasts.value).toHaveLength(0);
  });

  it('keeps a toast with timeout 0 until it is dismissed', () => {
    const id = pushToast('error', 'sticky', { timeout: 0 });

    vi.advanceTimersByTime(10 * 60 * 1000);
    expect(toasts.value).toHaveLength(1);

    dismissToast(id);
    expect(toasts.value).toHaveLength(0);
  });

  it('treats a negative timeout as sticky rather than throwing', () => {
    pushToast('info', 'odd', { timeout: -5 });

    vi.advanceTimersByTime(60_000);

    expect(toasts.value).toHaveLength(1);
  });

  it('does not stack an identical toast: it restarts the timer instead', () => {
    const first = pushToast('error', 'Disk full');
    vi.advanceTimersByTime(8000);

    const second = pushToast('error', 'Disk full');
    vi.advanceTimersByTime(8000);

    expect(second).toBe(first);
    expect(toasts.value).toHaveLength(1);

    vi.advanceTimersByTime(2000);
    expect(toasts.value).toHaveLength(0);
  });

  it('stacks toasts that differ in kind or text', () => {
    pushToast('info', 'same');
    pushToast('error', 'same');
    pushToast('info', 'other');

    expect(toasts.value).toHaveLength(3);
  });

  it(`drops the oldest beyond ${MAX_TOASTS} and cancels its timer`, () => {
    const ids = Array.from({ length: MAX_TOASTS + 2 }, (_, index) =>
      pushToast('info', `toast ${index}`),
    );

    expect(toasts.value).toHaveLength(MAX_TOASTS);
    expect(toasts.value.map((toast) => toast.id)).toEqual(ids.slice(2));
    expect(vi.getTimerCount()).toBe(MAX_TOASTS);
  });
});

describe('dismissToast and clearToasts', () => {
  it('dismisses one toast and cancels its timer', () => {
    const keep = pushToast('info', 'keep');
    const drop = pushToast('info', 'drop');

    dismissToast(drop);

    expect(toasts.value.map((toast) => toast.id)).toEqual([keep]);
    expect(vi.getTimerCount()).toBe(1);
  });

  it('ignores an unknown id without notifying', () => {
    pushToast('info', 'a');
    const before = toasts.value;

    dismissToast(9999);

    expect(toasts.value).toBe(before);
  });

  it('clears everything', () => {
    pushToast('info', 'a');
    pushToast('error', 'b', { timeout: 0 });

    clearToasts();

    expect(toasts.value).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe('holdToast and releaseToast', () => {
  it('pauses the timer while held and restarts the full time on release', () => {
    const id = pushToast('info', 'reading');

    vi.advanceTimersByTime(3000);
    holdToast(id);
    vi.advanceTimersByTime(60_000);
    expect(toasts.value).toHaveLength(1);

    releaseToast(id);
    vi.advanceTimersByTime(3999);
    expect(toasts.value).toHaveLength(1);

    vi.advanceTimersByTime(1);
    expect(toasts.value).toHaveLength(0);
  });

  it('releasing a sticky toast does not start a timer', () => {
    const id = pushToast('error', 'sticky', { timeout: 0 });

    holdToast(id);
    releaseToast(id);

    expect(vi.getTimerCount()).toBe(0);
    expect(toasts.value).toHaveLength(1);
  });

  it('ignores a toast that is already gone', () => {
    const id = pushToast('info', 'gone');
    dismissToast(id);

    releaseToast(id);
    holdToast(id);

    expect(vi.getTimerCount()).toBe(0);
  });

  it('does not start a timer when an identical toast is pushed while held', () => {
    const id = pushToast('warning', 'Careful');
    holdToast(id);

    pushToast('warning', 'Careful');
    vi.advanceTimersByTime(60_000);

    expect(toasts.value).toHaveLength(1);
    expect(vi.getTimerCount()).toBe(0);
  });
});
