import { describe, expect, it } from 'vitest';
import { ApiError, errorMessage, isAbortError, isApiError, parseErrorBody } from '@/api/errors';

describe('ApiError', () => {
  it('carries status, code, requestId and an optional cause', () => {
    const cause = new Error('boom');
    const error = new ApiError({ status: 409, code: 'conflict', message: 'Already exists', cause });

    expect(error).toBeInstanceOf(Error);
    expect(error.name).toBe('ApiError');
    expect(error.status).toBe(409);
    expect(error.code).toBe('conflict');
    expect(error.requestId).toBeUndefined();
    expect(error.message).toBe('Already exists');
    expect(error.cause).toBe(cause);
    expect(isApiError(error)).toBe(true);
    expect(isApiError(new Error('x'))).toBe(false);
  });
});

describe('isAbortError', () => {
  it('recognises DOM abort errors by name only', () => {
    expect(isAbortError(new DOMException('Aborted', 'AbortError'))).toBe(true);
    expect(isAbortError(new Error('x'))).toBe(false);
    expect(isAbortError('AbortError')).toBe(false);
    expect(isAbortError(null)).toBe(false);
  });
});

describe('errorMessage', () => {
  it('prefers the message of an Error or string and falls back otherwise', () => {
    expect(errorMessage(new ApiError({ status: 500, code: 'x', message: 'Server broke' }))).toBe(
      'Server broke',
    );
    expect(errorMessage('plain')).toBe('plain');
    expect(errorMessage(undefined)).toBe('Something went wrong');
    expect(errorMessage(new Error(''), 'custom')).toBe('custom');
  });
});

describe('parseErrorBody', () => {
  it('accepts only { error: { code: string, message: string } }', () => {
    expect(parseErrorBody({ error: { code: 'a', message: 'b' } })).toEqual({
      code: 'a',
      message: 'b',
    });
    expect(parseErrorBody({ error: { code: 'a' } })).toBeNull();
    expect(parseErrorBody({ error: 'a' })).toBeNull();
    expect(parseErrorBody(null)).toBeNull();
    expect(parseErrorBody('x')).toBeNull();
  });
});
