import { isBoolean, persistedSignal } from '@/stores/persisted';
import { isMatchMode } from '@/util/match-modes';

export const matchMode = persistedSignal('duplicates.mode', 'hash', isMatchMode);
export const strictPrefix = persistedSignal('duplicates.strict', false, isBoolean);
