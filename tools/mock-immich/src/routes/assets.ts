import { unlinkSync } from 'node:fs';
import { assetView, removeAsset } from '../assets.ts';
import { ApiError, badRequest, noContent, ok } from '../http.ts';
import type { MockContext } from '../mock.ts';
import { toRealPath } from '../mounts.ts';
import { type Route, route } from '../router.ts';
import type { Asset, User } from '../types.ts';
import { VISIBILITIES } from '../types.ts';
import { createChecker } from '../validate.ts';

/** The caller's own, unlocked assets: what an API key may reach. */
export const accessibleAsset = (ctx: MockContext, user: User, id: string): Asset | undefined => {
  const asset = ctx.data.assets.get(id);
  return asset !== undefined && asset.ownerId === user.id && asset.visibility !== 'locked'
    ? asset
    : undefined;
};

/** Immich refuses the whole call when one id is out of reach. */
function requireAssets(ctx: MockContext, user: User, ids: string[], access: string): Asset[] {
  const assets = new Map<string, Asset>();
  for (const id of ids) {
    const asset = accessibleAsset(ctx, user, id);
    if (asset === undefined) {
      throw badRequest(`Not found or no ${access} access`);
    }
    assets.set(id, asset);
  }
  return [...assets.values()];
}

/**
 * A forced delete removes the original file of an online asset in a real Immich, which is what
 * the application must never ask for: record it, and remove the file for real when a mount says
 * where it lives, so a test can see the damage.
 */
function forceDelete(ctx: MockContext, asset: Asset): void {
  if (!asset.isOffline) {
    let fileRemoved = false;
    const real = toRealPath(ctx.mounts, asset.originalPath);
    if (real !== undefined) {
      try {
        unlinkSync(real);
        fileRemoved = true;
      } catch {
        // Already gone, or not a plain file: nothing was lost.
      }
    }
    ctx.data.violations.push({
      kind: 'online-asset-force-deleted',
      assetId: asset.id,
      path: asset.originalPath,
      libraryId: asset.libraryId,
      fileRemoved,
      at: new Date().toISOString(),
    });
  }
  removeAsset(ctx, asset);
}

export const assetRoutes: Route[] = [
  route('GET', '/assets/:id', { permission: 'asset.read' }, ({ ctx, user, params }) => {
    const asset = accessibleAsset(ctx, user, params.id ?? '');
    if (asset === undefined) {
      throw new ApiError(ctx.settings.unknownAssetStatus, 'Not found or no asset.read access');
    }
    return ok(assetView(ctx, asset, 'full'));
  }),

  route('PUT', '/assets', { permission: 'asset.update' }, ({ ctx, user, body }) => {
    const c = createChecker(body);
    c.only(['ids', 'isFavorite', 'rating', 'visibility', 'description']);
    const ids = c.uuidList('ids');
    const isFavorite = c.boolean('isFavorite');
    const rating = c.nullableInteger('rating', { min: -1, max: 5 });
    const visibility = c.oneOf('visibility', VISIBILITIES);
    const description = c.string('description');
    c.done();
    const assets = requireAssets(ctx, user, ids, 'asset.update');
    const now = ctx.clock.now();
    for (const asset of assets) {
      asset.isFavorite = isFavorite ?? asset.isFavorite;
      if (rating !== undefined) {
        asset.rating = rating;
      }
      asset.visibility = visibility ?? asset.visibility;
      asset.description = description ?? asset.description;
      asset.updatedAt = now;
    }
    return noContent();
  }),

  route('DELETE', '/assets', { permission: 'asset.delete' }, ({ ctx, user, body }) => {
    const c = createChecker(body);
    c.only(['ids', 'force']);
    const ids = c.uuidList('ids');
    const force = c.boolean('force') ?? false;
    c.done();
    const assets = requireAssets(ctx, user, ids, 'asset.delete');
    const now = ctx.clock.now();
    for (const asset of assets) {
      if (force) {
        forceDelete(ctx, asset);
      } else {
        asset.deletedAt ??= now;
        asset.updatedAt = now;
      }
    }
    return noContent();
  }),
];
