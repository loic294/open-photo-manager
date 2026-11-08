import type { Signal } from '@preact/signals-core';
import { html, nothing } from 'lit';
import { live } from 'lit/directives/live.js';
import type { RenameIssueField } from '@/api/generated/RenameIssueField';
import type { PatternChangeEvent } from '@/components/apm-pattern-editor';
import '@/components/apm-pattern-editor';
import type { SegmentedChangeEvent } from '@/components/apm-segmented';
import { renamePreviewAdapter } from '@/components/pattern-preview-adapter';
import '@/components/apm-segmented';
import type { SelectChangeEvent } from '@/components/apm-select';
import '@/components/apm-select';
import { renderPendingNotice } from '@/tools/framework/plan-notices';
import type { ToolContext } from '@/tools/framework/tool-context';
import { fileCount } from '@/tools/move/move-actions';
import {
  clean,
  countDigits,
  MATCH_MODES,
  matchIgnoreCase,
  matchMode,
  matchPattern,
  pattern,
  policy,
  previewKey,
  RENAME_POLICIES,
  renameParams,
  rewriteEnabled,
  rewriteFind,
  rewriteIgnoreCase,
  rewriteReplace,
  validation,
} from '@/tools/rename/rename-state';
import { CONFLICT_POLICIES } from '@/util/conflict-policies';
import { skipLabel } from '@/util/skip-codes';

function toggle(label: string, state: Signal<boolean>) {
  return html`<label class="flex items-center gap-2 text-xs"><input type="checkbox"
    class="toggle toggle-xs" .checked=${state.value}
    @change=${(event: Event) => {
      state.value = (event.target as HTMLInputElement).checked;
    }} />${label}</label>`;
}

function field(label: string, state: Signal<string>, invalid: boolean) {
  return html`<label class="flex min-w-0 flex-col gap-1 text-xs">${label}
    <input class="input input-sm w-full font-mono ${invalid ? 'input-error' : ''}"
      aria-label=${label} aria-invalid=${invalid ? 'true' : 'false'} .value=${live(state.value)}
      spellcheck="false" autocapitalize="off"
      @input=${(event: Event) => {
        state.value = (event.target as HTMLInputElement).value;
      }} /></label>`;
}

export function renameControls(ctx: ToolContext) {
  const params = renameParams(ctx);
  const key = previewKey(params);
  const current = validation.value.key === key && validation.value.status === 'invalid';
  const issues = (area: RenameIssueField) =>
    current ? validation.value.issues.filter((issue) => issue.field === area) : [];
  const messages = (area: RenameIssueField) =>
    issues(area).map(
      (issue) => html`<p class="text-xs text-error" role="alert">${issue.message}</p>`,
    );
  const stats = ctx.plan.stats;
  return html`
    <apm-segmented label="Filename match mode" .value=${matchMode.value}
      .options=${[
        { value: 'all', label: 'All files' },
        { value: 'glob', label: 'Glob' },
        { value: 'regex', label: 'Regular expression' },
      ]}
      @segmented-change=${(event: SegmentedChangeEvent) => {
        const mode = MATCH_MODES.find((entry) => entry === event.detail);
        if (mode) matchMode.value = mode;
      }}></apm-segmented>
    <apm-select label="If a file with the same name exists" show-label .value=${policy.value}
      .options=${CONFLICT_POLICIES.filter((entry) => entry.value !== 'replace').map((entry) => ({ ...entry, title: entry.help }))}
      @select-change=${(event: SelectChangeEvent) => {
        const next = RENAME_POLICIES.find((entry) => entry === event.detail.value);
        if (next) policy.value = next;
      }}></apm-select>
    ${
      ctx.plan.status === 'ready'
        ? html`<span class="text-xs" role="status">
      ${
        (stats.to_rename ?? 0) > 0
          ? `${fileCount(stats.to_rename ?? 0)} will be renamed`
          : (stats.matched ?? 0) === 0
            ? 'No matching files'
            : stats.already_in_place === stats.matched
              ? 'No names would change'
              : 'Nothing eligible to rename'
      }
      ${[
        'already_in_place',
        'identical_at_destination',
        'skipped_by_policy',
        'pattern_failed',
        'reserved_destination',
        'renamed_by_policy',
      ].map((code) =>
        (stats[code] ?? 0) > 0
          ? html`<span class="badge badge-ghost badge-xs">${stats[code]} ${skipLabel(code)}</span>`
          : nothing,
      )}
      ${(stats.renamed_by_policy ?? 0) > 0 ? 'Use {count} for explicit numbering of duplicate names.' : nothing}
    </span>`
        : nothing
    }
    ${renderPendingNotice(ctx, {
      code: 'metadata_pending',
      text: (count) => `${fileCount(count)} waiting for metadata`,
      title:
        'These files cannot be renamed until their metadata is known. The plan updates automatically.',
    })}
    <details class="dropdown min-w-0">
      <summary class="btn btn-ghost btn-sm">Match and clean rules${issues('matcher').length + issues('rewrite').length > 0 ? html`<span class="badge badge-error badge-xs">Fix rules</span>` : nothing}</summary>
      <div class="dropdown-content left-0 z-30 w-80 max-w-[85vw] max-h-80 overflow-auto rounded-box bg-base-100 p-3 shadow-lg flex flex-col gap-2">
        ${
          matchMode.value !== 'all'
            ? html`${field('Filename match pattern', matchPattern, issues('matcher').length > 0)}
          ${toggle('Ignore case when matching', matchIgnoreCase)}`
            : nothing
        }
        ${messages('matcher')}
        <p class="text-xs text-base-content/70">Glob matches the whole filename (*, ?, [abc]). Rust regular expressions search anywhere; use ^ and $ to anchor. No look-around or backreferences.</p>
        ${toggle('Clean numbered copies', clean)}
        <p class="text-xs text-base-content/70">Removes (1), -1, -1-1 and copy suffixes from the stem only. Off by default.</p>
        <details><summary class="cursor-pointer text-xs">Cleaning limitations</summary>
          <p class="text-xs">A single-digit date tail such as 2024-12-5 can be removed. _1 is not cleaned. Extensions stay unchanged.</p></details>
        ${toggle('Enable regex rewrite', rewriteEnabled)}
        ${
          rewriteEnabled.value
            ? html`${field('Rewrite find', rewriteFind, issues('rewrite').length > 0)}
          ${field('Rewrite replace', rewriteReplace, issues('rewrite').length > 0)}
          ${toggle('Ignore case when rewriting', rewriteIgnoreCase)}
          <p class="text-xs text-base-content/70">Replaces all non-overlapping matches in the cleaned stem. Use $1 or \${name} for captures, $$ for a literal dollar.</p>`
            : nothing
        }
        ${messages('rewrite')}
      </div>
    </details>
    <details class="dropdown min-w-0">
      <summary class="btn btn-ghost btn-sm">Counter digits: ${countDigits.value === 0 ? 'Exact number' : countDigits.value}</summary>
      <div class="dropdown-content right-0 z-30 w-72 max-w-[85vw] rounded-box bg-base-100 p-3 shadow-lg">
        <apm-select label="Counter digits" show-label .value=${String(countDigits.value)}
          .options=${Array.from({ length: 10 }, (_, width) => ({
            value: String(width),
            label:
              width === 0
                ? 'Exact number (1)'
                : `${width} digits (${String(1).padStart(width, '0')})`,
          }))}
          @select-change=${(event: SelectChangeEvent) => {
            const width = Number(event.detail.value);
            if (Number.isInteger(width) && width >= 0 && width <= 9) countDigits.value = width;
          }}></apm-select>
        <p class="text-xs text-base-content/70">Applies to {count}: restarts at 1 per folder and name group. Width is minimum zero-padding, never truncation. RAW/JPEG sets share a number.</p>
        ${messages('count_digits')}
      </div>
    </details>
    <div class="basis-full min-w-0">
      <apm-pattern-editor label="Name pattern" .value=${pattern.value} .source=${params?.source ?? null}
        .rules=${params?.rules ?? null} .requestKey=${key} .adapter=${renamePreviewAdapter} .limit=${5}
        @pattern-change=${(event: PatternChangeEvent) => {
          pattern.value = event.detail.value;
          if (event.detail.requestKey === previewKey(renameParams(ctx))) {
            validation.value = {
              key: event.detail.requestKey,
              status: event.detail.status,
              issues: event.detail.issues ?? [],
            };
          }
        }}></apm-pattern-editor>
    </div>`;
}
