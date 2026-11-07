import type { ErrorBody } from '@/api/generated/ErrorBody';

export interface ApiErrorInit {
  status: number;
  code: string;
  message: string;
  requestId?: string | undefined;
  cause?: unknown;
}

/**
 * Every failed `api` call rejects with this. `status` is the HTTP status, or `0` when the server
 * could not be reached (`code: 'network'`). `code` is the server's stable snake_case error code
 * (`not_found`, ...); `message` is safe to show to the user.
 */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  /** From the `x-request-id` response header: quote it when reporting a server problem. */
  readonly requestId: string | undefined;

  constructor(init: ApiErrorInit) {
    super(init.message, init.cause === undefined ? undefined : { cause: init.cause });
    this.name = 'ApiError';
    this.status = init.status;
    this.code = init.code;
    this.requestId = init.requestId;
  }
}

export function isApiError(value: unknown): value is ApiError {
  return value instanceof ApiError;
}

/** True for a cancelled request: callers normally ignore these (`signal.abort()` is not a failure). */
export function isAbortError(value: unknown): boolean {
  return typeof value === 'object' && value !== null && (value as Error).name === 'AbortError';
}

/** A user-presentable message for anything a `catch` block can receive. */
export function errorMessage(value: unknown, fallback = 'Something went wrong'): string {
  if (value instanceof Error && value.message) return value.message;
  if (typeof value === 'string' && value) return value;
  return fallback;
}

/** Narrow `{ "error": { "code", "message" } }`; anything else (proxy HTML, empty body) is `null`. */
export function parseErrorBody(value: unknown): ErrorBody['error'] | null {
  if (typeof value !== 'object' || value === null) return null;
  const error = (value as { error?: unknown }).error;
  if (typeof error !== 'object' || error === null) return null;
  const { code, message } = error as { code?: unknown; message?: unknown };
  return typeof code === 'string' && typeof message === 'string' ? { code, message } : null;
}
