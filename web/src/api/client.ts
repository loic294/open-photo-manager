import { ApiError, isAbortError, parseErrorBody, parseFieldErrors } from '@/api/errors';

/** Header the server requires on every state-changing request (CSRF guard). */
export const CSRF_HEADER = 'x-opm-request';

export type QueryScalar = string | number | boolean | null | undefined;
export type Query = Record<string, QueryScalar | readonly QueryScalar[]>;

export interface RequestOptions {
  /** `null`/`undefined` values are skipped and arrays repeat the key (`?a=1&a=2`). */
  query?: Query;
  /** Serialised as JSON (sets `content-type`). */
  body?: unknown;
  /** Aborting rejects with the platform's `AbortError`: check it with `isAbortError`. */
  signal?: AbortSignal | undefined;
  headers?: Record<string, string>;
}

export type GetOptions = Omit<RequestOptions, 'body'>;

export interface ApiClient {
  get<T>(path: string, options?: GetOptions): Promise<T>;
  post<T>(path: string, options?: RequestOptions): Promise<T>;
  put<T>(path: string, options?: RequestOptions): Promise<T>;
  delete<T>(path: string, options?: RequestOptions): Promise<T>;
}

export interface ApiClientConfig {
  /** Defaults to the global `fetch`, looked up on every call (so test stubs apply). */
  fetch?: typeof fetch;
  /** Prefixed to every path (default `''`: same origin). */
  baseUrl?: string;
}

/** Query string without the leading `?` (empty when there is nothing to send). */
export function buildQuery(query: Query | undefined): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query ?? {})) {
    for (const item of Array.isArray(value) ? value : [value]) {
      if (item !== null && item !== undefined) params.append(key, String(item));
    }
  }
  return params.toString();
}

export function withQuery(path: string, query?: Query): string {
  const search = buildQuery(query);
  if (!search) return path;
  return `${path}${path.includes('?') ? '&' : '?'}${search}`;
}

function failure(status: number, text: string, requestId: string | undefined): ApiError {
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    // Not JSON (a proxy's HTML page, an empty body): fall through to the generic error.
  }
  const parsed = parseErrorBody(body);
  return new ApiError({
    status,
    code: parsed?.code ?? 'http_error',
    message: parsed?.message ?? `Request failed with status ${status}`,
    requestId,
    fields: parsed ? parseFieldErrors(body) : [],
  });
}

export function createApiClient(config: ApiClientConfig = {}): ApiClient {
  const baseUrl = config.baseUrl ?? '';

  async function request<T>(method: string, path: string, options: RequestOptions): Promise<T> {
    const { query, body, signal, headers: extra } = options;
    const headers = new Headers({ accept: 'application/json', ...extra });
    if (method !== 'GET') headers.set(CSRF_HEADER, '1');
    if (body !== undefined) headers.set('content-type', 'application/json');

    let response: Response;
    let text: string;
    try {
      const init = {
        method,
        headers,
        body: body === undefined ? null : JSON.stringify(body),
        signal,
      };
      const url = baseUrl + withQuery(path, query);
      response = await (config.fetch ? config.fetch(url, init) : globalThis.fetch(url, init));
      text = await response.text();
    } catch (error) {
      // A caller's abort is not a failure: propagate it untouched, whatever it rejected with.
      if (signal?.aborted || isAbortError(error)) throw error;
      throw new ApiError({
        status: 0,
        code: 'network',
        message: 'Could not reach the server. Check your connection and try again.',
        cause: error,
      });
    }

    const requestId = response.headers.get('x-request-id') ?? undefined;
    if (!response.ok) throw failure(response.status, text, requestId);
    if (response.status === 204 || text === '') return undefined as T;
    try {
      return JSON.parse(text) as T;
    } catch (cause) {
      throw new ApiError({
        status: response.status,
        code: 'invalid_response',
        message: 'The server sent a response that could not be read.',
        requestId,
        cause,
      });
    }
  }

  return {
    get: (path, options = {}) => request('GET', path, options),
    post: (path, options = {}) => request('POST', path, options),
    put: (path, options = {}) => request('PUT', path, options),
    delete: (path, options = {}) => request('DELETE', path, options),
  };
}

/** The shared client. Wrap endpoints in thin typed functions in `src/api/{area}.ts`. */
export const api: ApiClient = createApiClient();
