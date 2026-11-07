import { afterEach, describe, expect, it, vi } from 'vitest';
import { api, buildQuery, createApiClient, withQuery } from '@/api/client';
import { ApiError, isAbortError, isApiError } from '@/api/errors';

function respond(body: BodyInit | null, init: ResponseInit = {}): Response {
  return new Response(body, init);
}

function json(value: unknown, init: ResponseInit = {}): Response {
  return respond(JSON.stringify(value), {
    ...init,
    headers: { 'content-type': 'application/json', ...init.headers },
  });
}

function stubFetch(handler: (url: string, init: RequestInit) => Response | Promise<Response>) {
  return vi.fn<typeof fetch>(async (input, init) => handler(String(input), init ?? {}));
}

function sent(mock: ReturnType<typeof stubFetch>, index = 0) {
  const [url, init] = mock.mock.calls[index] ?? [];
  return { url: String(url), init: init as RequestInit, headers: new Headers(init?.headers) };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('buildQuery / withQuery', () => {
  it('skips null and undefined, repeats array keys and stringifies scalars', () => {
    const query = buildQuery({
      a: 1,
      b: true,
      c: null,
      d: undefined,
      e: ['x', 'y', null],
      f: 'a b',
    });
    expect(query).toBe('a=1&b=true&e=x&e=y&f=a+b');
  });

  it('returns an empty string when there is nothing to send', () => {
    expect(buildQuery(undefined)).toBe('');
    expect(buildQuery({ a: undefined, b: [] })).toBe('');
    expect(withQuery('/api/x', { a: null })).toBe('/api/x');
  });

  it('appends to a path that already has a query string', () => {
    expect(withQuery('/api/x', { a: 1 })).toBe('/api/x?a=1');
    expect(withQuery('/api/x?z=0', { a: 1 })).toBe('/api/x?z=0&a=1');
  });
});

describe('requests', () => {
  it('sends GET without the CSRF header and parses JSON', async () => {
    const fetchMock = stubFetch(() => json([{ id: 1 }]));
    const client = createApiClient({ fetch: fetchMock });

    const result = await client.get<{ id: number }[]>('/api/volumes', { query: { a: 1 } });

    expect(result).toEqual([{ id: 1 }]);
    const { url, init, headers } = sent(fetchMock);
    expect(url).toBe('/api/volumes?a=1');
    expect(init.method).toBe('GET');
    expect(headers.get('x-apm-request')).toBeNull();
    expect(headers.get('accept')).toBe('application/json');
    expect(headers.get('content-type')).toBeNull();
    expect(init.body).toBeNull();
  });

  it.each(['post', 'put', 'delete'] as const)(
    'sends %s with the CSRF header and a JSON body',
    async (verb) => {
      const fetchMock = stubFetch(() => json({ ok: true }));
      const client = createApiClient({ fetch: fetchMock });

      await client[verb]('/api/things', { body: { name: 'é', ids: [1, 2] } });

      const { init, headers } = sent(fetchMock);
      expect(init.method).toBe(verb.toUpperCase());
      expect(headers.get('x-apm-request')).toBe('1');
      expect(headers.get('content-type')).toBe('application/json');
      expect(JSON.parse(String(init.body))).toEqual({ name: 'é', ids: [1, 2] });
    },
  );

  it('sends CSRF on bodyless writes and ignores a caller trying to override it', async () => {
    const fetchMock = stubFetch(() => respond(null, { status: 204 }));
    const client = createApiClient({ fetch: fetchMock });

    await client.post('/api/rescan', { headers: { 'x-apm-request': '0', 'x-extra': 'yes' } });

    const { headers, init } = sent(fetchMock);
    expect(headers.get('x-apm-request')).toBe('1');
    expect(headers.get('x-extra')).toBe('yes');
    expect(headers.get('content-type')).toBeNull();
    expect(init.body).toBeNull();
  });

  it('resolves undefined for 204 and for an empty 200 body', async () => {
    const client = createApiClient({
      fetch: stubFetch((url) =>
        url.endsWith('204') ? respond(null, { status: 204 }) : respond(''),
      ),
    });

    await expect(client.post<void>('/api/204')).resolves.toBeUndefined();
    await expect(client.get<void>('/api/empty')).resolves.toBeUndefined();
  });

  it('prefixes baseUrl', async () => {
    const fetchMock = stubFetch(() => json({}));
    await createApiClient({ fetch: fetchMock, baseUrl: 'http://nas:8080' }).get('/api/health');
    expect(sent(fetchMock).url).toBe('http://nas:8080/api/health');
  });

  it('looks the global fetch up on every call', async () => {
    const fetchMock = stubFetch(() => json({ status: 'ok' }));
    vi.stubGlobal('fetch', fetchMock);

    await expect(api.get('/api/health')).resolves.toEqual({ status: 'ok' });
    expect(fetchMock).toHaveBeenCalledOnce();
  });
});

describe('errors', () => {
  it('turns the server error body into an ApiError with the request id', async () => {
    const client = createApiClient({
      fetch: stubFetch(() =>
        json(
          { error: { code: 'not_found', message: 'No such folder' } },
          { status: 404, headers: { 'x-request-id': 'req-42' } },
        ),
      ),
    });

    const error = await client.get('/api/folders').catch((e: unknown) => e);

    expect(isApiError(error)).toBe(true);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({
      name: 'ApiError',
      status: 404,
      code: 'not_found',
      message: 'No such folder',
      requestId: 'req-42',
    });
  });

  it('falls back to http_error for non-JSON and unexpected bodies', async () => {
    const html = createApiClient({
      fetch: stubFetch(() => respond('<html>bad gateway</html>', { status: 502 })),
    });
    const shape = createApiClient({
      fetch: stubFetch(() => json({ error: 'nope' }, { status: 500 })),
    });

    await expect(html.get('/api/x')).rejects.toMatchObject({
      status: 502,
      code: 'http_error',
      message: 'Request failed with status 502',
      requestId: undefined,
    });
    await expect(shape.get('/api/x')).rejects.toMatchObject({ status: 500, code: 'http_error' });
  });

  it('maps network failures to code "network" with status 0 and keeps the cause', async () => {
    const cause = new TypeError('Failed to fetch');
    const client = createApiClient({
      fetch: vi.fn<typeof fetch>(async () => {
        throw cause;
      }),
    });

    const error = await client.get('/api/x').catch((e: unknown) => e);

    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({ status: 0, code: 'network', cause });
  });

  it('reports an unreadable success body as invalid_response', async () => {
    const client = createApiClient({ fetch: stubFetch(() => respond('<html>', { status: 200 })) });

    await expect(client.get('/api/x')).rejects.toMatchObject({
      status: 200,
      code: 'invalid_response',
    });
  });
});

describe('abort', () => {
  // Like the real fetch: never settles until the signal aborts, then rejects with its reason.
  function neverResolving() {
    return stubFetch((_url, init) => {
      const signal = init.signal as AbortSignal;
      return new Promise<Response>((_resolve, reject) => {
        if (signal.aborted) reject(signal.reason);
        signal.addEventListener('abort', () => reject(signal.reason));
      });
    });
  }

  it('passes the signal to fetch and propagates the AbortError untouched', async () => {
    const fetchMock = neverResolving();
    const client = createApiClient({ fetch: fetchMock });
    const controller = new AbortController();

    const pending = client.get('/api/slow', { signal: controller.signal }).catch((e: unknown) => e);
    controller.abort();
    const error = await pending;

    expect(sent(fetchMock).init.signal).toBe(controller.signal);
    expect(isAbortError(error)).toBe(true);
    expect(isApiError(error)).toBe(false);
  });

  it('propagates a custom abort reason instead of reporting a network error', async () => {
    const client = createApiClient({ fetch: neverResolving() });
    const controller = new AbortController();

    const pending = client.get('/api/slow', { signal: controller.signal }).catch((e: unknown) => e);
    controller.abort('navigated away');

    await expect(pending).resolves.toBe('navigated away');
  });

  it('does not wrap an already aborted request', async () => {
    const client = createApiClient({ fetch: neverResolving() });
    const controller = new AbortController();
    controller.abort();

    const error = await client
      .get('/api/x', { signal: controller.signal })
      .catch((e: unknown) => e);

    expect(isAbortError(error)).toBe(true);
  });
});
