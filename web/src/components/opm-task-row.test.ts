import { describe, expect, it, vi } from 'vitest';
import type { TaskDto } from '@/api/generated/TaskDto';
import type { TaskKind } from '@/api/generated/TaskKind';
import '@/components/opm-task-row';
import type { OpmTaskRow, TaskCancelEvent, TaskDismissEvent } from '@/components/opm-task-row';
import { mount } from '@/test/helpers';
import { makeTask } from '@/test/task-fixtures';
import { formatCount } from '@/util/format-count';

const MB = 1024 ** 2;
const GB = 1024 ** 3;

type RowProps = Partial<Pick<OpmTaskRow, 'cancelling' | 'compact'>>;

function row(task: Partial<TaskDto> = {}, props: RowProps = {}) {
  return mount('opm-task-row', { task: makeTask(task), ...props });
}

/** The segments of the numbers line. */
function numbers(element: HTMLElement): (string | null)[] {
  return [...element.querySelectorAll('.tabular-nums > span')].map((span) => span.textContent);
}

const bar = (element: HTMLElement) => element.querySelector('progress');
const dot = (element: HTMLElement) => element.querySelector('.status');
const cancelButton = (element: HTMLElement) =>
  element.querySelector<HTMLButtonElement>('button[aria-label^="Cancel"]');
const dismissButton = (element: HTMLElement) =>
  element.querySelector<HTMLButtonElement>('button[aria-label^="Dismiss"]');
const alertLine = (element: HTMLElement) => element.querySelector('.alert');

describe('opm-task-row', () => {
  it('renders nothing without a task', async () => {
    const element = await mount('opm-task-row');

    expect(element.children).toHaveLength(0);
  });

  describe('a queued task', () => {
    it('is waiting: no bar, a neutral dot and a cancel button', async () => {
      const element = await row({ state: 'queued', started_at: null });

      expect(numbers(element)).toEqual(['Waiting']);
      expect(bar(element)).toBeNull();
      expect(dot(element)?.classList.contains('status-neutral')).toBe(true);
      expect(cancelButton(element)?.getAttribute('aria-label')).toBe('Cancel Move 3 files');
      expect(dismissButton(element)).toBeNull();
    });

    it('says it is cancelling while the request is out', async () => {
      const element = await row({ state: 'queued', started_at: null }, { cancelling: true });

      expect(numbers(element)).toEqual(['Cancelling…']);
      expect(cancelButton(element)?.disabled).toBe(true);
    });
  });

  describe('a running task', () => {
    it('with bytes shows the counts, the speed and the time left on a determinate bar', async () => {
      const element = await row({
        items_done: 120,
        items_total: 300,
        bytes_done: GB,
        bytes_total: 4 * GB,
        speed: 42 * MB,
        eta_secs: 120,
      });

      expect(numbers(element)).toEqual([
        '120 / 300 items',
        '1 GB / 4 GB',
        '42 MB/s',
        'about 2 min left',
      ]);
      // The bar follows the bytes (25%), not the items (40%).
      expect(bar(element)?.getAttribute('value')).toBe('25');
      expect(bar(element)?.getAttribute('max')).toBe('100');
      expect(bar(element)?.classList.contains('progress-primary')).toBe(true);
      expect(dot(element)?.classList.contains('status-primary')).toBe(true);
    });

    it('with items shows items per second on a bar that follows the items', async () => {
      const element = await row({ items_done: 40, items_total: 160, speed: 85, eta_secs: 200 });

      expect(numbers(element)).toEqual(['40 / 160 items', '85 items/s', 'about 3 min left']);
      expect(bar(element)?.getAttribute('value')).toBe('25');
    });

    it('groups the digits of a large count like the locale does', async () => {
      const element = await row({ items_done: 1_204, items_total: 100_000 });

      expect(numbers(element)).toEqual([`${formatCount(1_204)} / ${formatCount(100_000)} items`]);
    });

    it('has an indeterminate bar while it has no total', async () => {
      const element = await row({ items_done: 7 });

      expect(numbers(element)).toEqual(['7 items']);
      expect(bar(element)).not.toBeNull();
      expect(bar(element)?.hasAttribute('value')).toBe(false);
    });

    it('shows the bytes done when it has no byte total', async () => {
      const element = await row({ bytes_done: 5 * MB, speed: 2 });

      expect(numbers(element)).toEqual(['5 MB', '2 items/s']);
    });

    it('says it is starting while there is nothing to count', async () => {
      const element = await row();

      expect(numbers(element)).toEqual(['Starting…']);
      expect(bar(element)?.hasAttribute('value')).toBe(false);
    });

    it('writes one item in the singular', async () => {
      const element = await row({ items_done: 1 });

      expect(numbers(element)).toEqual(['1 item']);
    });

    it('leaves out a speed of zero and a time left it does not have', async () => {
      const element = await row({ items_done: 3, items_total: 10, speed: 0, eta_secs: null });

      expect(numbers(element)).toEqual(['3 / 10 items']);
    });

    it('starts from zero on a determinate bar when the total is known', async () => {
      const element = await row({ items_done: 0, items_total: 10 });

      expect(numbers(element)).toEqual(['0 / 10 items']);
      expect(bar(element)?.getAttribute('value')).toBe('0');
    });

    it('keeps its counts but drops the speed and the time left while it is cancelling', async () => {
      const element = await row(
        { items_done: 40, items_total: 160, speed: 85, eta_secs: 200 },
        { cancelling: true },
      );

      expect(numbers(element)).toEqual(['Cancelling…', '40 / 160 items']);
      expect(cancelButton(element)?.disabled).toBe(true);
    });

    it('has an enabled cancel button and no dismiss button', async () => {
      const element = await row();

      expect(cancelButton(element)?.disabled).toBe(false);
      expect(dismissButton(element)).toBeNull();
    });
  });

  describe('a task that is over', () => {
    it('shows a done task with a full bar, its final counts and a dismiss button', async () => {
      const element = await row({
        state: 'done',
        items_done: 300,
        items_total: 300,
        bytes_done: 4 * GB,
        bytes_total: 4 * GB,
        finished_at: 2_000,
      });

      expect(numbers(element)).toEqual(['Done', '300 items', '4 GB']);
      expect(bar(element)?.getAttribute('value')).toBe('100');
      expect(bar(element)?.classList.contains('progress-success')).toBe(true);
      expect(dot(element)?.classList.contains('status-success')).toBe(true);
      expect(dismissButton(element)?.getAttribute('aria-label')).toBe('Dismiss Move 3 files');
      expect(cancelButton(element)).toBeNull();
    });

    it('shows a done task with nothing to measure on a full bar', async () => {
      const element = await row({ state: 'done', finished_at: 2_000 });

      expect(numbers(element)).toEqual(['Done']);
      expect(bar(element)?.getAttribute('value')).toBe('100');
    });

    it('shows a failed task with its error, a red bar where it stopped and a dismiss button', async () => {
      const element = await row({
        state: 'failed',
        error: 'move failed: disk full',
        items_done: 40,
        items_total: 160,
        finished_at: 2_000,
      });

      expect(numbers(element)).toEqual(['Failed', '40 / 160 items']);
      expect(bar(element)?.getAttribute('value')).toBe('25');
      expect(bar(element)?.classList.contains('progress-error')).toBe(true);
      expect(dot(element)?.classList.contains('status-error')).toBe(true);
      expect(alertLine(element)?.textContent?.trim()).toBe('move failed: disk full');
      expect(alertLine(element)?.classList.contains('alert-error')).toBe(true);
      expect(dismissButton(element)).not.toBeNull();
      expect(cancelButton(element)).toBeNull();
    });

    it('does not use role="alert": the live region of the indicator announces failures', async () => {
      const element = await row({ state: 'failed', error: 'disk full', finished_at: 2_000 });

      expect(element.querySelector('[role="alert"]')).toBeNull();
    });

    it('shows an interrupted task like a failure', async () => {
      const element = await row({
        state: 'interrupted',
        error: 'interrupted by restart',
        items_done: 5,
        finished_at: 2_000,
      });

      expect(numbers(element)).toEqual(['Interrupted', '5 items']);
      expect(dot(element)?.classList.contains('status-error')).toBe(true);
      expect(alertLine(element)?.textContent?.trim()).toBe('interrupted by restart');
    });

    it('has no bar for a failed task that never declared a total', async () => {
      const element = await row({ state: 'failed', error: 'boom', items_done: 4, finished_at: 2 });

      expect(bar(element)).toBeNull();
    });

    it('shows no error line for a failure without a message', async () => {
      const element = await row({ state: 'failed', error: null, finished_at: 2_000 });

      expect(alertLine(element)).toBeNull();
    });

    it('shows a cancelled task with a warning bar where it stopped', async () => {
      const element = await row({
        state: 'cancelled',
        items_done: 80,
        items_total: 160,
        finished_at: 2_000,
      });

      expect(numbers(element)).toEqual(['Cancelled', '80 / 160 items']);
      expect(bar(element)?.getAttribute('value')).toBe('50');
      expect(bar(element)?.classList.contains('progress-warning')).toBe(true);
      expect(dot(element)?.classList.contains('status-warning')).toBe(true);
      expect(alertLine(element)).toBeNull();
      expect(dismissButton(element)).not.toBeNull();
    });
  });

  describe('compact', () => {
    it('drops the kind icon, the speed and the time left', async () => {
      const element = await row(
        { items_done: 40, items_total: 160, speed: 85, eta_secs: 200 },
        { compact: true },
      );

      expect(numbers(element)).toEqual(['40 / 160 items']);
      // The only icon left is the one in the cancel button.
      expect(element.querySelectorAll('opm-icon')).toHaveLength(1);
      expect(cancelButton(element)?.querySelector('opm-icon')).not.toBeNull();
    });

    it('still shows the kind icon when it is not compact', async () => {
      const element = await row();

      expect(element.querySelectorAll('opm-icon')).toHaveLength(2);
    });
  });

  describe('kind', () => {
    it.each([
      ['index', 'database'],
      ['fs_move', 'folder-input'],
      ['fs_trash', 'trash-2'],
      ['fs_rmdir', 'folder-x'],
      ['plan_execute', 'list-checks'],
      ['immich_sync', 'cloud-download'],
      ['immich_cleanup', 'cloud-upload'],
      ['trash_empty', 'brush-cleaning'],
    ] as [TaskKind, string][])('shows the %s task with the %s icon', async (kind, icon) => {
      const element = await row({ kind });

      expect(element.querySelector('opm-icon')?.getAttribute('name')).toBe(icon);
    });
  });

  describe('events', () => {
    it('asks to cancel a task, with its id', async () => {
      const element = await row({ id: 42 });
      const heard = vi.fn();
      element.addEventListener('task-cancel', heard);

      cancelButton(element)?.click();

      expect(heard).toHaveBeenCalledOnce();
      const event = heard.mock.calls[0]?.[0] as TaskCancelEvent;
      expect(event.detail).toEqual({ id: 42 });
      expect(event.bubbles).toBe(true);
    });

    it('asks to dismiss a task that is over, with its id', async () => {
      const element = await row({ id: 42, state: 'failed', error: 'boom', finished_at: 2 });
      const heard = vi.fn();
      element.addEventListener('task-dismiss', heard);

      dismissButton(element)?.click();

      expect(heard).toHaveBeenCalledOnce();
      const event = heard.mock.calls[0]?.[0] as TaskDismissEvent;
      expect(event.detail).toEqual({ id: 42 });
      expect(event.bubbles).toBe(true);
    });

    it('bubbles to the element that holds the row', async () => {
      const element = await row({ id: 7 });
      const heard = vi.fn();
      document.body.addEventListener('task-cancel', heard);

      cancelButton(element)?.click();
      document.body.removeEventListener('task-cancel', heard);

      expect(heard).toHaveBeenCalledOnce();
    });
  });

  describe('text', () => {
    it('renders a title as text, never as markup', async () => {
      const title = '<img src=x onerror="window.pwned = true"><b>bold</b>';
      const element = await row({ title, state: 'failed', error: '<i>boom</i>', finished_at: 2 });

      expect(element.querySelector('img')).toBeNull();
      expect(element.querySelector('b')).toBeNull();
      expect(element.querySelector('i')).toBeNull();
      expect(element.querySelector('.truncate')?.textContent).toBe(title);
      expect(alertLine(element)?.textContent?.trim()).toBe('<i>boom</i>');
    });

    it('keeps the whole title as text and in the tooltip though it is cut to one line', async () => {
      const title = `Move ${'a very long folder name '.repeat(8)}`.trim();
      const element = await row({ title });
      const text = element.querySelector('.truncate');

      expect(text?.textContent).toBe(title);
      expect(text?.getAttribute('title')).toBe(title);
      expect(bar(element)?.getAttribute('aria-label')).toBe(title);
    });

    it('shows the latest title when the task changes', async () => {
      const element = await row({ title: 'Move 3 files' });

      element.task = makeTask({ title: 'Move 2 files' });
      await element.updateComplete;

      expect(element.querySelector('.truncate')?.textContent).toBe('Move 2 files');
      expect(cancelButton(element)?.getAttribute('aria-label')).toBe('Cancel Move 2 files');
    });

    it('follows the task from running to done', async () => {
      const element = await row({ items_done: 3, items_total: 3 });
      expect(cancelButton(element)).not.toBeNull();

      element.task = makeTask({ state: 'done', items_done: 3, items_total: 3, finished_at: 2 });
      await element.updateComplete;

      expect(numbers(element)).toEqual(['Done', '3 items']);
      expect(cancelButton(element)).toBeNull();
      expect(dismissButton(element)).not.toBeNull();
    });
  });
});
