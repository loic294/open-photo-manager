import { describe, expect, it } from 'vitest';
import { CONFLICT_POLICIES, conflictPolicyLabel, isConflictPolicy } from './conflict-policies';

describe('conflict policies', () => {
  it('shares ordered labels and help', () => {
    expect(CONFLICT_POLICIES.map((entry) => entry.value)).toEqual([
      'smart',
      'skip',
      'rename',
      'replace',
    ]);
    for (const entry of CONFLICT_POLICIES) {
      expect(conflictPolicyLabel(entry.value)).toBe(entry.label);
      expect(entry.help.length).toBeGreaterThan(0);
      expect(isConflictPolicy(entry.value)).toBe(true);
    }
    expect(isConflictPolicy('default')).toBe(false);
    expect(isConflictPolicy(null)).toBe(false);
  });
});
