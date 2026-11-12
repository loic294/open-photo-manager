import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { onReindex, REINDEX_EVENT, requestReindex } from '@/app/app-actions';
import { clearToasts, toasts } from '@/stores/toasts';

beforeEach(() => {
  clearToasts();
});

afterEach(() => {
  clearToasts();
  vi.restoreAllMocks();
});

describe('requestReindex', () => {
  it('tells the user that re-indexing is not available yet when nothing handles it', () => {
    requestReindex('all');

    expect(toasts.value).toHaveLength(1);
    expect(toasts.value[0]).toMatchObject({
      kind: 'info',
      message: 'Re-indexing is not available yet.',
    });
  });

  it('dispatches a cancelable opm-reindex event on the document with the scope', () => {
    const seen: CustomEvent<{ scope: string }>[] = [];
    const listener = (event: CustomEvent<{ scope: string }>) => seen.push(event);
    document.addEventListener(REINDEX_EVENT, listener);

    requestReindex('current');
    document.removeEventListener(REINDEX_EVENT, listener);

    expect(seen).toHaveLength(1);
    expect(seen[0]?.type).toBe('opm-reindex');
    expect(seen[0]?.detail).toEqual({ scope: 'current' });
    expect(seen[0]?.cancelable).toBe(true);
  });

  it('shows no toast when a listener calls preventDefault', () => {
    const listener = (event: Event) => event.preventDefault();
    document.addEventListener(REINDEX_EVENT, listener);

    requestReindex('all');
    document.removeEventListener(REINDEX_EVENT, listener);

    expect(toasts.value).toHaveLength(0);
  });
});

describe('onReindex', () => {
  it('receives the scope of each request and marks it handled', () => {
    const handler = vi.fn();
    const off = onReindex(handler);

    requestReindex('current');
    requestReindex('all');
    off();

    expect(handler.mock.calls).toEqual([['current'], ['all']]);
    expect(toasts.value).toHaveLength(0);
  });

  it('stops receiving requests once unregistered, and the fallback toast returns', () => {
    const handler = vi.fn();
    onReindex(handler)();

    requestReindex('all');

    expect(handler).not.toHaveBeenCalled();
    expect(toasts.value).toHaveLength(1);
  });
});
