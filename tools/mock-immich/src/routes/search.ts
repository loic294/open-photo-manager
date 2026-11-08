import { assetView } from '../assets.ts';
import { ok } from '../http.ts';
import { type Route, route } from '../router.ts';
import { searchMetadata } from '../search.ts';

export const searchRoutes: Route[] = [
  route('POST', '/search/metadata', { permission: 'asset.read' }, ({ ctx, user, body }) => {
    const outcome = searchMetadata(ctx, user, body);
    const detail = outcome.withExif ? 'exif' : 'basic';
    const items = outcome.items.map((asset) => assetView(ctx, asset, detail));
    return ok({
      albums: { total: 0, count: 0, items: [], facets: [] },
      assets: {
        total: items.length,
        count: items.length,
        items,
        facets: [],
        nextPage: outcome.nextPage,
        nextCursor: outcome.nextCursor,
      },
    });
  }),
];
