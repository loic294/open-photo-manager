import type { FileDetail } from '@/api/generated/FileDetail';
import type { FileItem } from '@/api/generated/FileItem';
import type { Overlay } from '@/api/generated/Overlay';

type FileItemOverrides = Partial<Omit<FileItem, 'overlay'>> & { overlay?: Partial<Overlay> };

/** A plain photo with a capture date taken from its EXIF data; override what a test is about. */
export function fileItem(overrides: FileItemOverrides = {}): FileItem {
  const { overlay, ...rest } = overrides;
  return {
    id: 7,
    volume_id: 1,
    dir: 'trip',
    name: 'IMG_0007.jpg',
    kind: 'image',
    size: 4_200_000,
    capture_ts: Date.UTC(2024, 0, 5, 15, 4) / 1000,
    capture_src: 'exif',
    camera: 'Canon EOS R5',
    rating: 0,
    flag: 0,
    width: 6000,
    height: 4000,
    duration_ms: null,
    mtime: 1_700_000_000,
    in_immich: false,
    overlay: { matched: false, planned: false, will_delete: false, ...overlay },
    ...rest,
  };
}

/** The detail of `item`: a JPEG without a hash, a birth time, tags or sidecars until a test adds them. */
export function fileDetail(
  item: FileItem = fileItem(),
  overrides: Partial<Omit<FileDetail, 'item'>> = {},
): FileDetail {
  return {
    item,
    hash: null,
    mime: 'image/jpeg',
    btime: null,
    tags: [],
    sidecars: [],
    ...overrides,
  };
}
