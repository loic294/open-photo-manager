import { api } from '@/api/client';
import type { ImmichConnectionDto } from '@/api/generated/ImmichConnectionDto';
import type { ImmichConnectRequest } from '@/api/generated/ImmichConnectRequest';
import type { ImmichDetectPrefixRequest } from '@/api/generated/ImmichDetectPrefixRequest';
import type { ImmichDetectPrefixResponse } from '@/api/generated/ImmichDetectPrefixResponse';
import type { ImmichDisconnectResult } from '@/api/generated/ImmichDisconnectResult';
import type { ImmichExcludeTrashResult } from '@/api/generated/ImmichExcludeTrashResult';
import type { ImmichLibrariesResponse } from '@/api/generated/ImmichLibrariesResponse';
import type { ImmichMappingEntry } from '@/api/generated/ImmichMappingEntry';
import type { ImmichMappingsResponse } from '@/api/generated/ImmichMappingsResponse';
import type { ImmichStatusDto } from '@/api/generated/ImmichStatusDto';
import type { ImmichSyncRequest } from '@/api/generated/ImmichSyncRequest';
import type { OutboxDismissResult } from '@/api/generated/OutboxDismissResult';
import type { OutboxPage } from '@/api/generated/OutboxPage';
import type { OutboxRetryResult } from '@/api/generated/OutboxRetryResult';
import type { OutboxState } from '@/api/generated/OutboxState';
import type { TaskDto } from '@/api/generated/TaskDto';

export const fetchImmichStatus = ({
  check = false,
  signal,
}: {
  check?: boolean;
  signal?: AbortSignal;
} = {}) => api.get<ImmichStatusDto>('/api/immich/status', { query: { check }, signal });
export const connectImmich = (body: ImmichConnectRequest) =>
  api.post<ImmichConnectionDto>('/api/immich/connect', { body });
export const disconnectImmich = () => api.delete<ImmichDisconnectResult>('/api/immich/connection');
export const fetchImmichLibraries = () => api.get<ImmichLibrariesResponse>('/api/immich/libraries');
export const saveImmichMappings = (mappings: ImmichMappingEntry[]) =>
  api.put<ImmichMappingsResponse>('/api/immich/mappings', { body: { mappings } });
export const detectImmichPrefix = (body: ImmichDetectPrefixRequest, signal?: AbortSignal) =>
  api.post<ImmichDetectPrefixResponse>('/api/immich/detect-prefix', { body, signal });
export const excludeImmichTrash = () =>
  api.post<ImmichExcludeTrashResult>('/api/immich/exclude-trash');
export const requestImmichSync = (body: ImmichSyncRequest) =>
  api.post<TaskDto>('/api/immich/sync', { body });
export const fetchImmichOutbox = ({
  state,
  offset = 0,
  limit = 50,
  signal,
}: {
  state?: OutboxState;
  offset?: number;
  limit?: number;
  signal?: AbortSignal;
} = {}) => api.get<OutboxPage>('/api/immich/outbox', { query: { state, offset, limit }, signal });
export const retryImmichOutbox = (ids?: number[]) =>
  api.post<OutboxRetryResult>('/api/immich/outbox/retry', { body: { ids: ids ?? null } });
export const dismissImmichOutbox = (ids: number[]) =>
  api.post<OutboxDismissResult>('/api/immich/outbox/dismiss', { body: { ids } });
