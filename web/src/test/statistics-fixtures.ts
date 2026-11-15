import type { StatisticsResponse } from '@/api/generated/StatisticsResponse';

export function sampleStatistics(): StatisticsResponse {
  return {
    timezone: 'America/Los_Angeles',
    from: '2026-11-11',
    to: '2026-11-13',
    started_at: 1794412800,
    observed_at: 1794610000,
    pending_events: 0,
    coverage: [],
    segments: [{ id: 1, timezone: 'America/Los_Angeles', started_at: 1794412800, ended_at: null }],
    activity: [
      {
        segment_id: 1,
        day: '2026-11-13',
        timezone: 'America/Los_Angeles',
        volume_id: 1,
        volume_name: 'Photos',
        action: 'duplicates',
        count: 2,
        bytes: 2048,
      },
    ],
    snapshots: [
      {
        segment_id: 1,
        day: '2026-11-13',
        timezone: 'America/Los_Angeles',
        observed_at: 1794610000,
        inventory: {
          volume_id: 1,
          volume_name: 'Photos',
          online: true,
          baseline_complete: true,
          last_scan_at: 1794610000,
          held_removals: 0,
          files: 10,
          bytes: 10240,
          kinds: [{ kind: 'image', files: 10, bytes: 10240 }],
          metadata_pending: 1,
          metadata_failed: 2,
          hashes_pending: 0,
          hashes_failed: 0,
          thumbnails_pending: 0,
          thumbnails_failed: 0,
          trash_items: 2,
          trash_bytes: 2048,
        },
      },
    ],
  };
}
