import { render } from 'lit';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApmPatternEditor } from '@/components/apm-pattern-editor';
import type { ApmSelect } from '@/components/apm-select';
import { FakePlan, toolConfig } from '@/test/tool-config';
import { createToolContext } from '@/tools/framework/tool-context';
import {
  destinationInsideSource,
  moveControls,
  renderMoveSummary,
  renderPatternSection,
  renderPendingNotices,
} from './move-controls';
import { createRuleState, moveRules, moveType } from './move-state';
import { moveTool } from './tool';

vi.mock('@/api/format', () => ({
  fetchFormatTokens: async () => ({ tokens: [], examples: [], sample: {} }),
  previewFormat: vi.fn(),
}));
afterEach(() => {
  document.body.replaceChildren();
  moveType.value = 'keep_source';
  moveRules.pattern.value = '{YYYY}/{mm}';
  moveRules.patternStatus.value = 'idle';
  vi.restoreAllMocks();
});
function setup() {
  const container = document.createElement('div');
  document.body.append(container);
  const ctx = createToolContext(toolConfig(), container);
  const plan = new FakePlan();
  Object.defineProperty(ctx, 'plan', { value: plan });
  return { ctx, plan, container };
}

it('uses shared selectors, conditional match controls, and the same pattern component for both tools', async () => {
  const { ctx, container } = setup();
  render(moveControls(ctx), container);
  const type = container.querySelector<ApmSelect>('apm-select');
  expect(type?.options.map((option) => option.label)).toEqual([
    'Keep source file structure',
    'Keep destination file structure',
    'Flat file transfer',
    'Use custom file structure',
  ]);
  type?.dispatchEvent(new CustomEvent('select-change', { detail: { value: 'keep_destination' } }));
  render(moveControls(ctx), container);
  expect(container.querySelector('apm-match-mode')).not.toBeNull();
  moveType.value = 'custom';
  render(moveControls(ctx), container);
  expect(container.querySelector('apm-match-mode')).toBeNull();
  let editor = container.querySelector<ApmPatternEditor>('apm-pattern-editor');
  expect(editor?.limit).toBe(3);
  editor?.dispatchEvent(
    new CustomEvent('pattern-change', { detail: { value: '{camera}', status: 'invalid' } }),
  );
  expect(moveRules.pattern.value).toBe('{camera}');
  expect(moveRules.patternStatus.value).toBe('invalid');
  render(moveControls(ctx), container);
  expect(container.querySelector('details')?.open).toBe(true);
  const other = createRuleState('controls-test');
  render(renderPatternSection(ctx, other, { collapsible: false, limit: 5 }), container);
  editor = container.querySelector('apm-pattern-editor');
  expect(editor?.limit).toBe(5);
  expect(editor?.parentElement?.tagName).toBe('DIV');
});

it('summarizes only server stats and renders pending metadata/hash notices once', () => {
  const { ctx, plan, container } = setup();
  render(renderMoveSummary(ctx), container);
  expect(container.textContent).toBe('');
  plan.statusSignal.value = 'ready';
  plan.statsSignal.value = {
    candidates: 2,
    to_move: 2,
    bytes_to_move: 1024,
    new_folders: 1,
    renamed: 1,
  };
  render(renderMoveSummary(ctx), container);
  expect(container.textContent).toContain('2 files (1 KB) will move, 1 new folders');
  plan.statsSignal.value = { candidates: 2, already_in_place: 2, to_move: 0 };
  render(renderMoveSummary(ctx), container);
  expect(container.textContent).toContain('All 2 files are already in place');
  plan.warningsSignal.value = [
    { code: 'metadata_pending', message: 'pending', count: 4 },
    { code: 'unhashed_files', message: 'pending', count: 3 },
  ];
  render(renderPendingNotices(ctx, { hash: false }), container);
  expect(container.textContent).toContain('4 files not read yet');
  expect(container.textContent).not.toContain('not hashed');
  render(renderPendingNotices(ctx, { hash: true }), container);
  expect(container.textContent).toContain('3 files not hashed yet');
});

it.each([
  ['', 'below', 'photos', true],
  ['a/b', 'a/b/c', 'photos', true],
  ['a/b', 'a/bc', 'photos', false],
  ['a/b', 'a/b', 'photos', false],
  ['a/b', 'a/b/c', 'backup', false],
])('warns only for strictly nested destinations', (source, destination, volume, expected) => {
  const ctx = createToolContext({ ...moveTool, plan: undefined }, document.createElement('div'));
  ctx.setBase('source', { volume: 'photos', dir: source });
  ctx.setBase('comparison', { volume, dir: destination });
  expect(destinationInsideSource(ctx)).toBe(expected);
});
