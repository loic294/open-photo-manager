import { describeFailure } from '@/util/failure-codes';

/** Preview and result details use the same vocabulary. */
export function skipLabel(code: string): string {
  return describeFailure(code, '');
}
