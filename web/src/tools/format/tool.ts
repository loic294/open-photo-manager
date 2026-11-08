import { html } from 'lit';
import { formatRules } from '@/tools/format/format-state';
import { followSource } from '@/tools/framework/follow-source';
import { defineTool } from '@/tools/framework/tool-config';
import { fileCount, type MoveWording, moveActions } from '@/tools/move/move-actions';
import {
  ensureSettings,
  renderConflictPolicy,
  renderMoveSummary,
  renderPatternSection,
  renderPendingNotices,
} from '@/tools/move/move-controls';
import { buildMoveParams } from '@/tools/move/move-params';

export const FORMAT_REASONS = {
  source: 'Open the folder to format in the left pane.',
  destination: 'The destination folder is not available. Choose another one in the right pane.',
};

export const FORMAT_WORDING: MoveWording = {
  title: (count) => `Move and format ${fileCount(count)}?`,
  confirmLabel: (count) => `Move and format ${fileCount(count)}`,
  intro: ({ size, destination }) =>
    `${size} will be moved into folders built from the pattern ${formatRules.pattern.value} inside ${destination}.`,
};

export const formatTool = defineTool({
  id: 'format',
  title: 'Folder formatting',
  summary:
    "Reorganise source files into folders built from a pattern, using each file's capture date.",
  recursive: 'toggle',
  filter: { applies: 'source' },
  panes: {
    source: { heading: 'Source', baseMode: 'current', showMatches: false, planSide: 'source' },
    comparison: {
      heading: 'Destination',
      baseMode: 'pinned',
      showMatches: false,
      planSide: 'comparison',
      defaultBase: { from: 'source', label: 'Same as the source folder' },
    },
  },
  plan: {
    kind: 'move',
    params: (ctx) =>
      buildMoveParams(
        ctx,
        {
          moveType: 'custom',
          pattern: formatRules.pattern.value,
          patternStatus: formatRules.patternStatus.value,
          policy: formatRules.policy.value,
          matchMode: 'hash',
          strictPrefix: false,
        },
        FORMAT_REASONS,
      ),
  },
  controls: (ctx) => html`${renderConflictPolicy(formatRules)}${renderMoveSummary(ctx)}
    ${renderPendingNotices(ctx, { hash: false })}
    ${renderPatternSection(ctx, formatRules, { collapsible: false, limit: 5 })}`,
  actions: (ctx) =>
    moveActions(ctx, {
      label: 'Move and format files',
      icon: 'folder-tree',
      emptyMessage: 'None of the selected files need to move.',
      wording: FORMAT_WORDING,
    }),
  setup: (ctx) => {
    ensureSettings();
    return followSource(ctx, { enabled: (current) => current.baseIsDefault('comparison') });
  },
});
