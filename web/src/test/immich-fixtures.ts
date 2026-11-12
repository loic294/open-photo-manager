import type { ImmichLibraryDto } from '@/api/generated/ImmichLibraryDto';
import type { ImmichStatusDto } from '@/api/generated/ImmichStatusDto';
import type { OutboxPage } from '@/api/generated/OutboxPage';

export const sampleLibraries: ImmichLibraryDto[] = [
  {
    id: 'photos-lib',
    name: 'Photos',
    import_paths: ['/mnt/photos'],
    exclusion_patterns: [],
    asset_count: 42,
  },
  {
    id: 'archive-lib',
    name: 'Archive',
    import_paths: ['/mnt/archive', '/mnt/other'],
    exclusion_patterns: [],
    asset_count: 10,
  },
];

export function sampleImmichStatus(overrides: Partial<ImmichStatusDto> = {}): ImmichStatusDto {
  return {
    connected: true,
    mode: 'full',
    connection: {
      server_url: 'http://127.0.0.1:12283',
      email: 'admin@example.test',
      user_name: 'Test admin',
      is_admin: true,
      api_key_name: 'OPM test',
      allow_insecure_tls: false,
      server_version: 'v3.4.0',
      connected_at: 1700000000,
      last_ok_at: 1700000010,
      last_error: null,
    },
    reachable: true,
    needs_reconnect: false,
    server_version: 'v3.4.0',
    version_warning: null,
    mappings: [],
    last_sync_at: null,
    sync: null,
    outbox_pending: 0,
    needs_attention: 0,
    outbox: {
      pending: 0,
      waiting_for_immich: 0,
      attention: 0,
      oldest_pending_at: null,
      last_run_at: null,
      last_error: null,
      scans_on_demand: true,
    },
    direct_database: {
      ready: false,
      reason: 'Direct database access has not been checked.',
      checked_at: null,
      warning:
        'Direct SQL modification is not officially supported by Immich v3.3.3. Keep a current backup. An active scan can import a duplicate.',
    },
    ...overrides,
  };
}

export function sampleOutboxPage(overrides: Partial<OutboxPage> = {}): OutboxPage {
  return {
    items: [
      {
        id: 1,
        kind: 'moved',
        from: 'photos/a.jpg',
        to: 'archive/a.jpg',
        state: 'attention',
        attempts: 3,
        last_error: 'scan_timeout',
        created_at: 1700000000,
        next_attempt_at: 1700000100,
      },
      {
        id: 2,
        kind: 'removed',
        from: 'photos/b.jpg',
        to: null,
        state: 'attention',
        attempts: 4,
        last_error: 'not_offline',
        created_at: 1700000000,
        next_attempt_at: 1700000100,
      },
    ],
    total: 2,
    ...overrides,
  };
}
