import { api } from '@/api/client';
import type { FormatTokensResponse } from '@/api/generated/FormatTokensResponse';
import type { RenamePreviewRequest } from '@/api/generated/RenamePreviewRequest';
import type { RenamePreviewResponse } from '@/api/generated/RenamePreviewResponse';

export const previewRename = (request: RenamePreviewRequest, signal?: AbortSignal) =>
  api.post<RenamePreviewResponse>('/api/rename/preview', { body: request, signal });

let catalogue: Promise<FormatTokensResponse> | null = null;

export function fetchRenameTokens(): Promise<FormatTokensResponse> {
  catalogue ??= api.get<FormatTokensResponse>('/api/rename/tokens').catch((error: unknown) => {
    catalogue = null;
    throw error;
  });
  return catalogue;
}
