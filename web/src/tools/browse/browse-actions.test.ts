import { afterEach, describe, expect, it, vi } from 'vitest';
import type { SelectionDto } from '@/api/generated/SelectionDto';
import type { TaskDto } from '@/api/generated/TaskDto';
import { resetSelectionForTests, selection } from '@/stores/selection';
import { makeTask } from '@/test/task-fixtures';
import { browseActions } from '@/tools/browse/browse-actions';
import { browseTool } from '@/tools/browse/tool';
import { createToolContext, type ToolContext } from '@/tools/framework/tool-context';

const mocks = vi.hoisted(() => ({
  createSelection: vi.fn(),
  trashFiles: vi.fn(),
  confirm: vi.fn(),
}));

vi.mock('@/api/selections', () => ({ createSelection: mocks.createSelection }));
vi.mock('@/api/trash', () => ({ trashFiles: mocks.trashFiles }));
vi.mock('@/components/confirm-dialog', () => ({ confirmDialog: mocks.confirm }));

afterEach(() => {
  resetSelectionForTests();
  vi.clearAllMocks();
});

function context(): ToolContext {
  return createToolContext(browseTool, document.createElement('div'));
}

const stored: SelectionDto = {
  id: 7,
  item_count: 3,
  total_size: 12_345,
  label: 'Browse',
  created_at: 1,
  expires_at: 2,
};

describe('browse actions', () => {
  it('is disabled until one of the panes selects a file', () => {
    const action = browseActions(context()).options[0];

    expect(action?.disabled?.(context())).toBe('Select files to delete');
  });

  it('deduplicates both panes, confirms the bytes, then queues one trash task', async () => {
    mocks.createSelection.mockResolvedValue(stored);
    mocks.confirm.mockResolvedValue(true);
    mocks.trashFiles.mockResolvedValue(makeTask({ kind: 'fs_trash' }) as TaskDto);
    const ctx = context();
    selection.source.set([1, 2]);
    selection.comparison.set([2, 3]);

    await browseActions(ctx).options[0]?.run(ctx);

    expect(mocks.createSelection).toHaveBeenCalledWith([1, 2, 3], 'Browse');
    expect(mocks.confirm).toHaveBeenCalledWith(
      expect.objectContaining({
        title: 'Delete 3 files?',
        confirmLabel: 'Delete 3 files',
        danger: true,
        message: expect.stringContaining('12 KB'),
      }),
    );
    expect(mocks.trashFiles).toHaveBeenCalledWith({ selection_id: 7 });
    expect(selection.source.count.value).toBe(0);
    expect(selection.comparison.count.value).toBe(0);
  });

  it('keeps the selection and sends no delete request when confirmation is cancelled', async () => {
    mocks.createSelection.mockResolvedValue(stored);
    mocks.confirm.mockResolvedValue(false);
    const ctx = context();
    selection.source.set([1]);

    await browseActions(ctx).options[0]?.run(ctx);

    expect(mocks.trashFiles).not.toHaveBeenCalled();
    expect(selection.source.toArray()).toEqual([1]);
  });
});
