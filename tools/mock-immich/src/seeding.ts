import { addAsset, setCaptureDate } from './assets.ts';
import { badRequest } from './http.ts';
import type { MockContext } from './mock.ts';
import { DEFAULT_GENERATE_START, pickLibrary } from './scenario.ts';
import type { Asset, AssetInput, GenerateInput, Library } from './types.ts';

function libraryFor(ctx: MockContext, ref: string | undefined, path: string): Library {
  const library = pickLibrary(ctx.data.libraries.values(), ref, path);
  if (library === undefined) {
    throw badRequest(
      ref === undefined
        ? `no library has an import path above ${path}: name one with "library"`
        : `unknown library ${JSON.stringify(ref)}`,
    );
  }
  return library;
}

const toMs = (value: string | undefined): number | undefined =>
  value === undefined ? undefined : Date.parse(value);

export interface Upserted {
  asset: Asset;
  created: boolean;
}

/** Creates the asset at `input.path`, or changes the fields given when there is one already. */
export function upsertAsset(
  ctx: MockContext,
  input: AssetInput,
  now: number = ctx.clock.now(),
): Upserted {
  const existing = ctx.data.assetsByPath.get(input.path);
  const library = libraryFor(ctx, input.library, input.path);
  if (existing === undefined) {
    if (input.id !== undefined && ctx.data.assets.has(input.id)) {
      throw badRequest(`asset id ${input.id} is already used`);
    }
    const asset = addAsset(ctx, {
      libraryId: library.id,
      ownerId: library.ownerId,
      originalPath: input.path,
      id: input.id,
      type: input.type,
      fileCreatedAt: toMs(input.fileCreatedAt),
      fileModifiedAt: toMs(input.fileModifiedAt),
      isFavorite: input.isFavorite,
      rating: input.rating,
      description: input.description,
      visibility: input.visibility,
      isOffline: input.isOffline,
      // Immich keeps the assets it lost in its trash.
      trashed: input.trashed ?? input.isOffline,
      make: input.make,
      model: input.model,
      checksum: input.checksum,
      now,
    });
    return { asset, created: true };
  }

  if (library.id !== existing.libraryId) {
    throw badRequest(`${input.path} belongs to another library`);
  }
  if (input.id !== undefined && input.id !== existing.id) {
    throw badRequest(`${input.path} already has the id ${existing.id}`);
  }
  if (input.type !== undefined) {
    existing.type = input.type;
  }
  const created = toMs(input.fileCreatedAt);
  if (created !== undefined) {
    setCaptureDate(ctx, existing, created);
  }
  existing.fileModifiedAt = toMs(input.fileModifiedAt) ?? existing.fileModifiedAt;
  existing.isFavorite = input.isFavorite ?? existing.isFavorite;
  if (input.rating !== undefined) {
    existing.rating = input.rating;
  }
  existing.description = input.description ?? existing.description;
  existing.visibility = input.visibility ?? existing.visibility;
  existing.isOffline = input.isOffline ?? existing.isOffline;
  if (input.make !== undefined) {
    existing.make = input.make;
  }
  if (input.model !== undefined) {
    existing.model = input.model;
  }
  existing.checksum = input.checksum ?? existing.checksum;
  const trashed = input.trashed ?? (input.isOffline === true ? true : undefined);
  if (trashed !== undefined) {
    existing.deletedAt = trashed ? (existing.deletedAt ?? now) : null;
  }
  existing.updatedAt = now;
  return { asset: existing, created: false };
}

export interface Generated {
  created: number;
  firstPath: string;
  lastPath: string;
}

/**
 * Adds `count` assets `prefix + 000000.ext`, `prefix + 000001.ext`, ... whose capture time moves
 * `stepMs` every `ties` assets. All or none: a taken path stops the call before anything is added.
 */
export function generateAssets(ctx: MockContext, input: GenerateInput): Generated {
  const library = libraryFor(ctx, input.library, input.prefix);
  const width = Math.max(6, String(input.count - 1).length);
  const pathOf = (index: number) =>
    `${input.prefix}${String(index).padStart(width, '0')}.${input.extension}`;
  for (let index = 0; index < input.count; index += 1) {
    if (ctx.data.assetsByPath.has(pathOf(index))) {
      throw badRequest(`an asset already exists at ${pathOf(index)}`);
    }
  }
  const start = Date.parse(input.startAt ?? DEFAULT_GENERATE_START);
  const now = ctx.clock.now();
  for (let index = 0; index < input.count; index += 1) {
    addAsset(ctx, {
      libraryId: library.id,
      ownerId: library.ownerId,
      originalPath: pathOf(index),
      fileCreatedAt: start + Math.floor(index / input.ties) * input.stepMs,
      now,
    });
  }
  return { created: input.count, firstPath: pathOf(0), lastPath: pathOf(input.count - 1) };
}
