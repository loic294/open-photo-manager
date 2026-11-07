import { type ApiClient, api } from '@/api/client';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { FileItem } from '@/api/generated/FileItem';
import type { IdsResponse } from '@/api/generated/IdsResponse';
import type { ItemsRequest } from '@/api/generated/ItemsRequest';
import type { ItemsResponse } from '@/api/generated/ItemsResponse';
import type { SortKey } from '@/api/generated/SortKey';
import type { SortOrder } from '@/api/generated/SortOrder';

export interface FetchOptions {
  /** Aborting rejects with an `AbortError` (`isAbortError`): not a failure. */
  signal?: AbortSignal | undefined;
  /** Defaults to the shared `api`; tests inject a fake. */
  client?: Pick<ApiClient, 'post'>;
}

/** The server clamps `limit` to its own maximum; this is the size the grid asks for. */
export const ITEMS_PAGE_SIZE = 200;

/** Only the named fields are sent: a caller's request may carry extras (a view model, a router state). */
function itemsBody(
  request: BrowseRequest,
  sort: SortKey,
  order: SortOrder,
  offset: number,
  limit: number,
): ItemsRequest {
  return {
    volume_id: request.volume_id,
    dir: request.dir,
    filter: request.filter,
    plan: request.plan,
    sort,
    order,
    offset,
    limit,
  };
}

/** One page of a listing: `POST /api/browse/items`. `total` ignores `offset` and `limit`. */
export function fetchItems(
  request: BrowseRequest,
  sort: SortKey,
  order: SortOrder,
  offset: number,
  limit: number,
  options: FetchOptions = {},
): Promise<ItemsResponse> {
  const { signal, client = api } = options;
  return client.post<ItemsResponse>('/api/browse/items', {
    body: itemsBody(request, sort, order, offset, limit),
    signal,
  });
}

/**
 * Every real file id of a listing, in display order (ghosts are not in it): `POST
 * /api/browse/ids`. For select-all and the viewer. The server ignores `offset` and `limit` here.
 */
export async function fetchIds(
  request: BrowseRequest,
  sort: SortKey,
  order: SortOrder,
  options: FetchOptions = {},
): Promise<number[]> {
  const { signal, client = api } = options;
  const response = await client.post<IdsResponse>('/api/browse/ids', {
    body: itemsBody(request, sort, order, 0, ITEMS_PAGE_SIZE),
    signal,
  });
  return response.ids;
}

/** Thumbnail renditions the server renders: `sm` has a longest edge of 320 px, `lg` of 800 px. */
export type ThumbSize = 'sm' | 'lg';

/** Longest edge, in device pixels, of the `sm` rendition. */
export const THUMB_SM_PX = 320;

/**
 * The smallest rendition that is still sharp for a thumbnail shown `cssPx` wide:
 * `sm` while `cssPx * devicePixelRatio <= 320`, else `lg`.
 */
export function thumbSizeFor(cssPx: number, pixelRatio?: number): ThumbSize {
  const ratio = pixelRatio ?? window.devicePixelRatio;
  return cssPx * (ratio > 0 ? ratio : 1) <= THUMB_SM_PX ? 'sm' : 'lg';
}

/**
 * URL of a file's thumbnail. `v` is the file's modification time: it is only the browser's cache
 * key (the response is immutable), so a changed file is fetched again. A ghost item carries the id
 * and mtime of the real file it stands for, so this works for it too.
 */
export function thumbUrl(item: Pick<FileItem, 'id' | 'mtime'>, size: ThumbSize): string {
  return `/api/files/${item.id}/thumb?size=${size}&v=${item.mtime}`;
}
