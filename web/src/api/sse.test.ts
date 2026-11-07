import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { TaskDto } from '@/api/generated/TaskDto';
import { createEventsClient, type EventsClient, SERVER_RETRY_MS } from '@/api/sse';
import { FakeEventSource } from '@/test/fake-event-source';

const task: TaskDto = {
  id: 7,
  kind: 'fs_move',
  title: 'Move 3 files',
  state: 'running',
  items_done: 1,
  items_total: 3,
  bytes_done: 10,
  bytes_total: 30,
  speed: null,
  eta_secs: null,
  error: null,
  started_at: 1,
  finished_at: null,
};

function client(overrides: Parameters<typeof createEventsClient>[0] = {}): EventsClient {
  return createEventsClient({
    factory: (url) => new FakeEventSource(url),
    random: () => 0,
    ...overrides,
  });
}

let active: EventsClient | undefined;

function started(overrides?: Parameters<typeof createEventsClient>[0]): EventsClient {
  active = client(overrides);
  active.start();
  return active;
}

beforeEach(() => {
  FakeEventSource.reset();
  vi.useFakeTimers();
});

afterEach(() => {
  active?.stop();
  active = undefined;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('connection lifecycle', () => {
  it('is closed until started, then connecting, then open', () => {
    const events = client();
    expect(events.connection.value).toBe('closed');

    events.start();
    active = events;
    expect(events.connection.value).toBe('connecting');
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(FakeEventSource.last().url).toBe('/api/events');

    FakeEventSource.last().open();
    expect(events.connection.value).toBe('open');
  });

  it('start() is idempotent: one EventSource for the whole app', () => {
    const events = started();
    events.start();
    events.start();
    expect(FakeEventSource.instances).toHaveLength(1);
  });

  it('stop() closes the source, reports closed and stops retrying', () => {
    const events = started();
    const source = FakeEventSource.last();
    source.open();

    events.stop();

    expect(source.closed).toBe(true);
    expect(events.connection.value).toBe('closed');
    vi.advanceTimersByTime(120_000);
    expect(FakeEventSource.instances).toHaveLength(1);
  });

  it('does not fire resync for a clean first open', () => {
    const events = started();
    const resync = vi.fn();
    events.onResync(resync);

    FakeEventSource.last().open();

    expect(resync).not.toHaveBeenCalled();
  });
});

describe('events', () => {
  it('delivers each event only to the handlers of its type, narrowed', () => {
    const events = started();
    const onTask = vi.fn();
    const onFs = vi.fn();
    const onIndex = vi.fn();
    events.on('task', onTask);
    events.on('fs_change', onFs);
    events.on('index', onIndex);
    const source = FakeEventSource.last();
    source.open();

    source.message({ type: 'task', task });
    source.message({ type: 'fs_change', volume_id: 2, dirs: ['a', 'b/c'] });
    source.message({ type: 'index', state: 'scanning', files_total: 10, files_done: 4 });

    expect(onTask).toHaveBeenCalledExactlyOnceWith({ type: 'task', task });
    expect(onFs).toHaveBeenCalledExactlyOnceWith({
      type: 'fs_change',
      volume_id: 2,
      dirs: ['a', 'b/c'],
    });
    expect(onIndex).toHaveBeenCalledExactlyOnceWith({
      type: 'index',
      state: 'scanning',
      files_total: 10,
      files_done: 4,
    });
  });

  it('notifies every subscriber and stops after unsubscribe', () => {
    const events = started();
    const first = vi.fn();
    const second = vi.fn();
    const off = events.on('index', first);
    events.on('index', second);
    const source = FakeEventSource.last();
    const message = { type: 'index', state: 'idle', files_total: 0, files_done: 0 };

    source.message(message);
    off();
    source.message(message);

    expect(first).toHaveBeenCalledOnce();
    expect(second).toHaveBeenCalledTimes(2);
  });

  it('treats the same handler subscribed twice as two independent subscriptions', () => {
    const events = client();
    const handler = vi.fn();
    const offFirst = events.on('index', handler);
    events.on('index', handler);
    offFirst();

    events.emit({ type: 'index', state: 'idle', files_total: 0, files_done: 0 });

    expect(handler).toHaveBeenCalledOnce();
  });

  it('isolates a throwing handler from the others', () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    const events = client();
    const after = vi.fn();
    events.on('index', () => {
      throw new Error('boom');
    });
    events.on('index', after);

    events.emit({ type: 'index', state: 'idle', files_total: 0, files_done: 0 });

    expect(after).toHaveBeenCalledOnce();
    expect(error).toHaveBeenCalledOnce();
  });

  it('lets a handler unsubscribe itself while being dispatched', () => {
    const events = client();
    const calls: string[] = [];
    const offA = events.on('index', () => {
      calls.push('a');
      offA();
    });
    events.on('index', () => calls.push('b'));

    events.emit({ type: 'index', state: 'idle', files_total: 0, files_done: 0 });
    events.emit({ type: 'index', state: 'idle', files_total: 0, files_done: 0 });

    expect(calls).toEqual(['a', 'b', 'b']);
  });

  it('emit() delivers without a connection', () => {
    const events = client();
    const handler = vi.fn();
    events.on('fs_change', handler);

    events.emit({ type: 'fs_change', volume_id: 1, dirs: [] });

    expect(handler).toHaveBeenCalledOnce();
  });
});

describe('malformed messages', () => {
  it('drops them with a console warning and keeps working', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const events = started();
    const handler = vi.fn();
    events.on('task', handler);
    events.on('index', handler);
    const source = FakeEventSource.last();
    source.open();

    source.message('not json {');
    source.message('42');
    source.message({ type: 'task' });
    source.message({ type: 'fs_change', volume_id: 'x', dirs: [] });
    source.message({ type: 'index', state: 'scanning' });
    source.message({ nothing: true });

    expect(handler).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledTimes(6);
    expect(events.connection.value).toBe('open');

    source.message({ type: 'index', state: 'idle', files_total: 1, files_done: 1 });
    expect(handler).toHaveBeenCalledOnce();
  });

  it('ignores well-formed events of an unknown type without warning', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const events = started();
    const handler = vi.fn();
    events.on('task', handler);

    FakeEventSource.last().message({ type: 'from_the_future', payload: 1 });

    expect(handler).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
  });
});

describe('reconnect', () => {
  it('closes the failed source, reports reconnecting and retries after the server retry delay', () => {
    const events = started();
    const first = FakeEventSource.last();
    first.open();

    first.error();

    expect(first.closed).toBe(true);
    expect(events.connection.value).toBe('reconnecting');
    vi.advanceTimersByTime(SERVER_RETRY_MS - 1);
    expect(FakeEventSource.instances).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeEventSource.instances).toHaveLength(2);
    expect(events.connection.value).toBe('reconnecting');

    FakeEventSource.last().open();
    expect(events.connection.value).toBe('open');
  });

  it('fires resync once per re-connect, not for the first open', () => {
    const events = started();
    const resync = vi.fn();
    events.onResync(resync);
    FakeEventSource.last().open();
    expect(resync).not.toHaveBeenCalled();

    FakeEventSource.last().error();
    vi.advanceTimersByTime(SERVER_RETRY_MS);
    FakeEventSource.last().open();
    expect(resync).toHaveBeenCalledTimes(1);

    FakeEventSource.last().error();
    vi.advanceTimersByTime(SERVER_RETRY_MS);
    FakeEventSource.last().open();
    expect(resync).toHaveBeenCalledTimes(2);
  });

  it('fires resync when the very first attempts failed and a later one succeeds', () => {
    const events = started();
    const resync = vi.fn();
    events.onResync(resync);

    FakeEventSource.last().error();
    vi.advanceTimersByTime(SERVER_RETRY_MS);
    FakeEventSource.last().error();
    vi.advanceTimersByTime(2 * SERVER_RETRY_MS);
    FakeEventSource.last().open();

    expect(resync).toHaveBeenCalledOnce();
  });

  it('backs off exponentially up to the cap and never below the server retry', () => {
    const events = started({ maxRetryMs: 20_000 });
    const delays: number[] = [];
    for (let index = 0; index < 5; index += 1) {
      const before = FakeEventSource.instances.length;
      FakeEventSource.last().error();
      let waited = 0;
      while (FakeEventSource.instances.length === before) {
        vi.advanceTimersByTime(500);
        waited += 500;
      }
      delays.push(waited);
    }

    expect(events.connection.value).toBe('reconnecting');
    expect(delays).toEqual([3000, 6000, 12_000, 20_000, 20_000]);
  });

  it('adds jitter on top of the backoff but never exceeds the cap', () => {
    const events = started({ random: () => 0.8, maxRetryMs: 7000 });
    FakeEventSource.last().error();
    vi.advanceTimersByTime(3599);
    expect(FakeEventSource.instances).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeEventSource.instances).toHaveLength(2);

    FakeEventSource.last().error();
    vi.advanceTimersByTime(6999);
    expect(FakeEventSource.instances).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(FakeEventSource.instances).toHaveLength(3);
    expect(events.connection.value).toBe('reconnecting');
  });

  it('resets the backoff after a successful open', () => {
    started();
    FakeEventSource.last().error();
    vi.advanceTimersByTime(SERVER_RETRY_MS);
    FakeEventSource.last().error();
    vi.advanceTimersByTime(2 * SERVER_RETRY_MS);
    FakeEventSource.last().open();

    FakeEventSource.last().error();
    vi.advanceTimersByTime(SERVER_RETRY_MS);

    expect(FakeEventSource.instances).toHaveLength(4);
  });

  it('ignores callbacks of a source it already replaced', () => {
    const events = started();
    const stale = FakeEventSource.last();
    stale.error();
    vi.advanceTimersByTime(SERVER_RETRY_MS);
    const resync = vi.fn();
    events.onResync(resync);
    const received = vi.fn();
    events.on('index', received);

    stale.open();
    stale.message({ type: 'index', state: 'idle', files_total: 0, files_done: 0 });
    stale.error();

    expect(resync).not.toHaveBeenCalled();
    expect(received).not.toHaveBeenCalled();
    expect(FakeEventSource.instances).toHaveLength(2);
    vi.advanceTimersByTime(60_000);
    expect(FakeEventSource.instances).toHaveLength(2);
  });

  it('keeps retrying when the factory throws', () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    let attempts = 0;
    const events = started({
      factory: (url) => {
        attempts += 1;
        if (attempts === 1) throw new Error('EventSource is not defined');
        return new FakeEventSource(url);
      },
    });
    expect(events.connection.value).toBe('reconnecting');

    vi.advanceTimersByTime(SERVER_RETRY_MS);

    expect(attempts).toBe(2);
    expect(FakeEventSource.instances).toHaveLength(1);
  });

  it('stop() during a pending retry cancels it; start() again resyncs on open', () => {
    const events = started();
    const resync = vi.fn();
    events.onResync(resync);
    FakeEventSource.last().error();

    events.stop();
    vi.advanceTimersByTime(60_000);
    expect(FakeEventSource.instances).toHaveLength(1);

    events.start();
    expect(events.connection.value).toBe('reconnecting');
    FakeEventSource.last().open();
    expect(resync).toHaveBeenCalledOnce();
  });

  it('reconnects immediately when the browser comes back online', () => {
    const events = started();
    FakeEventSource.last().error();
    vi.advanceTimersByTime(SERVER_RETRY_MS);
    FakeEventSource.last().error();
    expect(FakeEventSource.instances).toHaveLength(2);

    window.dispatchEvent(new Event('online'));

    expect(FakeEventSource.instances).toHaveLength(3);
    expect(events.connection.value).toBe('reconnecting');
    // The superseded timer must not open a fourth source.
    vi.advanceTimersByTime(60_000);
    expect(FakeEventSource.instances).toHaveLength(3);
  });

  it('reconnects immediately when the tab becomes visible while waiting', () => {
    started();
    FakeEventSource.last().error();

    document.dispatchEvent(new Event('visibilitychange'));

    expect(FakeEventSource.instances).toHaveLength(2);
  });

  it('does nothing on online / visibility changes while connected', () => {
    started();
    FakeEventSource.last().open();

    window.dispatchEvent(new Event('online'));
    document.dispatchEvent(new Event('visibilitychange'));

    expect(FakeEventSource.instances).toHaveLength(1);
  });

  it('removes its window listeners on stop()', () => {
    const events = started();
    FakeEventSource.last().error();
    events.stop();

    window.dispatchEvent(new Event('online'));

    expect(FakeEventSource.instances).toHaveLength(1);
  });
});
