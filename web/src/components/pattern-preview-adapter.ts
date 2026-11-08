import { fetchFormatTokens, previewFormat } from '@/api/format';
import type { FormatTokensResponse } from '@/api/generated/FormatTokensResponse';
import type { PatternIssue } from '@/api/generated/PatternIssue';
import type { RenameIssue } from '@/api/generated/RenameIssue';
import type { RenameRules } from '@/api/generated/RenameRules';
import type { SideParams } from '@/api/generated/SideParams';
import { fetchRenameTokens, previewRename } from '@/api/rename';
import { skipLabel } from '@/util/skip-codes';

/** Presentation only: wire responses remain owned by their generated DTOs. */
export interface PatternPreview {
  error: PatternIssue | null;
  issues: RenameIssue[];
  appends_filename: boolean;
  note?: string;
  columns: readonly string[];
  samples: { source: string; destination: string; detail: string | null; prefix?: boolean }[];
}

export interface PatternPreviewAdapter {
  catalogue(): Promise<FormatTokensResponse>;
  preview(
    input: { pattern: string; source: SideParams; limit: number; rules: RenameRules | null },
    signal: AbortSignal,
  ): Promise<PatternPreview>;
}

export const formatPreviewAdapter: PatternPreviewAdapter = {
  catalogue: fetchFormatTokens,
  preview: async ({ pattern, source, limit }, signal) => {
    const response = await previewFormat({ pattern, source, limit }, signal);
    return {
      error: response.error,
      issues: [],
      appends_filename: response.appends_filename,
      columns: ['Source', 'Destination', 'Date from'],
      samples: response.samples.map((sample) => ({
        source: sample.source,
        destination: sample.skip ? skipLabel(sample.skip) : (sample.destination ?? ''),
        detail: sample.capture_src,
        prefix: !sample.skip,
      })),
    };
  },
};

export const renamePreviewAdapter: PatternPreviewAdapter = {
  catalogue: fetchRenameTokens,
  preview: async ({ pattern, source, limit, rules }, signal) => {
    if (!rules) throw new Error('Rename preview rules are missing.');
    const response = await previewRename({ rules: { ...rules, pattern }, source, limit }, signal);
    const issue = response.issues.find((entry) => entry.field === 'pattern');
    return {
      error: issue ?? null,
      issues: response.issues,
      appends_filename: false,
      columns: ['Original name', 'Proposed name / outcome', 'Result'],
      note: `Naming sample from volume ${response.volume_id ?? '—'}/${response.dir || ''}: ${response.samples.length} of ${response.matched} matched files (${response.folder_files} files in this folder). ${response.truncated ? 'Naming cap exceeded; counters in this sample may be incomplete. ' : ''}Other folders are not sampled. The built plan and ghost cards show final conflict-resolved names.`,
      samples: response.samples.map((sample) => ({
        source: sample.name,
        destination:
          sample.new_name ??
          (sample.outcome === 'pending'
            ? 'Waiting for metadata'
            : skipLabel(sample.error ?? 'pattern_failed')),
        detail: sample.outcome === 'unchanged' ? 'Unchanged' : sample.outcome,
      })),
    };
  },
};
