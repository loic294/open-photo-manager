import { expect, it } from 'vitest';
import { ApiError } from '@/api/errors';
import { describeImmichError } from '@/tools/settings/immich/immich-errors';

it.each([
  ['immich_not_connected', 'Connect to Immich'],
  ['immich_needs_reconnect', 'stored key'],
  ['immich_auth_failed', 'email or password'],
  ['immich_invalid_url', 'HTTP or HTTPS'],
  ['immich_admin_required', 'administrator'],
  ['immich_unreachable', 'could not be reached'],
  ['immich_unauthorized', 'stored key'],
  ['immich_forbidden', 'permission'],
  ['immich_error', 'refused'],
  ['immich_not_mapped', 'Map a volume'],
  ['unreadable', 'volume'],
  ['immich_unavailable', 'retry'],
  ['not_offline', 'online'],
  ['scan_timeout', 'Rescan'],
  ['carry_failed', 'carry over'],
  ['path_mismatch', 'different path'],
])('describes %s', (code, words) => {
  expect(
    describeImmichError(new ApiError({ status: 409, code, message: 'raw' })).message,
  ).toContain(words);
  expect(describeImmichError(code).message).toContain(words);
});

it('places credential errors and preserves unknown API messages', () => {
  expect(describeImmichError('immich_auth_failed').field).toBe('password');
  expect(describeImmichError('immich_invalid_url').field).toBe('server_url');
  expect(
    describeImmichError(new ApiError({ status: 500, code: 'other', message: 'Useful detail' })),
  ).toEqual({ code: 'other', message: 'Useful detail' });
  expect(describeImmichError(new Error('Network failed')).message).toBe('Network failed');
});
