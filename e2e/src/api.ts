import type { BrowseRequest } from '../../web/src/api/generated/BrowseRequest';
import type { Facets } from '../../web/src/api/generated/Facets';
import type { FoldersResponse } from '../../web/src/api/generated/FoldersResponse';
import type { IdsResponse } from '../../web/src/api/generated/IdsResponse';
import type { IndexStatusDto } from '../../web/src/api/generated/IndexStatusDto';
import type { ItemsRequest } from '../../web/src/api/generated/ItemsRequest';
import type { ItemsResponse } from '../../web/src/api/generated/ItemsResponse';
import type { RescanRequest } from '../../web/src/api/generated/RescanRequest';
import type { SettingsDto } from '../../web/src/api/generated/SettingsDto';
import type { TaskDto } from '../../web/src/api/generated/TaskDto';
import type { VolumeDto } from '../../web/src/api/generated/VolumeDto';

export const emptyFilter = { cameras: [], ratings: [], flags: [], tags: [], selection_id: null };

export class Api {
  constructor(private origin: () => string) {}

  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const url = this.origin();
    const response = await fetch(`${url}${path}`, {
      method,
      headers: {
        Host: new URL(url).host,
        Origin: url,
        'x-apm-request': '1',
        'Content-Type': 'application/json',
      },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(15_000),
    });
    if (!response.ok)
      throw new Error(`${method} ${path}: ${response.status} ${await response.text()}`);
    if (response.status === 204) return undefined as T;
    return response.json() as Promise<T>;
  }

  settings = () => this.request<SettingsDto>('GET', '/api/settings');
  saveSettings = (settings: SettingsDto) =>
    this.request<SettingsDto>('PUT', '/api/settings', settings);
  tasks = () => this.request<TaskDto[]>('GET', '/api/tasks');
  indexStatus = () => this.request<IndexStatusDto>('GET', '/api/index/status');
  volumes = () => this.request<VolumeDto[]>('GET', '/api/volumes');
  folders = (request: BrowseRequest) =>
    this.request<FoldersResponse>('POST', '/api/browse/folders', request);
  items = (request: ItemsRequest) =>
    this.request<ItemsResponse>('POST', '/api/browse/items', request);
  ids = (request: ItemsRequest) => this.request<IdsResponse>('POST', '/api/browse/ids', request);
  facets = (request: BrowseRequest) => this.request<Facets>('POST', '/api/facets', request);
  rescan = (request: RescanRequest) => this.request<TaskDto>('POST', '/api/index/rescan', request);

  async listing(volume: string, dir: string): Promise<ItemsResponse> {
    const volumes = await this.volumes();
    const found = volumes.find((entry) => entry.name === volume);
    if (!found) throw new Error(`Unknown volume ${volume}`);
    return this.items({
      volume_id: found.id,
      dir,
      filter: emptyFilter,
      plan: null,
      sort: 'name',
      order: 'asc',
      offset: 0,
      limit: 500,
    });
  }
}
