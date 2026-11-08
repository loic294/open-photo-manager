/** Shared file-operation and planner reasons; unknown future codes still read naturally. */
export const FAILURE_REASONS: Readonly<Record<string, string>> = {
  source_missing: 'Source no longer exists',
  source_changed: 'Source changed since the plan was built',
  no_survivor: 'No surviving matched copy',
  stale_match: 'Matched counterpart changed',
  stale_plan: 'File or destination changed; build a fresh plan',
  stale_index: 'File facts changed since indexing',
  file_gone: 'File no longer exists',
  sidecar_conflict: 'Companion file conflicts with the destination',
  not_a_regular_file: 'Not a regular file',
  symlink_rejected: 'Symbolic links are not allowed',
  volume_offline: 'Volume is offline',
  unknown_volume: 'Volume is unknown',
  volume_read_only: 'Volume is read-only',
  file_in_use: 'File is open in another program; close it and retry',
  destination_invalid: 'Destination is invalid',
  name_too_long: 'File name is too long',
  no_space: 'Not enough free space',
  verify_failed: 'Copy verification failed',
  immich_move_refused:
    'Immich database update refused: no file moved. Check the database connection and retry.',
  not_empty: 'Folder is no longer empty',
  outside_trash: 'Item is outside the trash',
  io_error: 'File could not be read or written',
  cancelled: 'Operation was cancelled',
  internal: 'An internal error stopped the operation',
  aborted: 'Operation was interrupted',
  already_in_place: 'Already at the destination',
  identical_at_destination: 'Identical file already at the destination',
  skipped_by_policy: 'Skipped by the conflict policy',
  metadata_pending: 'Metadata is still being indexed',
  pattern_failed: 'Naming pattern could not be applied',
  extension_changed: 'The name pattern would change the original extension',
  renamed_by_policy: 'Renamed to avoid a name conflict',
  reserved_destination: 'Destination is reserved by another item',
  move: 'Will move',
  renamed: 'Renamed to avoid a name conflict',
  replaced: 'Existing destination file goes to the trash',
  cross_volume_items: 'Moved to another volume with copy verification',
};

export function describeFailure(code: string, message: string): string {
  const known = FAILURE_REASONS[code];
  if (typeof known === 'string') return known;
  const words = code.replace(/[_-]+/g, ' ').trim();
  return words ? words[0]?.toUpperCase() + words.slice(1) : message || 'Unknown failure';
}
