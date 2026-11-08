export type AuthKind = 'none' | 'apiKey' | 'session' | 'invalid';

export interface LogEntry {
  /** Absolute and never reused, even after a reset. */
  index: number;
  time: string;
  method: string;
  path: string;
  query?: string;
  /** 0 while the request is in flight (held at a gate) or when the connection was dropped. */
  status: number;
  auth: AuthKind;
  keyName?: string;
  /** No route matched: a call the mock does not model (the Rust client never makes one). */
  unexpected?: true;
  durationMs: number;
  /** The JSON body with secrets redacted, long strings cut and long arrays summarised. */
  bodySummary?: unknown;
  /** A failure rule or a gate acted on this request. */
  injected?: 'fail' | 'drop' | 'slow' | 'gate';
}

export interface LogHandle {
  entry: LogEntry;
  done(status: number): void;
}

export interface RequestLog {
  begin(method: string, path: string, query: string): LogHandle;
  /** The entries with an index of at least `index`, oldest first. */
  since(index?: number): { entries: LogEntry[]; next: number };
  /** Forgets the entries; later ones keep counting from where this one stopped. */
  clear(): void;
}

const SECRET_KEY_RE = /password|secret|token|apikey/i;
const MAX_STRING = 120;
const MAX_ARRAY = 20;
const MAX_DEPTH = 6;

export function summariseBody(value: unknown, depth = 0): unknown {
  if (typeof value === 'string') {
    return value.length > MAX_STRING ? `${value.slice(0, MAX_STRING)}…` : value;
  }
  if (Array.isArray(value)) {
    if (value.length > MAX_ARRAY) {
      return `[${value.length} items]`;
    }
    return value.map((item) => summariseBody(item, depth + 1));
  }
  if (typeof value === 'object' && value !== null) {
    if (depth >= MAX_DEPTH) {
      return '[object]';
    }
    const out: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) {
      out[key] = SECRET_KEY_RE.test(key) ? '[redacted]' : summariseBody(item, depth + 1);
    }
    return out;
  }
  return value;
}

export function createRequestLog(capacity = 50_000): RequestLog {
  let entries: LogEntry[] = [];
  let first = 0;

  return {
    begin(method, path, query) {
      const started = performance.now();
      const entry: LogEntry = {
        index: first + entries.length,
        time: new Date().toISOString(),
        method,
        path,
        status: 0,
        auth: 'none',
        durationMs: 0,
      };
      if (query !== '') {
        entry.query = query;
      }
      entries.push(entry);
      if (entries.length > capacity + capacity / 10) {
        const drop = entries.length - capacity;
        entries = entries.slice(drop);
        first += drop;
      }
      return {
        entry,
        done(status) {
          entry.status = status;
          entry.durationMs = Math.round((performance.now() - started) * 10) / 10;
        },
      };
    },
    since(index = 0) {
      const start = Math.max(index - first, 0);
      return {
        entries: entries.slice(start).map((entry) => structuredClone(entry)),
        next: first + entries.length,
      };
    },
    clear() {
      first += entries.length;
      entries = [];
    },
  };
}
