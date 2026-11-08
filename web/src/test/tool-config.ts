import { signal } from '@preact/signals-core';
import { vi } from 'vitest';
import type { PlanRef } from '@/api/generated/PlanRef';
import type { PlanSide } from '@/api/generated/PlanSide';
import type { PlanBinding, PlanStatus, PlanWarningView } from '@/tools/framework/plan-binding';
import type { PaneSpec, ToolConfig } from '@/tools/framework/tool-config';

/** A pane that only browses: no base, no plan. */
export function plainPane(heading: string): PaneSpec {
  return { heading, baseMode: 'none', showMatches: false, planSide: null };
}

/**
 * A valid config (two plain panes, the filter on both, no plan and no actions) with `patch`
 * applied on top. `id` defaults to `'browse'`: pick another when a test needs a tool of its own.
 */
export function toolConfig(patch: Partial<ToolConfig> = {}): ToolConfig {
  return {
    id: 'browse',
    title: 'Test tool',
    summary: 'A tool for the tests.',
    panes: { source: plainPane('Source'), comparison: plainPane('Comparison') },
    filter: { applies: 'both' },
    ...patch,
  };
}

/**
 * A `PlanBinding` whose state a test sets through signals, so the view re-renders like it would
 * for the real plan client. Make the view use it by mocking `createPlanBinding`.
 */
export class FakePlan implements PlanBinding {
  readonly statusSignal = signal<PlanStatus>('idle');
  readonly errorSignal = signal<string | null>(null);
  readonly planIdSignal = signal<number | null>(null);
  readonly statsSignal = signal<Readonly<Record<string, number>>>({});
  readonly warningsSignal = signal<readonly PlanWarningView[]>([]);

  readonly stale = vi.fn<(ref: PlanRef) => void>();
  readonly retry = vi.fn<() => void>();
  readonly dispose = vi.fn<() => void>();

  get status(): PlanStatus {
    return this.statusSignal.value;
  }

  get error(): string | null {
    return this.errorSignal.value;
  }

  get planId(): number | null {
    return this.planIdSignal.value;
  }

  get stats(): Readonly<Record<string, number>> {
    return this.statsSignal.value;
  }

  get warnings(): readonly PlanWarningView[] {
    return this.warningsSignal.value;
  }

  ref(side: PlanSide): PlanRef | null {
    const planId = this.planIdSignal.value;
    return planId === null ? null : { plan_id: planId, side };
  }
}
