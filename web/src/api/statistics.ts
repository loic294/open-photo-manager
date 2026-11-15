import { api } from '@/api/client';
import type { StatisticsQuery } from '@/api/generated/StatisticsQuery';
import type { StatisticsResponse } from '@/api/generated/StatisticsResponse';

export function getStatistics(query: StatisticsQuery, signal?: AbortSignal) {
  return api.get<StatisticsResponse>('/api/statistics', { query: { ...query }, signal });
}
