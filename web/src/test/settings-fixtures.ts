import type { SettingsDto } from '@/api/generated/SettingsDto';

/** The settings a fresh server answers with: use it as the base of a test's loaded document. */
export function sampleSettings(overrides: Partial<SettingsDto> = {}): SettingsDto {
  return {
    sidecar_extensions: ['xmp', 'json', 'aae', 'dop', 'pp3', 'xml'],
    junk_names: ['@eaDir', '#recycle', '#snapshot', '.DS_Store', 'Thumbs.db', 'desktop.ini'],
    junk_prefixes: ['._'],
    keep_cross_volume_source_in_trash: false,
    name_max_len: 255,
    default_conflict_policy: 'smart',
    index: {
      concurrency: 2,
      hash_concurrency: 1,
      thumbnail_concurrency: 2,
      watch_enabled: true,
      rescan_interval_minutes: 360,
    },
    trash_retention_days: null,
    excluded_folders: [],
    ...overrides,
  };
}
