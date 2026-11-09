import { type Agent, type IncomingHttpHeaders, request } from 'node:http';

export interface RawRequest {
  url: string;
  method?: string;
  path: string;
  headers?: Record<string, string | string[]> | string[];
  body?: string | Uint8Array;
  headersOnly?: boolean;
  agent?: Agent;
  signal?: AbortSignal;
}

/** Node HTTP preserves controlled and duplicate headers without Fetch's URL normalization. */
export function rawRequest(options: RawRequest) {
  return new Promise<{
    status: number;
    headers: IncomingHttpHeaders;
    bytes: Buffer;
    text: string;
    json<T = unknown>(): T;
  }>((resolve, reject) => {
    const signal = options.signal;
    const abortError = () =>
      Object.assign(new Error('The operation was aborted', { cause: signal?.reason }), {
        name: 'AbortError',
        code: 'ABORT_ERR',
      });
    if (signal?.aborted) {
      reject(abortError());
      return;
    }
    const cleanup = () => signal?.removeEventListener('abort', abort);
    const fail = (error: Error) => {
      cleanup();
      reject(error);
    };
    const origin = new URL(options.url);
    const req = request(
      {
        hostname: origin.hostname,
        port: origin.port,
        method: options.method ?? 'GET',
        path: options.path,
        headers: options.headers ?? { Host: origin.host },
        setHost: false,
        agent: options.agent ?? false,
      },
      (response) => {
        const chunks: Buffer[] = [];
        let size = 0;
        const finish = () => {
          cleanup();
          const bytes = Buffer.concat(chunks);
          const text = bytes.toString('utf8');
          resolve({
            status: response.statusCode ?? 0,
            headers: response.headers,
            bytes,
            text,
            json: <T>() => JSON.parse(text) as T,
          });
        };
        if (options.headersOnly) {
          finish();
          response.destroy();
          req.destroy();
          return;
        }
        response.on('data', (chunk: Buffer) => {
          size += chunk.length;
          if (size > 4 * 1024 ** 2) {
            req.destroy(new Error('Raw HTTP response exceeds 4 MiB'));
            return;
          }
          chunks.push(chunk);
        });
        response.on('error', fail);
        response.on('end', finish);
      },
    );
    const abort = () => {
      const error = abortError();
      req.destroy(error);
      fail(error);
    };
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    req.setTimeout(15_000, () => req.destroy(new Error('Raw HTTP request timed out')));
    req.on('error', fail);
    req.end(options.body);
  });
}
