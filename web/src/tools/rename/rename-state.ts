import { signal } from '@preact/signals-core';
import type { RenameIssue } from '@/api/generated/RenameIssue';
import type { RenameParams } from '@/api/generated/RenameParams';
import type { RenameRules } from '@/api/generated/RenameRules';
import { isBoolean, isString, oneOf, persistedSignal } from '@/stores/persisted';
import type { ToolContext } from '@/tools/framework/tool-context';
import { normalizeFilter } from '@/util/filter';

export const MATCH_MODES = ['all', 'glob', 'regex'] as const;
export const RENAME_POLICIES = ['smart', 'skip', 'rename'] as const;
export const matchMode = persistedSignal('rename.matchMode', 'all', oneOf(MATCH_MODES));
export const matchPattern = persistedSignal('rename.matchPattern', '', isString);
export const matchIgnoreCase = persistedSignal('rename.matchIgnoreCase', true, isBoolean);
export const clean = persistedSignal('rename.clean', false, isBoolean);
export const rewriteEnabled = persistedSignal('rename.rewriteEnabled', false, isBoolean);
export const rewriteFind = persistedSignal('rename.rewriteFind', '', isString);
export const rewriteReplace = persistedSignal('rename.rewriteReplace', '', isString);
export const rewriteIgnoreCase = persistedSignal('rename.rewriteIgnoreCase', true, isBoolean);
export const pattern = persistedSignal('rename.pattern', '{name}', isString);
export const countDigits = persistedSignal(
  'rename.countDigits',
  0,
  (value): value is number =>
    typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 9,
);
export const policy = persistedSignal('rename.policy', 'smart', oneOf(RENAME_POLICIES));

export const validation = signal<{
  key: string;
  status: 'checking' | 'valid' | 'invalid' | 'idle';
  issues: RenameIssue[];
}>({ key: '', status: 'idle', issues: [] });

export function currentRules(): RenameRules {
  return {
    matcher: {
      mode: matchMode.value,
      pattern: matchPattern.value,
      ignore_case: matchIgnoreCase.value,
    },
    clean: clean.value,
    rewrite: rewriteEnabled.value
      ? {
          find: rewriteFind.value,
          replace: rewriteReplace.value,
          ignore_case: rewriteIgnoreCase.value,
        }
      : null,
    pattern: pattern.value,
    count_digits: countDigits.value,
  };
}

export function renameParams(ctx: ToolContext): RenameParams | null {
  const scope = ctx.scope('source');
  return scope
    ? {
        source: { scope, filter: normalizeFilter(ctx.filter('source')) },
        rules: currentRules(),
        conflict_policy: policy.value,
      }
    : null;
}

/** Includes every naming dependency, but not conflict policy (resolved by the built plan). */
export function previewKey(params: RenameParams | null): string {
  return JSON.stringify(params ? [params.source, params.rules] : null);
}

export function validationReason(ctx: ToolContext): string | null {
  const params = renameParams(ctx);
  if (!params) return 'Open a source folder to rename its files.';
  const result = validation.value;
  if (result.key !== previewKey(params) || result.status === 'checking')
    return 'Checking rename rules...';
  if (result.status === 'invalid') return 'Fix the rename rules before building a plan.';
  if (result.status !== 'valid')
    return 'Check the rename rules; retry the live preview if unavailable.';
  return null;
}
