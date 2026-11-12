import { afterEach, expect, it } from 'vitest';
import { createRuleState } from './move-state';

afterEach(() => localStorage.clear());

it('persists independent rules and rejects invalid policies', () => {
  localStorage.setItem('opm.v1.invalid.policy', '"erase"');
  const left = createRuleState('left');
  const right = createRuleState('right');
  const invalid = createRuleState('invalid');
  expect(invalid.policy.value).toBe('default');
  expect(right.pattern.value).toBe('{YYYY}/{mm}');
  left.pattern.value = '{camera}';
  left.policy.value = 'rename';
  left.patternStatus.value = 'invalid';
  expect(right.pattern.value).toBe('{YYYY}/{mm}');
  expect(localStorage.getItem('opm.v1.left.pattern')).toBe('"{camera}"');
  expect(localStorage.getItem('opm.v1.left.policy')).toBe('"rename"');
  expect(localStorage.getItem('opm.v1.left.patternStatus')).toBeNull();
});
