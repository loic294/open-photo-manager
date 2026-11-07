import { type ReadonlySignal, signal } from '@preact/signals-core';
import type { ServerEvent } from '@/api/generated/ServerEvent';

/**
 * Client for `GET /api/events` (server-sent events). Wire format: data-only messages, each one
 * `ServerEvent` JSON tagged by `type`, no `event:` field and NO replay: whatever happens while
 * disconnected is lost, which is why every re-connect fires `onResync` so stores can refetch.
 *
 * The browser's own EventSource reconnect is not used: a non-200 answer (or a proxy dropping the
 * stream) leaves it permanently CLOSED. Instead every error closes the source and a new one is
 * opened after a capped exponential backoff (never below the server's `retry:` of 3 s).
 */

export type ConnectionState = 'connecting' | 'open' | 'reconnecting' | 'closed';
export type ServerEventType = ServerEvent['type'];
export type ServerEventOf<T extends ServerEventType> = Extract<ServerEvent, { type: T }>;
export type EventHandler<T extends ServerEventType> = (event: ServerEventOf<T>) => void;

/** The part of `EventSource` the client uses, so tests can inject a fake. */
export interface EventSourceLike {
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent<string>) => void) | null;
  onerror: ((event: Event) => void) | null;
  close(): void;
}

export interface EventsClientOptions {
  url?: string;
  factory?: (url: string) => EventSourceLike;
  /** Shortest reconnect delay; defaults to the server's `retry: 3000`. */
  minRetryMs?: number;
  maxRetryMs?: number;
  /** Jitter source in [0, 1); tests inject a constant. */
  random?: () => number;
}

export interface EventsClient {
  /** `connecting` (first attempt), `open`, `reconnecting` (after an error) or `closed` (stopped). */
  readonly connection: ReadonlySignal<ConnectionState>;
  /** Subscribe to one event type; returns the unsubscribe function. */
  on<T extends ServerEventType>(type: T, handler: EventHandler<T>): () => void;
  /**
   * Called after every connection that follows a failure or a stop (events were possibly missed):
   * refetch what you cache. Not called for a clean first connection.
   */
  onResync(handler: () => void): () => void;
  /** Opens the stream. Idempotent; the shell calls it once. */
  start(): void;
  stop(): void;
  /** Delivers an event as if the server had sent it (tests, synthetic local events). */
  emit(event: ServerEvent): void;
}

export const SERVER_RETRY_MS = 3000;
const MAX_RETRY_MS = 30_000;
const JITTER = 0.25;

function isServerEvent(value: unknown): value is ServerEvent {
  if (typeof value !== 'object' || value === null) return false;
  const event = value as Record<string, unknown>;
  switch (event.type) {
    case 'task':
      return typeof event.task === 'object' && event.task !== null;
    case 'fs_change':
      return typeof event.volume_id === 'number' && Array.isArray(event.dirs);
    case 'index':
      return (
        typeof event.state === 'string' &&
        typeof event.files_total === 'number' &&
        typeof event.files_done === 'number'
      );
    default:
      return false;
  }
}

const KNOWN_TYPES: readonly unknown[] = ['task', 'fs_change', 'index'];

export function createEventsClient(options: EventsClientOptions = {}): EventsClient {
  const url = options.url ?? '/api/events';
  const factory = options.factory ?? ((target: string) => new EventSource(target));
  const minRetryMs = options.minRetryMs ?? SERVER_RETRY_MS;
  const maxRetryMs = Math.max(options.maxRetryMs ?? MAX_RETRY_MS, minRetryMs);
  const random = options.random ?? Math.random;

  const connection = signal<ConnectionState>('closed');
  const handlers: { [T in ServerEventType]: Set<EventHandler<T>> } = {
    task: new Set(),
    fs_change: new Set(),
    index: new Set(),
  };
  const resyncHandlers = new Set<() => void>();

  let running = false;
  let source: EventSourceLike | null = null;
  let timer: number | undefined;
  let attempt = 0;
  let resyncPending = false;

  function dispatch<T extends ServerEventType>(event: ServerEventOf<T>) {
    for (const handler of [...(handlers[event.type as T] as Set<EventHandler<T>>)]) {
      try {
        handler(event);
      } catch (error) {
        console.error(`[events] a "${event.type}" handler threw`, error);
      }
    }
  }

  function receive(raw: unknown) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(String(raw));
    } catch {
      console.warn('[events] dropped a message that is not JSON');
      return;
    }
    if (isServerEvent(parsed)) {
      dispatch(parsed);
      return;
    }
    const type =
      typeof parsed === 'object' && parsed !== null ? (parsed as { type?: unknown }).type : null;
    // A well-formed event of a type this build does not know (newer server) is ignored quietly.
    if (typeof type !== 'string' || KNOWN_TYPES.includes(type)) {
      console.warn('[events] dropped a malformed event', parsed);
    }
  }

  function closeSource() {
    const current = source;
    source = null;
    if (!current) return;
    current.onopen = current.onmessage = current.onerror = null;
    try {
      current.close();
    } catch {
      // Already closed.
    }
  }

  function backoff(): number {
    const delay = minRetryMs * 2 ** attempt * (1 + JITTER * random());
    attempt += 1;
    return Math.round(Math.min(maxRetryMs, delay));
  }

  function fail() {
    closeSource();
    resyncPending = true;
    connection.value = 'reconnecting';
    timer = window.setTimeout(connect, backoff());
  }

  function connect() {
    timer = undefined;
    connection.value = resyncPending ? 'reconnecting' : 'connecting';
    let next: EventSourceLike;
    try {
      next = factory(url);
    } catch (error) {
      console.error('[events] cannot open the event stream', error);
      fail();
      return;
    }
    source = next;
    next.onopen = () => {
      attempt = 0;
      connection.value = 'open';
      if (!resyncPending) return;
      resyncPending = false;
      for (const handler of [...resyncHandlers]) {
        try {
          handler();
        } catch (error) {
          console.error('[events] a resync handler threw', error);
        }
      }
    };
    next.onmessage = (message) => receive(message.data);
    next.onerror = () => fail();
  }

  // A sleeping laptop or a network change should not wait out a 30 s backoff.
  function retryNow() {
    if (timer === undefined) return;
    window.clearTimeout(timer);
    attempt = 0;
    connect();
  }

  function onVisibilityChange() {
    if (document.visibilityState === 'visible') retryNow();
  }

  return {
    connection,
    on(type, handler) {
      const entry: EventHandler<typeof type> = (event) => handler(event);
      const set = handlers[type] as Set<typeof entry>;
      set.add(entry);
      return () => {
        set.delete(entry);
      };
    },
    onResync(handler) {
      const entry = () => handler();
      resyncHandlers.add(entry);
      return () => {
        resyncHandlers.delete(entry);
      };
    },
    start() {
      if (running) return;
      running = true;
      window.addEventListener('online', retryNow);
      document.addEventListener('visibilitychange', onVisibilityChange);
      connect();
    },
    stop() {
      if (!running) return;
      running = false;
      window.removeEventListener('online', retryNow);
      document.removeEventListener('visibilitychange', onVisibilityChange);
      window.clearTimeout(timer);
      timer = undefined;
      closeSource();
      attempt = 0;
      resyncPending = true;
      connection.value = 'closed';
    },
    emit: dispatch,
  };
}

/** The shared client: one `EventSource('/api/events')` for the whole app. */
export const events: EventsClient = createEventsClient();
