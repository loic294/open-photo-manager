import { badRequest, ok } from '../http.ts';
import type { MockContext } from '../mock.ts';
import { type Route, route } from '../router.ts';
import type { Album, Tag } from '../types.ts';
import { iso, isUuid } from '../util.ts';
import { createChecker } from '../validate.ts';
import { accessibleAsset } from './assets.ts';

/** `AlbumResponseDto`; a trashed asset is in the album but not counted. */
export function albumView(ctx: MockContext, album: Album) {
  let assetCount = 0;
  let thumbnail: string | null = null;
  for (const id of album.assetIds) {
    const asset = ctx.data.assets.get(id);
    if (asset !== undefined && asset.deletedAt === null) {
      assetCount += 1;
      thumbnail ??= id;
    }
  }
  return {
    id: album.id,
    albumName: album.name,
    description: '',
    assetCount,
    shared: false,
    ownerId: album.ownerId,
    createdAt: iso(album.createdAt),
    updatedAt: iso(album.updatedAt),
    albumThumbnailAssetId: thumbnail,
    albumUsers: [],
    hasSharedLink: false,
    isActivityEnabled: true,
  };
}

export const tagView = (tag: Tag) => ({
  id: tag.id,
  name: tag.name,
  value: tag.value,
  createdAt: iso(tag.createdAt),
  updatedAt: iso(tag.updatedAt),
});

/** Albums and tags belong to nobody in particular here: every user sees and uses all of them. */
export const albumRoutes: Route[] = [
  route('GET', '/albums', { permission: 'album.read' }, ({ ctx, query }) => {
    const assetId = query.get('assetId');
    if (assetId !== null && !isUuid(assetId)) {
      throw badRequest('assetId must be a UUID');
    }
    const albums = [...ctx.data.albums.values()].filter(
      (album) => assetId === null || album.assetIds.has(assetId),
    );
    return ok(albums.map((album) => albumView(ctx, album)));
  }),

  route(
    'PUT',
    '/albums/:id/assets',
    { permission: 'albumAsset.create' },
    ({ ctx, user, params, body }) => {
      const c = createChecker(body);
      c.only(['ids']);
      const ids = c.uuidList('ids');
      c.done();
      const album = ctx.data.albums.get(params.id ?? '');
      if (album === undefined) {
        throw badRequest('Not found or no albumAsset.create access');
      }
      let added = false;
      const results = ids.map((id) => {
        if (accessibleAsset(ctx, user, id) === undefined) {
          return { id, success: false, error: 'no_permission' };
        }
        if (album.assetIds.has(id)) {
          return { id, success: false, error: 'duplicate' };
        }
        album.assetIds.add(id);
        added = true;
        return { id, success: true };
      });
      if (added) {
        album.updatedAt = ctx.clock.now();
      }
      return ok(results);
    },
  ),
];

export const tagRoutes: Route[] = [
  route('GET', '/tags', { permission: 'tag.read' }, ({ ctx }) =>
    ok([...ctx.data.tags.values()].map(tagView)),
  ),

  route('PUT', '/tags/assets', { permission: 'tag.asset' }, ({ ctx, user, body }) => {
    const c = createChecker(body);
    c.only(['assetIds', 'tagIds']);
    const assetIds = c.uuidList('assetIds');
    const tagIds = c.uuidList('tagIds');
    c.done();
    const tags = [...new Set(tagIds)].flatMap((id) => ctx.data.tags.get(id) ?? []);
    const assets = [...new Set(assetIds)].flatMap((id) => accessibleAsset(ctx, user, id) ?? []);
    for (const tag of tags) {
      for (const asset of assets) {
        tag.assetIds.add(asset.id);
      }
    }
    return ok({ count: tags.length * assets.length });
  }),
];
