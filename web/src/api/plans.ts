import { type ApiClient, api } from '@/api/client';
import type { CreatePlanRequest } from '@/api/generated/CreatePlanRequest';
import type { ExecutePlanRequest } from '@/api/generated/ExecutePlanRequest';
import type { PlanDto } from '@/api/generated/PlanDto';
import type { PlanKind } from '@/api/generated/PlanKind';
import type { PlanSelectionRequest } from '@/api/generated/PlanSelectionRequest';
import type { PlanSide } from '@/api/generated/PlanSide';
import type { SelectionDto } from '@/api/generated/SelectionDto';
import type { TaskDto } from '@/api/generated/TaskDto';

export interface PlanFetchOptions {
  signal?: AbortSignal | undefined;
  client?: Pick<ApiClient, 'get' | 'post' | 'delete'>;
}

export function createPlan(
  kind: PlanKind,
  params: Record<string, unknown>,
  { replaces = null, signal, client = api }: PlanFetchOptions & { replaces?: number | null } = {},
): Promise<PlanDto> {
  const body: CreatePlanRequest = { kind, params, replaces };
  return client.post('/api/plans', { body, signal });
}

export function getPlan(
  id: number,
  { signal, client = api }: PlanFetchOptions = {},
): Promise<PlanDto> {
  return client.get(`/api/plans/${id}`, { signal });
}

export function deletePlan(
  id: number,
  { signal, client = api }: PlanFetchOptions = {},
): Promise<void> {
  return client.delete(`/api/plans/${id}`, { signal });
}

export function executePlan(
  id: number,
  request: ExecutePlanRequest,
  { signal, client = api }: PlanFetchOptions = {},
): Promise<TaskDto> {
  const body: ExecutePlanRequest = { options: request.options, only: request.only };
  return client.post(`/api/plans/${id}/execute`, { body, signal });
}

export function createPlanSelection(
  planId: number,
  side: PlanSide,
  {
    restrictIds = null,
    signal,
    client = api,
  }: PlanFetchOptions & {
    restrictIds?: number[] | null;
  } = {},
): Promise<SelectionDto> {
  const body: PlanSelectionRequest = { side, restrict_ids: restrictIds };
  return client.post(`/api/plans/${planId}/selection`, { body, signal });
}
