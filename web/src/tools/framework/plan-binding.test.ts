import { describe, expect, it } from 'vitest';
import { toolConfig } from '@/test/tool-config';
import { createPlanBinding, NO_PLAN } from '@/tools/framework/plan-binding';
import { createToolContext } from '@/tools/framework/tool-context';

describe('NO_PLAN', () => {
  it('is empty: nothing to show, nothing to build', () => {
    expect(NO_PLAN.status).toBe('none');
    expect(NO_PLAN.error).toBeNull();
    expect(NO_PLAN.planId).toBeNull();
    expect(NO_PLAN.stats).toEqual({});
    expect(NO_PLAN.warnings).toEqual([]);
    expect(NO_PLAN.ref('source')).toBeNull();
    expect(NO_PLAN.ref('comparison')).toBeNull();
  });

  it('ignores a stale pane and a retry, and has nothing to dispose', () => {
    expect(() => NO_PLAN.stale({ plan_id: 4, side: 'source' })).not.toThrow();
    expect(() => NO_PLAN.retry()).not.toThrow();
    expect(NO_PLAN.dispose).toBeUndefined();
  });

  it('cannot be changed: every tool shares it', () => {
    expect(Object.isFrozen(NO_PLAN)).toBe(true);
    expect(Object.isFrozen(NO_PLAN.stats)).toBe(true);
    expect(Object.isFrozen(NO_PLAN.warnings)).toBe(true);
  });
});

describe('createPlanBinding', () => {
  it('gives a tool without a plan NO_PLAN', () => {
    const ctx = createToolContext(toolConfig(), document.createElement('div'));
    expect(createPlanBinding(ctx)).toBe(NO_PLAN);
    expect(ctx.plan).toBe(NO_PLAN);
  });
});
