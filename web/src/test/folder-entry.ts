import type { FolderEntry } from '@/api/generated/FolderEntry';
import type { Overlay } from '@/api/generated/Overlay';

type FolderEntryOverrides = Partial<Omit<FolderEntry, 'overlay'>> & { overlay?: Partial<Overlay> };

/**
 * A folder of a listing that has 3 items below it and nothing planned; override what a test is
 * about. `path` follows `name` unless given (a folder of the volume root).
 */
export function folderEntry(overrides: FolderEntryOverrides = {}): FolderEntry {
  const { overlay, ...rest } = overrides;
  const name = rest.name ?? 'trip';
  return {
    name,
    path: name,
    volume_id: 1,
    count: 3,
    total: 3,
    matches: null,
    overlay: { matched: false, planned: false, will_delete: false, ...overlay },
    ...rest,
  };
}
