import { expect, it } from 'vitest';
import { FAILURE_REASONS } from './failure-codes';
import { skipLabel } from './skip-codes';

it('uses the same wording as shared results for every known code', () => {
  for (const [code, reason] of Object.entries(FAILURE_REASONS))
    expect(skipLabel(code)).toBe(reason);
  expect(skipLabel('future_skip')).toBe('Future skip');
});
