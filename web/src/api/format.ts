import { api } from '@/api/client';
import type { FormatPreviewRequest } from '@/api/generated/FormatPreviewRequest';
import type { FormatPreviewResponse } from '@/api/generated/FormatPreviewResponse';
import type { FormatTokensResponse } from '@/api/generated/FormatTokensResponse';

export const previewFormat = (request: FormatPreviewRequest, signal?: AbortSignal) =>
  api.post<FormatPreviewResponse>('/api/format/preview', { body: request, signal });

let catalogue: Promise<FormatTokensResponse> | null = null;

export function fetchFormatTokens(): Promise<FormatTokensResponse> {
  catalogue ??= api.get<FormatTokensResponse>('/api/format/tokens').catch((error: unknown) => {
    catalogue = null;
    throw error;
  });
  return catalogue;
}
