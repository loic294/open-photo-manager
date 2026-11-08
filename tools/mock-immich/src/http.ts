import type { IncomingMessage, ServerResponse } from 'node:http';

const STATUS_NAMES: Record<number, string> = {
  400: 'Bad Request',
  401: 'Unauthorized',
  403: 'Forbidden',
  404: 'Not Found',
  408: 'Request Timeout',
  409: 'Conflict',
  413: 'Payload Too Large',
  429: 'Too Many Requests',
  500: 'Internal Server Error',
  502: 'Bad Gateway',
  503: 'Service Unavailable',
  504: 'Gateway Timeout',
};

/** Immich's error body (NestJS): `{ message, error, statusCode }`. */
export function errorBody(status: number, message: string | string[]) {
  return { message, error: STATUS_NAMES[status] ?? 'Error', statusCode: status };
}

export class ApiError extends Error {
  status: number;
  detail: string | string[];

  constructor(status: number, detail: string | string[]) {
    super(Array.isArray(detail) ? detail.join('; ') : detail);
    this.status = status;
    this.detail = detail;
  }
}

export const badRequest = (detail: string | string[]): ApiError => new ApiError(400, detail);

/** What a handler answers: a status and, unless it is 204, a JSON body. */
export interface Reply {
  status: number;
  body?: unknown;
}

export const ok = (body: unknown): Reply => ({ status: 200, body });
export const created = (body: unknown): Reply => ({ status: 201, body });
export const noContent = (): Reply => ({ status: 204 });

const MAX_BODY_BYTES = 64 * 1024 * 1024;

export async function readBody(req: IncomingMessage): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    const part = Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk));
    size += part.length;
    if (size > MAX_BODY_BYTES) {
      throw new ApiError(413, 'Request body too large');
    }
    chunks.push(part);
  }
  return Buffer.concat(chunks);
}

/** `undefined` for an empty body; a 400 for anything that is not JSON. */
export function parseJson(raw: Buffer): unknown {
  if (raw.length === 0) {
    return undefined;
  }
  try {
    return JSON.parse(raw.toString('utf8'));
  } catch (error) {
    const reason = error instanceof Error ? error.message : 'invalid JSON';
    throw badRequest(reason);
  }
}

export function sendJson(res: ServerResponse, status: number, body?: unknown): void {
  if (res.headersSent || res.writableEnded) {
    return;
  }
  if (status === 204 || body === undefined) {
    res.writeHead(status);
    res.end();
    return;
  }
  const payload = Buffer.from(JSON.stringify(body));
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': payload.length,
  });
  res.end(payload);
}

export function sendError(res: ServerResponse, error: ApiError): void {
  sendJson(res, error.status, errorBody(error.status, error.detail));
}
