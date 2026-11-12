import type { TemplateResult } from 'lit';
import type { PlanKind } from '@/api/generated/PlanKind';
import type { PlanSide } from '@/api/generated/PlanSide';
import type { IconName } from '@/components/opm-icon';
import type { BaseMode } from '@/components/opm-pane';
import { PANES, type PaneId, type ToolId } from '@/stores/nav';
import type { ToolContext } from '@/tools/framework/tool-context';
import type { ScopeLabels } from '@/util/filter';

/**
 * The declarative description of a tool: `<opm-tool-view .config=${config}>` renders it, so a new
 * tool needs no layout code. Build it with `defineTool` and put the page element next to it (see
 * `tools/browse/`). The view never knows a tool by name: everything a tool does is in its config.
 * Everything a callback needs comes from its `ToolContext` argument (`./tool-context.ts`).
 */

/**
 * How one pane behaves.
 *
 * - `heading`: the visible label of the pane and the name of its region (a `section`).
 * - `baseMode`: `'none'` (the pane just browses), `'current'` (the browsed folder IS the base the
 *   other pane is compared with or moved into) or `'pinned'` (the user pins a folder as the base
 *   and keeps browsing elsewhere; the pane shows the chip). See `ToolContext.base`.
 * - `showMatches`: the folder list marks the folders that match the plan (duplicate finder).
 * - `planSide`: which side of the tool's plan the pane shows (`BrowseRequest.plan`), `null` when
 *   the pane never shows the plan. Panes of a tool without a plan have `null`.
 * - `defaultBase`: only with `baseMode: 'pinned'`. While the user pinned nothing, the base of this
 *   pane is the base of pane `from` (Folder formatting formats in place by default). `label` names
 *   that default for the user, for example 'Same as the source folder'.
 */
export interface PaneSpec {
  heading: string;
  baseMode: BaseMode;
  showMatches: boolean;
  planSide: PlanSide | null;
  defaultBase?: { from: PaneId; label: string };
}

/**
 * Which pane(s) the filter bar's filter applies to.
 *
 * - `{ applies: 'both' }`: both panes get the filter (Browse, Folder formatting).
 * - `{ applies: 'source' }`: only the source pane does; the comparison pane gets `EMPTY_FILTER`.
 * - `{ applies: 'choice' }`: the bar shows a scope selector and the user chooses (the duplicate
 *   finder). `labels` renames the three scopes, `initial` is the scope until the user chooses
 *   (default `'both'`). The choice is a preference (`scopeFor` in `@/stores/filters`).
 * - `{ applies: 'none' }`: the filter does not apply to this tool at all (empty folder cleanup):
 *   the bar is shown disabled with `reason` as its explanation, and both panes get `EMPTY_FILTER`.
 */
export type FilterSpec =
  | { applies: 'both' | 'source' }
  | { applies: 'choice'; labels?: ScopeLabels; initial?: 'both' | 'source' | 'comparison' }
  | { applies: 'none'; reason: string };

/** What a tool's `PlanSpec.params` answers: the plan parameters, or why they are not known yet. */
export type PlanParamsResult =
  | { ready: true; params: Record<string, unknown> }
  | { ready: false; reason: string | null };

/**
 * A tool that builds a plan (a server-side preview of what it would do, shown by the panes as an
 * overlay). `kind` is the plan kind the server builds. `params` is the plan client's input: a
 * function of the context that reads signals, so a change of a base, the filter or the tool's own
 * controls rebuilds the plan. `{ ready: false, reason }` means "not yet": the reason (for example
 * 'Choose a base folder in each pane') is shown as quiet text in the tool bar, `null` shows
 * nothing.
 */
export interface PlanSpec {
  kind: PlanKind;
  params: (ctx: ToolContext) => PlanParamsResult;
}

/**
 * One thing the bottom bar can do. `run` does the work and owns its confirmation (use
 * `confirmDialog`); a rejection becomes ONE error toast with the message of the error, except an
 * abort, which is silent. `disabled` returns a reason, which disables the action and becomes its
 * tooltip; `hint` is a static tooltip for an enabled action.
 */
export interface ToolAction {
  id: string;
  label: string;
  icon?: IconName;
  danger?: boolean;
  hint?: string;
  disabled?: (ctx: ToolContext) => string | null;
  run: (ctx: ToolContext) => Promise<void>;
}

/**
 * The bottom bar's far-right area. One option shows a primary button, several a split button
 * that remembers the user's choice per tool (`defaultId` is used until then). `count` is the
 * number shown on the button (what the action would apply to). Ids are unique and `defaultId`
 * names one of them (`checkToolActions`).
 */
export interface ToolActions {
  options: ToolAction[];
  defaultId: string;
  count?: (ctx: ToolContext) => number;
}

/**
 * The whole tool.
 *
 * - `id`: the tool's id in `@/stores/nav`; the key of its filter, scope, state and last action.
 * - `title`: the tool's name in its help dialog (the shell owns the visible heading).
 * - `summary`: one sentence in the help dialog saying what the tool is for.
 * - `help`: steps explaining how to use the tool, shown below its summary.
 * - `panes`: how each of the two panes behaves.
 * - `filter`: what the filter bar's filter applies to.
 * - `recursive`: `'always'` (default) looks at subfolders too; `'toggle'` adds the "Include
 *   subfolders" switch (on by default, kept per tool in memory) to the tool bar.
 * - `controls`: the tool's own controls, built from shared components, shown in the tool bar. It
 *   runs in the view's render, so what it reads from signals re-renders it.
 * - `plan`: only for a tool that builds a plan.
 * - `actions`: the bottom bar's actions, `null` for none. Runs in the view's render too.
 * - `setup`: runs when the view connects and returns its cleanup, for wiring only this tool needs
 *   (`followSource` is one). It runs once per connection, not per render, and runs again for a
 *   new `config`.
 */
export interface ToolConfig {
  id: ToolId;
  title: string;
  summary: string;
  help?: readonly string[];
  panes: Record<PaneId, PaneSpec>;
  filter: FilterSpec;
  recursive?: 'toggle' | 'always';
  controls?: (ctx: ToolContext) => TemplateResult;
  controlsDialog?: { label: string; render: (ctx: ToolContext) => TemplateResult };
  floatingActions?: boolean;
  plan?: PlanSpec;
  actions?: (ctx: ToolContext) => ToolActions | null;
  setup?: (ctx: ToolContext) => (() => void) | undefined;
}

/** What is wrong with the actions, `null` when they are fine: ids are unique, `defaultId` exists. */
export function checkToolActions(actions: ToolActions): string | null {
  const seen = new Set<string>();
  for (const { id } of actions.options) {
    if (seen.has(id)) return `action id "${id}" is used twice`;
    seen.add(id);
  }
  if (actions.options.length > 0 && !seen.has(actions.defaultId)) {
    return `defaultId "${actions.defaultId}" is not the id of an action`;
  }
  return null;
}

/** What is wrong with the config, `null` when it is fine. `defineTool` throws this message. */
export function checkToolConfig(config: ToolConfig): string | null {
  for (const pane of PANES) {
    if (!config.panes[pane]) return `pane "${pane}" is missing`;
  }
  const planSides = PANES.filter((pane) => config.panes[pane].planSide !== null);
  if (config.plan && planSides.length === 0) {
    return 'a tool with a plan needs `planSide` on at least one pane';
  }
  if (!config.plan && planSides.length > 0) {
    return `pane "${planSides[0]}" has a \`planSide\` but the tool has no plan`;
  }
  for (const pane of PANES) {
    const spec = config.panes[pane];
    const base = spec.defaultBase;
    if (!base) continue;
    if (spec.baseMode !== 'pinned') {
      return `pane "${pane}" has a \`defaultBase\` but its baseMode is "${spec.baseMode}"`;
    }
    if (base.from === pane || !PANES.includes(base.from)) {
      return `pane "${pane}": \`defaultBase.from\` must be the other pane`;
    }
    if (config.panes[base.from].defaultBase) {
      return `pane "${pane}": its \`defaultBase.from\` pane "${base.from}" has a \`defaultBase\` too`;
    }
  }
  if (config.filter.applies === 'none' && !config.filter.reason) {
    return 'a filter that applies to none needs a `reason`';
  }
  return null;
}

/**
 * Identity: gives `config` its type and checks it, so a mistake fails the tool's own test at once
 * (it throws when the module is imported). The actions are a function of the context, so
 * `checkToolActions` runs on them when they are rendered (a tool's test may call it too).
 */
export function defineTool(config: ToolConfig): ToolConfig {
  const problem = checkToolConfig(config);
  if (problem !== null) throw new Error(`Tool "${config.id}": ${problem}`);
  return config;
}
