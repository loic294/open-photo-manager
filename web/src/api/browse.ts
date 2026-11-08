import { api } from '@/api/client';
import type { FetchOptions } from '@/api/files';
import type { BrowseRequest } from '@/api/generated/BrowseRequest';
import type { Filter } from '@/api/generated/Filter';
import type { FoldersResponse } from '@/api/generated/FoldersResponse';
import type { PlanRef } from '@/api/generated/PlanRef';
import type { PaneLocation } from '@/stores/nav';
import { volumeIdByName } from '@/stores/volumes';

/**
 * The ONE place a `BrowseRequest` is built from what a pane shows: the pane, the tool view's facet
 * and viewer requests all call this, nobody writes the object by hand.
 *
 * - No volume chosen (`location.volume === null`) is the volumes root: `volume_id: null` and
 *   `dir: ''`, which needs no volume list.
 * - A volume NAME is translated to its id with `volumeIdByName`. The result is `null` while the
 *   volumes are not loaded and when the name is unknown: the caller tells the two apart with
 *   `volumesStatus`. Reactive: read it inside `render()` and the element follows the volumes.
 */
export function requestFor(
  location: PaneLocation,
  filter: Filter,
  plan: PlanRef | null,
): BrowseRequest | null {
  if (location.volume === null) return { volume_id: null, dir: '', filter, plan };
  const volumeId = volumeIdByName(location.volume);
  if (volumeId === undefined) return null;
  return { volume_id: volumeId, dir: location.dir, filter, plan };
}

/** Only the named fields are sent: a caller's request may carry extras (a view model, a router state). */
function foldersBody(request: BrowseRequest): BrowseRequest {
  return {
    volume_id: request.volume_id,
    dir: request.dir,
    filter: request.filter,
    plan: request.plan,
  };
}

/**
 * The subfolders of a directory and the footer counts: `POST /api/browse/folders`. At the volumes
 * root (`volume_id: null`, `dir: ''`) the volumes are the folders. Rejects with `folder_not_found`
 * for a directory that is not indexed and `plan_not_found` for a plan the server forgot.
 */
export function fetchFolders(
  request: BrowseRequest,
  options: FetchOptions = {},
): Promise<FoldersResponse> {
  const { signal, client = api } = options;
  return client.post<FoldersResponse>('/api/browse/folders', {
    body: foldersBody(request),
    signal,
  });
}
