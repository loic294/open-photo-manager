import { describe, expect, it } from 'vitest';
import { describeFailure, FAILURE_REASONS } from '@/util/failure-codes';

describe('shared failure reasons', () => {
  it('pins every FsOpError code and each landed planner status', () => {
    const codes = [
      'source_missing',
      'source_changed',
      'not_a_regular_file',
      'symlink_rejected',
      'volume_offline',
      'unknown_volume',
      'volume_read_only',
      'file_in_use',
      'destination_invalid',
      'name_too_long',
      'no_space',
      'verify_failed',
      'immich_move_refused',
      'not_empty',
      'outside_trash',
      'io_error',
      'already_in_place',
      'identical_at_destination',
      'skipped_by_policy',
      'metadata_pending',
      'pattern_failed',
      'reserved_destination',
      'no_survivor',
      'stale_match',
      'cancelled',
      'internal',
      'aborted',
    ];
    for (const code of codes) {
      expect(FAILURE_REASONS[code]).toBeTruthy();
      expect(describeFailure(code, 'Technical detail')).toBe(FAILURE_REASONS[code]);
    }
  });
  it('makes an Immich move refusal explicit and actionable in the shared result flow', () => {
    const reason = describeFailure('immich_move_refused', 'Database unavailable');
    expect(reason).toContain('no file moved');
    expect(reason).toContain('database connection and retry');
  });
  it('humanizes unknown future codes and preserves a message when no code exists', () => {
    expect(describeFailure('counterpart_went_missing', 'Internal detail')).toBe(
      'Counterpart went missing',
    );
    expect(describeFailure('future-error', '')).toBe('Future error');
    expect(describeFailure('constructor', '')).toBe('Constructor');
    expect(describeFailure('__proto__', '')).toBe('Proto');
    expect(describeFailure('', 'Something happened')).toBe('Something happened');
    expect(describeFailure('', '')).toBe('Unknown failure');
  });
});
