import { type ApiClient, api } from '@/api/client';
import type { CreateSelectionRequest } from '@/api/generated/CreateSelectionRequest';
import type { SelectionDto } from '@/api/generated/SelectionDto';

export interface SelectionFetchOptions {
  signal?: AbortSignal | undefined;
  client?: Pick<ApiClient, 'get' | 'post'>;
}

export function createSelection(
  ids: number[],
  label: string | null = null,
  { signal, client = api }: SelectionFetchOptions = {},
): Promise<SelectionDto> {
  const body: CreateSelectionRequest = { ids, label };
  return client.post('/api/selections', { body, signal });
}

export function getSelection(
  id: number,
  { signal, client = api }: SelectionFetchOptions = {},
): Promise<SelectionDto> {
  return client.get(`/api/selections/${id}`, { signal });
}
