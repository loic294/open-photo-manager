import { api } from '@/api/client';
import type { IsmBindingRequest } from '@/api/generated/IsmBindingRequest';
import type { IsmConnectRequest } from '@/api/generated/IsmConnectRequest';
import type { IsmInfoDto } from '@/api/generated/IsmInfoDto';
import type { IsmStatusDto } from '@/api/generated/IsmStatusDto';
import type { IsmSyncFolderRequest } from '@/api/generated/IsmSyncFolderRequest';
import type { IsmTrashDto } from '@/api/generated/IsmTrashDto';
import type { IsmTrashPurgeRequest } from '@/api/generated/IsmTrashPurgeRequest';
import type { IsmTrashRestoreRequest } from '@/api/generated/IsmTrashRestoreRequest';
import type { TaskDto } from '@/api/generated/TaskDto';

export const fetchIsmStatus = (signal?: AbortSignal) =>
  api.get<IsmStatusDto>('/api/ism/status', { signal });
export const connectIsm = (body: IsmConnectRequest) =>
  api.put<IsmInfoDto>('/api/ism/connection', { body });
export const testIsm = () => api.post<IsmInfoDto>('/api/ism/test');
export const disconnectIsm = () => api.delete<IsmStatusDto>('/api/ism/connection');
export const stageIsmBinding = (body: IsmBindingRequest) =>
  api.put<IsmStatusDto>('/api/ism/bindings', { body });
/** `POST /api/ism/sync`: asks ISM to run an Immich scan for one bound folder. */
export const syncIsmFolder = (body: IsmSyncFolderRequest) =>
  api.post<TaskDto>('/api/ism/sync', { body });

/** `GET /api/trash/ism`: the ISM trash of a bound volume. 404 `ism_not_bound` otherwise. */
export const listIsmTrash = (volumeId: number, signal?: AbortSignal) =>
  api.get<IsmTrashDto>('/api/trash/ism', { query: { volume_id: volumeId }, signal });
/** `POST /api/trash/ism/restore`: a task that asks ISM to restore the items. */
export const restoreIsmTrash = (body: IsmTrashRestoreRequest) =>
  api.post<TaskDto>('/api/trash/ism/restore', { body });
/**
 * `POST /api/trash/ism/purge`: a task that asks ISM to delete the items for good. Needs
 * `confirm: true` (400 `confirmation_required`) and the token's purge grant (403 `forbidden`).
 */
export const purgeIsmTrash = (body: IsmTrashPurgeRequest) =>
  api.post<TaskDto>('/api/trash/ism/purge', { body });
