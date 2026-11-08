import { type Signal, signal } from '@preact/signals-core';
import type { ConflictPolicy } from '@/api/generated/ConflictPolicy';
import { isBoolean, isString, persistedSignal } from '@/stores/persisted';
import { isConflictPolicy } from '@/util/conflict-policies';
import { isMatchMode } from '@/util/match-modes';
import { isMoveType } from './move-types';

export type PatternStatus = 'idle' | 'checking' | 'valid' | 'invalid';
export type RulePolicy = 'default' | ConflictPolicy;
export interface RuleState {
  pattern: Signal<string>;
  policy: Signal<RulePolicy>;
  patternStatus: Signal<PatternStatus>;
}

export function isRulePolicy(value: unknown): value is RulePolicy {
  return value === 'default' || isConflictPolicy(value);
}

export function createRuleState(prefix: string): RuleState {
  return {
    pattern: persistedSignal(`${prefix}.pattern`, '{YYYY}/{mm}', isString),
    policy: persistedSignal<RulePolicy>(`${prefix}.policy`, 'default', isRulePolicy),
    patternStatus: signal<PatternStatus>('idle'),
  };
}

export const moveRules = createRuleState('move');
export const moveType = persistedSignal('move.type', 'keep_source', isMoveType);
export const matchMode = persistedSignal('move.mode', 'hash', isMatchMode);
export const strictPrefix = persistedSignal('move.strict', false, isBoolean);
