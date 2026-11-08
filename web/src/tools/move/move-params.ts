import type { MatchMode } from '@/api/generated/MatchMode';
import type { MoveParams } from '@/api/generated/MoveParams';
import type { MoveType } from '@/api/generated/MoveType';
import { volumeIdByName } from '@/stores/volumes';
import type { PlanParamsResult } from '@/tools/framework/tool-config';
import type { ToolContext } from '@/tools/framework/tool-context';
import type { PatternStatus, RulePolicy } from '@/tools/move/move-state';
import { normalizeFilter } from '@/util/filter';

export interface MoveRules {
  moveType: MoveType;
  pattern: string;
  patternStatus: PatternStatus;
  policy: RulePolicy;
  matchMode: MatchMode;
  strictPrefix: boolean;
}

export const MOVE_REASONS = {
  source: 'Choose the source folder: open it and press Use this folder.',
  destination: 'Choose the destination folder in the right pane and press Use this folder.',
};

export function buildMoveParams(
  ctx: ToolContext,
  rules: MoveRules,
  reasons = MOVE_REASONS,
): PlanParamsResult {
  const scope = ctx.scope('source');
  if (!scope) return { ready: false, reason: reasons.source };
  const destination = ctx.base('comparison');
  const volumeId = destination?.volume ? volumeIdByName(destination.volume) : undefined;
  if (!destination || volumeId === undefined) return { ready: false, reason: reasons.destination };
  if (rules.moveType === 'custom') {
    if (rules.patternStatus === 'checking')
      return { ready: false, reason: 'Checking the pattern...' };
    if (rules.patternStatus === 'invalid')
      return {
        ready: false,
        reason: 'Fix the folder pattern to see where the files will go.',
      };
  }
  return {
    ready: true,
    params: {
      move_type: rules.moveType,
      source: { scope, filter: normalizeFilter(ctx.filter('source')) },
      destination: { volume_id: volumeId, dir: destination.dir },
      pattern: rules.moveType === 'custom' ? rules.pattern : null,
      conflict_policy: rules.policy === 'default' ? null : rules.policy,
      match_mode: rules.moveType === 'keep_destination' ? rules.matchMode : null,
      strict_prefix:
        rules.moveType === 'keep_destination' &&
        rules.matchMode === 'name_prefix' &&
        rules.strictPrefix,
    } satisfies MoveParams,
  };
}
