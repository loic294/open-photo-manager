import type { EventSourceLike } from '@/api/sse';

/**
 * Stand-in for `EventSource` in tests. Pass `(url) => new FakeEventSource(url)` as the `factory`
 * of `createEventsClient`, or `vi.stubGlobal('EventSource', FakeEventSource)` to drive the shared
 * `events` client (call `FakeEventSource.reset()` in `beforeEach`).
 */
export class FakeEventSource implements EventSourceLike {
  static instances: FakeEventSource[] = [];

  static reset() {
    FakeEventSource.instances = [];
  }

  /** The most recently created source. */
  static last(): FakeEventSource {
    const last = FakeEventSource.instances.at(-1);
    if (!last) throw new Error('FakeEventSource: no source has been created yet');
    return last;
  }

  readonly url: string;
  closed = false;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }

  close() {
    this.closed = true;
  }

  /** Simulates the connection being established. */
  open() {
    this.onopen?.(new Event('open'));
  }

  /** Simulates a message; non-strings are JSON-encoded like the server does. */
  message(data: unknown) {
    const text = typeof data === 'string' ? data : JSON.stringify(data);
    this.onmessage?.(new MessageEvent<string>('message', { data: text }));
  }

  /** Simulates a connection failure. */
  error() {
    this.onerror?.(new Event('error'));
  }
}
