import { randomUUID } from 'node:crypto';
import type { MockContext } from './mock.ts';
import type { Asset, AssetType, Visibility } from './types.ts';
import { basename, extensionOf, iso, isoOrNull, type Obj, sha1Base64 } from './util.ts';

const IMAGE_EXTENSIONS = new Set(
  (
    '3fr ari arw avif bmp cap cin cr2 cr3 crw dcr dng erf fff gif heic heif hif iiq jp2 jpe jpeg ' +
    'jpg jxl k25 kdc mrw nef orf ori pef png psd raf raw rw2 rwl sr2 srf srw svg tif tiff webp x3f'
  ).split(' '),
);

const VIDEO_EXTENSIONS = new Set(
  '3gp 3gpp avi flv m2t m2ts m4v mkv mov mp4 mpg mts mxf ts webm wmv'.split(' '),
);

const MIME_TYPES: Record<string, string> = {
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  jpe: 'image/jpeg',
  png: 'image/png',
  gif: 'image/gif',
  webp: 'image/webp',
  heic: 'image/heic',
  heif: 'image/heif',
  avif: 'image/avif',
  tif: 'image/tiff',
  tiff: 'image/tiff',
  bmp: 'image/bmp',
  dng: 'image/dng',
  cr2: 'image/x-canon-cr2',
  cr3: 'image/x-canon-cr3',
  nef: 'image/x-nikon-nef',
  arw: 'image/x-sony-arw',
  raf: 'image/x-fuji-raf',
  rw2: 'image/x-panasonic-rw2',
  orf: 'image/x-olympus-orf',
  mp4: 'video/mp4',
  mov: 'video/quicktime',
  avi: 'video/x-msvideo',
  mkv: 'video/x-matroska',
  webm: 'video/webm',
  m4v: 'video/x-m4v',
  '3gp': 'video/3gpp',
  mts: 'video/mp2t',
  m2ts: 'video/mp2t',
  wmv: 'video/x-ms-wmv',
  mpg: 'video/mpeg',
  flv: 'video/x-flv',
};

export const isMediaFile = (name: string): boolean => {
  const extension = extensionOf(name);
  return IMAGE_EXTENSIONS.has(extension) || VIDEO_EXTENSIONS.has(extension);
};

export const assetTypeOf = (name: string): AssetType =>
  VIDEO_EXTENSIONS.has(extensionOf(name)) ? 'VIDEO' : 'IMAGE';

export function mimeTypeOf(name: string): string {
  const extension = extensionOf(name);
  return (
    MIME_TYPES[extension] ?? `${assetTypeOf(name) === 'VIDEO' ? 'video' : 'image'}/x-${extension}`
  );
}

export interface NewAsset {
  libraryId: string;
  ownerId: string;
  originalPath: string;
  id?: string;
  type?: AssetType;
  fileCreatedAt?: number;
  fileModifiedAt?: number;
  isFavorite?: boolean;
  rating?: number | null;
  description?: string;
  visibility?: Visibility;
  isOffline?: boolean;
  trashed?: boolean;
  make?: string | null;
  model?: string | null;
  checksum?: string;
  mtimeMs?: number | null;
  /** The clock tick of a batch; a single asset takes its own. */
  now?: number;
}

/** Adds an asset to the model; the path must not be taken (callers upsert on their own). */
export function addAsset(ctx: MockContext, input: NewAsset): Asset {
  const { data } = ctx;
  if (data.assetsByPath.has(input.originalPath)) {
    throw new Error(`an asset already exists at ${input.originalPath}`);
  }
  const now = input.now ?? ctx.clock.now();
  const created = input.fileCreatedAt ?? now;
  const asset: Asset = {
    id: input.id ?? randomUUID(),
    seq: data.nextSeq++,
    libraryId: input.libraryId,
    ownerId: input.ownerId,
    originalPath: input.originalPath,
    originalFileName: basename(input.originalPath),
    type: input.type ?? assetTypeOf(input.originalPath),
    checksum: input.checksum ?? sha1Base64(`path:${input.originalPath}`),
    fileCreatedAt: created,
    fileModifiedAt: input.fileModifiedAt ?? created,
    localDateTime: created,
    createdAt: now,
    updatedAt: now,
    deletedAt: input.trashed ? now : null,
    isOffline: input.isOffline ?? false,
    isFavorite: input.isFavorite ?? false,
    rating: input.rating ?? null,
    description: input.description ?? '',
    visibility: input.visibility ?? 'timeline',
    make: input.make ?? null,
    model: input.model ?? null,
    mtimeMs: input.mtimeMs ?? null,
  };
  data.assets.set(asset.id, asset);
  data.assetsByPath.set(asset.originalPath, asset);
  data.order = null;
  return asset;
}

/** Forgets an asset and its place in albums and tags (what a forced delete does). */
export function removeAsset(ctx: MockContext, asset: Asset): void {
  const { data } = ctx;
  data.assets.delete(asset.id);
  data.assetsByPath.delete(asset.originalPath);
  for (const album of data.albums.values()) {
    album.assetIds.delete(asset.id);
  }
  for (const tag of data.tags.values()) {
    tag.assetIds.delete(asset.id);
  }
  data.order = null;
}

/** Moves an asset's capture dates; the search order depends on them. */
export function setCaptureDate(ctx: MockContext, asset: Asset, fileCreatedAt: number): void {
  asset.fileCreatedAt = fileCreatedAt;
  asset.localDateTime = fileCreatedAt;
  ctx.data.order = null;
}

export const isTrashed = (asset: Asset): boolean => asset.deletedAt !== null;

/** How much of an asset a response carries. */
export type AssetDetail = 'basic' | 'exif' | 'full';

/** `ExifResponseDto`, reduced to what the mock models (nothing is read from the files). */
function exifView(asset: Asset): Obj {
  return {
    make: asset.make,
    model: asset.model,
    description: asset.description,
    rating: asset.rating,
    dateTimeOriginal: iso(asset.fileCreatedAt),
  };
}

/** `AssetResponseDto`. `deletedAt` is not in the spec, so only `isTrashed` tells about the trash. */
export function assetView(ctx: MockContext, asset: Asset, detail: AssetDetail = 'basic'): Obj {
  const view: Obj = {
    id: asset.id,
    checksum: asset.checksum,
    createdAt: iso(asset.createdAt),
    duration: asset.type === 'VIDEO' ? 0 : null,
    duplicateId: null,
    fileCreatedAt: iso(asset.fileCreatedAt),
    fileModifiedAt: iso(asset.fileModifiedAt),
    hasMetadata: true,
    height: null,
    isArchived: asset.visibility === 'archive',
    isEdited: false,
    isFavorite: asset.isFavorite,
    isOffline: asset.isOffline,
    isTrashed: isTrashed(asset),
    libraryId: asset.libraryId,
    livePhotoVideoId: null,
    localDateTime: iso(asset.localDateTime),
    originalFileName: asset.originalFileName,
    originalMimeType: mimeTypeOf(asset.originalFileName),
    originalPath: asset.originalPath,
    ownerId: asset.ownerId,
    thumbhash: null,
    type: asset.type,
    updatedAt: iso(asset.updatedAt),
    visibility: asset.visibility,
    width: null,
  };
  if (detail !== 'basic') {
    view.exifInfo = exifView(asset);
  }
  if (detail === 'full') {
    view.people = [];
    view.tags = [...ctx.data.tags.values()]
      .filter((tag) => tag.assetIds.has(asset.id))
      .map((tag) => ({
        id: tag.id,
        name: tag.name,
        value: tag.value,
        createdAt: iso(tag.createdAt),
        updatedAt: iso(tag.updatedAt),
      }));
  }
  return view;
}

/** What `/__mock/state` and `state.assets()` show: the model itself, with `deletedAt`. */
export function assetState(asset: Asset): Obj {
  return {
    id: asset.id,
    libraryId: asset.libraryId,
    ownerId: asset.ownerId,
    originalPath: asset.originalPath,
    originalFileName: asset.originalFileName,
    type: asset.type,
    checksum: asset.checksum,
    fileCreatedAt: iso(asset.fileCreatedAt),
    fileModifiedAt: iso(asset.fileModifiedAt),
    localDateTime: iso(asset.localDateTime),
    createdAt: iso(asset.createdAt),
    updatedAt: iso(asset.updatedAt),
    deletedAt: isoOrNull(asset.deletedAt),
    isOffline: asset.isOffline,
    isFavorite: asset.isFavorite,
    rating: asset.rating,
    description: asset.description,
    visibility: asset.visibility,
  };
}
