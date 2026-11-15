import { html, nothing } from 'lit';
import type { MatchChangeEvent } from '@/components/opm-match-mode';
import { tooltip } from '@/components/tooltip';
import '@/components/opm-match-mode';
import { matchMode, strictPrefix } from '@/tools/duplicates/duplicates-state';
import { renderPendingNotice } from '@/tools/framework/plan-notices';
import type { ToolContext } from '@/tools/framework/tool-context';
import { formatFileSize } from '@/util/format';
import { formatCount } from '@/util/format-count';
import { isMatchMode } from '@/util/match-modes';

export function duplicateControls(ctx: ToolContext) {
  const stats = ctx.plan.stats;
  const comparison = stats.comparison_matched ?? 0;
  const source = stats.source_matched ?? 0;
  return html`
    <opm-match-mode
      .mode=${matchMode.value}
      .strict=${strictPrefix.value}
      @match-change=${(event: MatchChangeEvent) => {
        const detail = event.detail;
        if (!detail || !isMatchMode(detail.mode) || typeof detail.strict !== 'boolean') return;
        matchMode.value = detail.mode;
        strictPrefix.value = detail.strict;
      }}
    ></opm-match-mode>
    ${
      ctx.plan.status === 'ready'
        ? html`<span
            class="text-sm text-base-content/70"
            role="status"
            ${tooltip(`${formatFileSize(stats.comparison_matched_bytes ?? 0)} comparison and ${formatFileSize(stats.source_matched_bytes ?? 0)} source files have a match`)}
          >${
            comparison === 0 && source === 0
              ? 'No duplicates found'
              : `${formatCount(comparison)} comparison and ${formatCount(source)} source files have a match`
          }</span>`
        : nothing
    }
    ${renderPendingNotice(ctx, {
      code: 'unhashed_files',
      text: (count) => `${formatCount(count)} files not hashed yet`,
      title:
        'Hashing runs in the background. These files cannot be matched until it finishes; matches update by themselves.',
    })}
  `;
}
