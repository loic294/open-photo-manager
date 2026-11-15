import { html, nothing, type TemplateResult } from 'lit';
import { tooltip } from '@/components/tooltip';
import type { ToolContext } from '@/tools/framework/tool-context';

export function renderPendingNotice(
  ctx: ToolContext,
  notice: { code: string; text: (count: number) => string; title: string },
): TemplateResult | typeof nothing {
  const warning = ctx.plan.warnings.find((entry) => entry.code === notice.code);
  if (!warning) return nothing;
  return html`<div class="inline-flex flex-wrap items-center gap-1" ${tooltip(notice.title)}>
    <span class="badge badge-warning">${notice.text(warning.count)}</span>
    <button type="button" class="btn btn-ghost btn-xs" @click=${() => ctx.plan.retry()}>
      Check again
    </button>
  </div>`;
}
