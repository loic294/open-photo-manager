import { errorMessage, isApiError } from '@/api/errors';

const MESSAGES: Readonly<Record<string, string>> = {
  immich_not_connected: 'Connect to Immich first.',
  immich_needs_reconnect: 'Immich no longer accepts the stored key. Connect again.',
  immich_auth_failed: 'Immich refused the email or password. Check both and try again.',
  immich_invalid_url: 'Enter a valid HTTP or HTTPS address for Immich.',
  immich_admin_required: 'This action needs an Immich administrator account.',
  immich_unreachable:
    'The Immich address could not be reached. Check the URL and that Immich is running.',
  immich_unauthorized: 'Immich refused the stored key. Connect again.',
  immich_forbidden: 'This account does not have permission for this action in Immich.',
  immich_error: 'Immich refused the request. Check the connection and try again.',
  immich_not_mapped: 'Map a volume to Immich before syncing.',
  immich_unavailable: 'Immich is not available. Updates will retry when it is back.',
  database_update_pending:
    'The file moved, but its Immich path update is not confirmed yet. It is durably queued for retry.',
  unreadable: 'The volume could not be read. Check that it is online.',
  not_offline: 'Immich still shows the old asset online. Rescan its library before retrying.',
  scan_timeout: 'Immich has not noticed the change. Rescan its library and retry.',
  carry_failed: 'Immich refused to carry over albums or metadata. Check the asset before retrying.',
  path_mismatch: 'The Immich asset is at a different path. Check the volume mapping.',
};

export function describeImmichError(error: unknown): {
  code: string;
  message: string;
  field?: string;
} {
  const code = isApiError(error) ? error.code : typeof error === 'string' ? error : 'unknown';
  const field =
    code === 'immich_auth_failed'
      ? 'password'
      : code === 'immich_invalid_url'
        ? 'server_url'
        : undefined;
  return { code, message: MESSAGES[code] ?? errorMessage(error), ...(field ? { field } : {}) };
}
