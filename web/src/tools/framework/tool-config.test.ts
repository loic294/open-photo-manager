import { describe, expect, it } from 'vitest';
import { toolConfig } from '@/test/tool-config';
import {
  checkToolActions,
  checkToolConfig,
  defineTool,
  type ToolAction,
  type ToolActions,
} from '@/tools/framework/tool-config';

const run = async () => {};

function action(id: string): ToolAction {
  return { id, label: id, run };
}

function actions(ids: string[], defaultId = ids[0] ?? ''): ToolActions {
  return { options: ids.map(action), defaultId };
}

describe('checkToolActions', () => {
  it('accepts unique ids and a default that is one of them', () => {
    expect(checkToolActions(actions(['move', 'copy'], 'copy'))).toBeNull();
  });

  it('accepts a single action', () => {
    expect(checkToolActions(actions(['only']))).toBeNull();
  });

  it('accepts no options at all: there is nothing to default to', () => {
    expect(checkToolActions({ options: [], defaultId: '' })).toBeNull();
  });

  it('names an id that is used twice', () => {
    expect(checkToolActions(actions(['a', 'b', 'a']))).toBe('action id "a" is used twice');
  });

  it('names a default that is not an id', () => {
    expect(checkToolActions(actions(['a', 'b'], 'c'))).toBe(
      'defaultId "c" is not the id of an action',
    );
  });
});

describe('checkToolConfig', () => {
  const pinnedWithDefault = {
    heading: 'Destination',
    baseMode: 'pinned',
    showMatches: false,
    planSide: 'comparison',
    defaultBase: { from: 'source', label: 'Same as the source folder' },
  } as const;

  it('accepts a plain tool', () => {
    expect(checkToolConfig(toolConfig())).toBeNull();
  });

  it('accepts a tool whose plan is shown by a pane', () => {
    const config = toolConfig({
      id: 'duplicates',
      plan: { kind: 'duplicates', params: () => ({ ready: false, reason: null }) },
      panes: {
        source: { heading: 'S', baseMode: 'none', showMatches: true, planSide: 'source' },
        comparison: { heading: 'C', baseMode: 'none', showMatches: true, planSide: 'comparison' },
      },
    });
    expect(checkToolConfig(config)).toBeNull();
  });

  it('refuses a plan that no pane shows', () => {
    const config = toolConfig({
      id: 'duplicates',
      plan: { kind: 'duplicates', params: () => ({ ready: false, reason: null }) },
    });
    expect(checkToolConfig(config)).toBe(
      'a tool with a plan needs `planSide` on at least one pane',
    );
  });

  it('refuses a pane that shows the plan of a tool without one', () => {
    const config = toolConfig({
      panes: {
        source: { heading: 'S', baseMode: 'none', showMatches: false, planSide: 'source' },
        comparison: { heading: 'C', baseMode: 'none', showMatches: false, planSide: null },
      },
    });
    expect(checkToolConfig(config)).toBe('pane "source" has a `planSide` but the tool has no plan');
  });

  it('refuses a missing pane', () => {
    const config = toolConfig();
    const panes = { source: config.panes.source } as unknown as ToolConfigPanes;
    expect(checkToolConfig({ ...config, panes })).toBe('pane "comparison" is missing');
  });

  it('accepts a default base that points at the other pane', () => {
    const config = toolConfig({
      id: 'format',
      panes: {
        source: { heading: 'S', baseMode: 'current', showMatches: false, planSide: null },
        comparison: { ...pinnedWithDefault, planSide: null },
      },
    });
    expect(checkToolConfig(config)).toBeNull();
  });

  it('refuses a default base on a pane that is not pinned', () => {
    const config = toolConfig({
      panes: {
        source: { heading: 'S', baseMode: 'none', showMatches: false, planSide: null },
        comparison: {
          heading: 'C',
          baseMode: 'current',
          showMatches: false,
          planSide: null,
          defaultBase: { from: 'source', label: 'x' },
        },
      },
    });
    expect(checkToolConfig(config)).toBe(
      'pane "comparison" has a `defaultBase` but its baseMode is "current"',
    );
  });

  it('refuses a default base that points at the pane itself', () => {
    const config = toolConfig({
      panes: {
        source: { heading: 'S', baseMode: 'none', showMatches: false, planSide: null },
        comparison: {
          ...pinnedWithDefault,
          planSide: null,
          defaultBase: { from: 'comparison', label: 'x' },
        },
      },
    });
    expect(checkToolConfig(config)).toBe(
      'pane "comparison": `defaultBase.from` must be the other pane',
    );
  });

  it('refuses two panes that default to each other', () => {
    const config = toolConfig({
      panes: {
        source: {
          ...pinnedWithDefault,
          planSide: null,
          defaultBase: { from: 'comparison', label: 'x' },
        },
        comparison: { ...pinnedWithDefault, planSide: null },
      },
    });
    expect(checkToolConfig(config)).toBe(
      'pane "source": its `defaultBase.from` pane "comparison" has a `defaultBase` too',
    );
  });

  it('refuses a filter that applies to nothing without saying why', () => {
    expect(checkToolConfig(toolConfig({ filter: { applies: 'none', reason: '' } }))).toBe(
      'a filter that applies to none needs a `reason`',
    );
    expect(
      checkToolConfig(toolConfig({ filter: { applies: 'none', reason: 'Empty is absolute.' } })),
    ).toBeNull();
  });
});

type ToolConfigPanes = ReturnType<typeof toolConfig>['panes'];

describe('defineTool', () => {
  it('returns the very config it was given', () => {
    const config = toolConfig();
    expect(defineTool(config)).toBe(config);
  });

  it('throws at once, naming the tool and the problem', () => {
    const config = toolConfig({ id: 'cleanup', filter: { applies: 'none', reason: '' } });
    expect(() => defineTool(config)).toThrow(
      'Tool "cleanup": a filter that applies to none needs a `reason`',
    );
  });
});
