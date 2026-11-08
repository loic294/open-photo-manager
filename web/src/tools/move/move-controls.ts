import { html, nothing } from 'lit';
import { errorMessage } from '@/api/errors';
import type { MatchChangeEvent } from '@/components/apm-match-mode';
import '@/components/apm-match-mode';
import type { PatternChangeEvent } from '@/components/apm-pattern-editor';
import '@/components/apm-pattern-editor';
import type { SelectChangeEvent } from '@/components/apm-select';
import '@/components/apm-select';
import { toast } from '@/components/toast';
import { loadSettings, settings } from '@/stores/settings';
import { renderPendingNotice } from '@/tools/framework/plan-notices';
import type { ToolContext } from '@/tools/framework/tool-context';
import { formatLocation } from '@/tools/move/move-actions';
import {
  isRulePolicy,
  matchMode,
  moveRules,
  moveType,
  type RuleState,
  strictPrefix,
} from '@/tools/move/move-state';
import { isMoveType, MOVE_TYPES } from '@/tools/move/move-types';
import { CONFLICT_POLICIES, conflictPolicyLabel } from '@/util/conflict-policies';
import { isSameOrBelow } from '@/util/dir-path';
import { formatFileSize } from '@/util/format';
import { formatCount } from '@/util/format-count';
import { isMatchMode } from '@/util/match-modes';
import { skipLabel } from '@/util/skip-codes';

const patternDisclosure = new WeakMap<RuleState, boolean>();

export function ensureSettings(): undefined {
  if (!settings.peek()) {
    void loadSettings().catch((error: unknown) => {
      toast.error(errorMessage(error, 'Could not load the default conflict policy'));
    });
  }
}

export function renderConflictPolicy(rules: RuleState) {
  const doc = settings.value;
  return html`<apm-select label="If a file with the same name exists" show-label
    .value=${rules.policy.value}
    .options=${[
      {
        value: 'default',
        label: doc ? `Default (${conflictPolicyLabel(doc.default_conflict_policy)})` : 'Default',
      },
      ...CONFLICT_POLICIES.map((entry) => ({ ...entry, title: entry.help })),
    ]}
    @select-change=${(event: SelectChangeEvent) => {
      if (isRulePolicy(event.detail.value)) rules.policy.value = event.detail.value;
    }}></apm-select>`;
}

export function renderPatternSection(
  ctx: ToolContext,
  rules: RuleState,
  options: { collapsible: boolean; limit: number },
) {
  const scope = ctx.scope('source');
  const destination = formatLocation(ctx.base('comparison'));
  const editor = html`<apm-pattern-editor .value=${rules.pattern.value}
    .source=${scope ? { scope, filter: ctx.filter('source') } : null}
    .basePrefix=${destination ? `${destination}/` : null} .limit=${options.limit}
    @pattern-change=${(event: PatternChangeEvent) => {
      rules.pattern.value = event.detail.value;
      rules.patternStatus.value = event.detail.status;
    }}></apm-pattern-editor>`;
  if (!options.collapsible) return html`<div class="basis-full min-w-0">${editor}</div>`;
  return html`<details class="collapse collapse-arrow basis-full min-w-0 bg-base-200 rounded-box"
    .open=${patternDisclosure.get(rules) ?? (rules.pattern.value === '' || rules.patternStatus.value === 'invalid')}>
    <summary class="collapse-title min-h-0 py-1 text-sm truncate"
      @click=${(event: Event) => {
        const details = (event.currentTarget as HTMLElement).closest('details');
        if (details) patternDisclosure.set(rules, !details.open);
      }}>Folder pattern: ${rules.pattern.value}</summary>
    <div class="collapse-content px-2">${editor}</div>
  </details>`;
}

export function renderMoveSummary(ctx: ToolContext) {
  if (ctx.plan.status !== 'ready') return nothing;
  const stats = ctx.plan.stats;
  const count = stats.to_move ?? 0;
  return html`<span class="inline-flex flex-wrap items-center gap-1 text-xs text-base-content/70" role="status">
    ${
      count > 0
        ? `${formatCount(count)} files (${formatFileSize(stats.bytes_to_move ?? 0)}) will move${(stats.new_folders ?? 0) > 0 ? `, ${formatCount(stats.new_folders ?? 0)} new folders` : ''}`
        : (stats.candidates ?? 0) > 0 && stats.already_in_place === stats.candidates
          ? `All ${formatCount(stats.candidates ?? 0)} files are already in place`
          : 'Nothing to move'
    }
    ${[
      'already_in_place',
      'identical_at_destination',
      'skipped_by_policy',
      'renamed',
      'replaced',
      'cross_volume_items',
    ].map((code) =>
      (stats[code] ?? 0) > 0
        ? html`<span class="badge badge-ghost badge-xs" title=${skipLabel(code)}>${formatCount(stats[code] ?? 0)} ${skipLabel(code)}</span>`
        : nothing,
    )}
  </span>`;
}

export function renderPendingNotices(ctx: ToolContext, options: { hash: boolean }) {
  return html`${renderPendingNotice(ctx, {
    code: 'metadata_pending',
    text: (count) => `${formatCount(count)} files not read yet`,
    title:
      'These files are not moved until their date and camera are known. The plan updates by itself.',
  })}${
    options.hash
      ? renderPendingNotice(ctx, {
          code: 'unhashed_files',
          text: (count) => `${formatCount(count)} files not hashed yet`,
          title:
            'Hashing runs in the background. The plan updates by itself when these files can be matched.',
        })
      : nothing
  }`;
}

export function destinationInsideSource(ctx: ToolContext): boolean {
  const source = ctx.base('source');
  const destination = ctx.base('comparison');
  return (
    ctx.recursive &&
    source?.volume !== null &&
    source !== null &&
    destination !== null &&
    source.volume === destination.volume &&
    destination.dir !== source.dir &&
    isSameOrBelow(destination.dir, source.dir)
  );
}

export function moveControls(ctx: ToolContext) {
  return html`
    <apm-select label="Move type" show-label .value=${moveType.value}
      .options=${Object.entries(MOVE_TYPES).map(([value, entry]) => ({ value, ...entry }))}
      @select-change=${(event: SelectChangeEvent) => {
        if (isMoveType(event.detail.value)) {
          patternDisclosure.delete(moveRules);
          moveType.value = event.detail.value;
        }
      }}></apm-select>
    ${renderConflictPolicy(moveRules)}
    ${
      moveType.value === 'keep_destination'
        ? html`<apm-match-mode
      .mode=${matchMode.value} .strict=${strictPrefix.value}
      @match-change=${(event: MatchChangeEvent) => {
        if (!isMatchMode(event.detail.mode) || typeof event.detail.strict !== 'boolean') return;
        matchMode.value = event.detail.mode;
        strictPrefix.value = event.detail.strict;
      }}></apm-match-mode><span class="text-xs text-base-content/70">Files without a match go to the destination folder.</span>`
        : nothing
    }
    ${
      moveType.value === 'keep_source' && destinationInsideSource(ctx)
        ? html`<span
      class="badge badge-warning" title="Files already moved there would be moved again, one folder deeper, the next time you run this">Destination is inside the source</span>`
        : nothing
    }
    ${renderMoveSummary(ctx)}${renderPendingNotices(ctx, { hash: moveType.value === 'keep_destination' && matchMode.value === 'hash' })}
    ${moveType.value === 'custom' ? renderPatternSection(ctx, moveRules, { collapsible: true, limit: 3 }) : nothing}
  `;
}
