import type { DuplicateParams } from '@/api/generated/DuplicateParams';
import { matchMode, strictPrefix } from '@/tools/duplicates/duplicates-state';
import type { PlanParamsResult } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import { normalizeFilter } from '@/util/filter';

export function duplicateParams(ctx: ToolContext): PlanParamsResult {
  const source = ctx.scope('source');
  const comparison = ctx.scope('comparison');
  if (!source || !comparison) {
    return {
      ready: false,
      reason: 'Open a volume or folder in both panes to look for duplicates.',
    };
  }
  const mode = matchMode.value;
  return {
    ready: true,
    params: {
      mode,
      strict_prefix: mode === 'name_prefix' && strictPrefix.value,
      source: { scope: source, filter: normalizeFilter(ctx.filter('source')) },
      comparison: { scope: comparison, filter: normalizeFilter(ctx.filter('comparison')) },
    } satisfies DuplicateParams,
  };
}
