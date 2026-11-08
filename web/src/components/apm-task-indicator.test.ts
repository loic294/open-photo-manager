import { afterEach, beforeEach, describe, expect, it, type MockInstance, vi } from 'vitest';
import { api } from '@/api/client';
import type { TaskDto } from '@/api/generated/TaskDto';
import { events } from '@/api/sse';
import '@/components/apm-task-indicator';
import type { ApmTaskIndicator } from '@/components/apm-task-indicator';
import type { ApmTaskRow } from '@/components/apm-task-row';
import { STORAGE_PREFIX } from '@/stores/persisted';
import { cancel, FINISHED_LINGER_MS, resetTasksForTests, startTasks, tasks } from '@/stores/tasks';
import { mount } from '@/test/helpers';
import { makeTask, taskEvent } from '@/test/task-fixtures';

let post: MockInstance<typeof api.post>;
let stop: (() => void) | undefined;

function need<T extends Element>(root: ParentNode, selector: string): T {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`Nothing matches ${selector}`);
  return found;
}

const trigger = (element: HTMLElement) => need<HTMLButtonElement>(element, 'button[aria-expanded]');
const dropdown = (element: HTMLElement) => need<HTMLElement>(element, '.dropdown');
const isOpen = (element: HTMLElement) => dropdown(element).classList.contains('dropdown-open');
const bar = (element: HTMLElement) => need<HTMLProgressElement>(trigger(element), 'progress');
const rows = (element: HTMLElement) => [
  ...element.querySelectorAll<ApmTaskRow>('.dropdown-content apm-task-row'),
];
const liveRegion = (element: HTMLElement) => need<HTMLElement>(element, '[aria-live="polite"]');
const announced = (element: HTMLElement) =>
  [...liveRegion(element).querySelectorAll('p')].map((line) => line.textContent);
const badge = (element: HTMLElement, tone: 'error' | 'ghost') =>
  trigger(element).querySelector(`.badge-${tone}`);

/** The visible text of the trigger, without the badges. */
const visibleLabel = (element: HTMLElement) =>
  need<HTMLElement>(trigger(element), 'span.truncate').textContent;

const percentText = (element: HTMLElement) =>
  need<HTMLElement>(trigger(element), 'span.tabular-nums').textContent?.trim();

const emit = (overrides: Partial<TaskDto>) => events.emit(taskEvent(overrides));

/** Mounts the indicator and feeds it the tasks the way the event stream does. */
async function show(...list: Partial<TaskDto>[]): Promise<ApmTaskIndicator> {
  const element = await mount('apm-task-indicator');
  for (const task of list) emit(task);
  await element.updateComplete;
  return element;
}

/** Lets the rows of the panel render (they are components of their own). */
async function rowsReady(element: HTMLElement): Promise<void> {
  await Promise.all(rows(element).map((row) => row.updateComplete));
}

async function update(element: HTMLElement) {
  await (element as ApmTaskIndicator).updateComplete;
}

function pointer(target: EventTarget, type: string, pointerType = 'mouse'): void {
  target.dispatchEvent(new PointerEvent(type, { pointerType, bubbles: type === 'pointerdown' }));
}

function focusChange(
  target: EventTarget,
  type: 'focusin' | 'focusout',
  relatedTarget: EventTarget | null = null,
): void {
  target.dispatchEvent(new FocusEvent(type, { bubbles: true, relatedTarget }));
}

function press(target: EventTarget, key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

async function hover(element: ApmTaskIndicator) {
  pointer(dropdown(element), 'pointerenter');
  await element.updateComplete;
}

async function unhover(element: ApmTaskIndicator) {
  pointer(dropdown(element), 'pointerleave');
  await element.updateComplete;
}

async function click(element: ApmTaskIndicator) {
  trigger(element).click();
  await element.updateComplete;
}

beforeEach(() => {
  resetTasksForTests();
  localStorage.clear();
  vi.spyOn(api, 'get').mockResolvedValue([]);
  post = vi.spyOn(api, 'post');
  stop = startTasks();
});

afterEach(() => {
  stop?.();
  stop = undefined;
  vi.useRealTimers();
  vi.restoreAllMocks();
  resetTasksForTests();
});

describe('apm-task-indicator when nothing is going on', () => {
  it('renders nothing and has no box of its own', async () => {
    const element = await mount('apm-task-indicator');

    expect(element.children).toHaveLength(0);
    expect(element.textContent?.trim()).toBe('');
    expect(element.classList.contains('contents')).toBe(true);
  });

  it('appears with the first task and disappears with the last', async () => {
    const element = await show();
    expect(element.querySelector('button')).toBeNull();

    emit({ id: 1 });
    await update(element);
    expect(trigger(element)).not.toBeNull();

    emit({ id: 1, state: 'failed', error: 'boom', finished_at: 5 });
    await update(element);
    expect(trigger(element)).not.toBeNull();

    need<HTMLButtonElement>(element, 'button[aria-label^="Dismiss"]').click();
    await update(element);
    expect(element.children).toHaveLength(0);
  });

  it('goes away when a task that ended well has been listed for its few seconds', async () => {
    const element = await mount('apm-task-indicator');
    vi.useFakeTimers();

    emit({ id: 1, state: 'done', finished_at: 5 });
    await update(element);
    expect(trigger(element)).not.toBeNull();

    vi.advanceTimersByTime(FINISHED_LINGER_MS);
    await update(element);

    expect(element.children).toHaveLength(0);
  });
});

describe('apm-task-indicator trigger', () => {
  it('is a real button that says what is going on in words', async () => {
    const element = await show({ items_done: 25, items_total: 100 });

    const button = trigger(element);
    expect(button.tagName).toBe('BUTTON');
    expect(button.getAttribute('type')).toBe('button');
    expect(button.getAttribute('aria-label')).toBe('Move 3 files running, 25 percent');
    expect(button.getAttribute('aria-expanded')).toBe('false');
    expect(button.getAttribute('aria-controls')).toBe(need(element, '.dropdown-content').id);
    // The panel is not a menu.
    expect(button.hasAttribute('aria-haspopup')).toBe(false);
  });

  it('shows the title of the only running task, the share and a determinate bar', async () => {
    const element = await show({ bytes_done: 1, bytes_total: 4, items_done: 9, items_total: 10 });

    expect(visibleLabel(element)).toBe('Move 3 files');
    expect(percentText(element)).toBe('25%');
    expect(bar(element).getAttribute('value')).toBe('25');
    expect(bar(element).getAttribute('max')).toBe('100');
    expect(bar(element).classList.contains('progress-primary')).toBe(true);
  });

  it('keeps its size when the share changes: fixed bar, fixed share, tabular digits', async () => {
    const element = await show({ items_done: 5, items_total: 100 });

    expect(bar(element).classList.contains('w-16')).toBe(true);
    expect(bar(element).classList.contains('md:w-28')).toBe(true);
    const share = need<HTMLElement>(trigger(element), 'span.tabular-nums');
    expect(share.classList.contains('w-9')).toBe(true);
    expect(share.textContent?.trim()).toBe('5%');

    emit({ items_done: 100, items_total: 100 });
    await update(element);

    expect(percentText(element)).toBe('100%');
    expect(bar(element).getAttribute('value')).toBe('100');
    expect(bar(element).classList.contains('w-16')).toBe(true);
    expect(bar(element).classList.contains('md:w-28')).toBe(true);
  });

  it('switches theme foreground and background together without a low-contrast colour transition', async () => {
    const element = await show(
      { id: 1, items_done: 20, items_total: 100 },
      { id: 2, state: 'queued', started_at: null },
    );

    expect(
      trigger(element).classList.contains('transition-[border-color,box-shadow,transform]'),
    ).toBe(true);
    expect(percentText(element)).toBe('20%');
    expect(badge(element, 'ghost')?.textContent).toBe('1');
  });

  it('follows the progress live', async () => {
    const element = await show({ items_done: 10, items_total: 100 });

    emit({ items_done: 40, items_total: 100 });
    await update(element);

    expect(bar(element).getAttribute('value')).toBe('40');
    expect(trigger(element).getAttribute('aria-label')).toBe('Move 3 files running, 40 percent');
  });

  it('counts the running tasks and averages their progress', async () => {
    const element = await show(
      { id: 1, items_done: 50, items_total: 100 },
      { id: 2, items_done: 74, items_total: 100, started_at: 2_000 },
    );

    expect(visibleLabel(element)).toBe('2 tasks');
    expect(percentText(element)).toBe('62%');
    expect(trigger(element).getAttribute('aria-label')).toBe('2 tasks running, 62 percent');
  });

  it('puts a failure next to the running tasks in its name and in a red badge', async () => {
    const element = await show(
      { id: 1, items_done: 50, items_total: 100 },
      { id: 2, items_done: 74, items_total: 100, started_at: 2_000 },
      { id: 3, state: 'failed', error: 'disk full', finished_at: 9 },
    );

    expect(trigger(element).getAttribute('aria-label')).toBe(
      '2 tasks running, 62 percent, 1 failed',
    );
    expect(badge(element, 'error')?.textContent).toBe('1');
    expect(badge(element, 'error')?.getAttribute('title')).toBe('1 failed');
    expect(badge(element, 'ghost')).toBeNull();
  });

  it('puts the tasks that wait in a grey badge', async () => {
    const element = await show(
      { id: 1, items_done: 20, items_total: 100 },
      { id: 2, state: 'queued', started_at: null },
      { id: 3, state: 'queued', started_at: null },
    );

    expect(badge(element, 'ghost')?.textContent).toBe('2');
    expect(badge(element, 'ghost')?.getAttribute('title')).toBe('2 waiting');
    expect(badge(element, 'error')).toBeNull();
    expect(trigger(element).getAttribute('aria-label')).toBe(
      'Move 3 files running, 20 percent, 2 waiting',
    );
  });

  it('has an indeterminate bar and no share while no running task has a total', async () => {
    const element = await show({ items_done: 12 });

    expect(bar(element).hasAttribute('value')).toBe(false);
    expect(bar(element).classList.contains('progress-primary')).toBe(true);
    expect(percentText(element)).toBe('');
    expect(trigger(element).getAttribute('aria-label')).toBe('Move 3 files running');
  });

  it('switches from indeterminate to determinate when the total is known', async () => {
    const element = await show({ items_done: 12 });

    emit({ items_done: 12, items_total: 48 });
    await update(element);

    expect(bar(element).getAttribute('value')).toBe('25');
    expect(percentText(element)).toBe('25%');
  });

  it('says it is waiting when every task is queued', async () => {
    const element = await show(
      { id: 1, state: 'queued', started_at: null },
      { id: 2, state: 'queued', started_at: null },
    );

    expect(visibleLabel(element)).toBe('Waiting');
    expect(bar(element).getAttribute('value')).toBe('0');
    expect(trigger(element).getAttribute('aria-label')).toBe('2 waiting');
    expect(trigger(element).querySelector('span.tabular-nums.text-right')).toBeNull();
  });

  it('shows a lone failure with a full red bar', async () => {
    const element = await show({ state: 'failed', error: 'disk full', finished_at: 9 });

    expect(visibleLabel(element)).toBe('Failed');
    expect(bar(element).getAttribute('value')).toBe('100');
    expect(bar(element).classList.contains('progress-error')).toBe(true);
    expect(trigger(element).getAttribute('aria-label')).toBe('1 failed');
    expect(badge(element, 'error')?.textContent).toBe('1');
  });

  it('counts an interrupted task as a failure', async () => {
    const element = await show({
      state: 'interrupted',
      error: 'interrupted by restart',
      finished_at: 9,
    });

    expect(trigger(element).getAttribute('aria-label')).toBe('1 failed');
  });

  it('shows a task that has just been done with a full green bar', async () => {
    const element = await show({ state: 'done', finished_at: 9 });

    expect(visibleLabel(element)).toBe('Done');
    expect(bar(element).getAttribute('value')).toBe('100');
    expect(bar(element).classList.contains('progress-success')).toBe(true);
    expect(trigger(element).getAttribute('aria-label')).toBe('Tasks done');
    expect(badge(element, 'error')).toBeNull();
  });

  it('shows a task that has just been cancelled with a full warning bar', async () => {
    const element = await show({ state: 'cancelled', finished_at: 9 });

    expect(visibleLabel(element)).toBe('Cancelled');
    expect(bar(element).classList.contains('progress-warning')).toBe(true);
    expect(trigger(element).getAttribute('aria-label')).toBe('Tasks cancelled');
  });

  it('always contains its visible text in its accessible name', async () => {
    const element = await mount('apm-task-indicator');
    const cases: Partial<TaskDto>[][] = [
      [{ id: 1, items_done: 1, items_total: 2, title: 'Index photos' }],
      [
        { id: 1, items_done: 1, items_total: 2 },
        { id: 2, items_done: 1, items_total: 2, started_at: 2_000 },
      ],
      [{ id: 1, state: 'queued', started_at: null }],
      [{ id: 1, state: 'failed', error: 'boom', finished_at: 9 }],
      [{ id: 1, state: 'done', finished_at: 9 }],
    ];
    const labels: (string | null)[] = [];

    for (const list of cases) {
      resetTasksForTests();
      for (const task of list) emit(task);
      await update(element);

      const label = visibleLabel(element);
      expect(trigger(element).getAttribute('aria-label')?.toLowerCase()).toContain(
        label?.toLowerCase(),
      );
      labels.push(label);
    }

    expect(labels).toEqual(['Index photos', '2 tasks', 'Waiting', 'Failed', 'Done']);
  });

  it('renders a title as text, never as markup', async () => {
    const title = '<img src=x onerror="window.pwned = true"><b>bold</b>';
    const element = await show({ title });
    await rowsReady(element);

    expect(element.querySelector('img')).toBeNull();
    expect(element.querySelector('b')).toBeNull();
    expect(visibleLabel(element)).toBe(title);
    expect(trigger(element).getAttribute('aria-label')).toContain(title);
  });
});

describe('apm-task-indicator panel', () => {
  it('is a dropdown that opens to the left of the trigger, with a named group inside', async () => {
    const element = await show({ items_done: 1, items_total: 2 });

    expect(dropdown(element).classList.contains('dropdown-end')).toBe(true);
    const panel = need(element, '.dropdown-content');
    expect(panel.querySelector('.card[role="group"]')?.getAttribute('aria-label')).toBe(
      'Background tasks',
    );
    const list = need(panel, 'ul');
    expect(list.classList.contains('overflow-y-auto')).toBe(true);
    expect(list.classList.contains('max-h-[70vh]')).toBe(true);
  });

  it('is 24rem wide from the md breakpoint up and a screen-wide sheet below it', async () => {
    const element = await show({ items_done: 1, items_total: 2 });

    // A panel hung from the trigger would stick out on the left of a phone: the trigger is not flush
    // with the right edge there (the theme toggle follows it).
    const panel = need(element, '.dropdown-content');
    expect(panel.classList.contains('max-md:fixed')).toBe(true);
    expect(panel.classList.contains('max-md:inset-x-2')).toBe(true);
    const card = need(panel, '.card');
    expect(card.classList.contains('w-full')).toBe(true);
    expect(card.classList.contains('md:w-96')).toBe(true);
    expect(card.classList.contains('w-96')).toBe(false);
  });

  it('lists a row for every visible task: running first, then queued, then finished', async () => {
    const element = await show(
      { id: 5, state: 'queued', started_at: null },
      { id: 2, state: 'failed', error: 'boom', finished_at: 9 },
      { id: 7, started_at: 2_000 },
      { id: 6, started_at: 1_000 },
      { id: 4, state: 'queued', started_at: null },
    );

    expect(rows(element).map((row) => row.task?.id)).toEqual([6, 7, 4, 5, 2]);
  });

  it('hands every row its task, and the tasks being cancelled to the rows', async () => {
    let finish: (task: TaskDto) => void = () => undefined;
    post.mockReturnValue(new Promise<TaskDto>((resolve) => (finish = resolve)));
    const element = await show({ id: 1 }, { id: 2, started_at: 2_000 });

    void cancel(2);
    await update(element);

    expect(rows(element).map((row) => row.cancelling)).toEqual([false, true]);
    expect(rows(element).map((row) => row.task?.title)).toEqual(['Move 3 files', 'Move 3 files']);

    finish(makeTask({ id: 2, started_at: 2_000 }));
    await update(element);
    expect(rows(element).map((row) => row.cancelling)).toEqual([false, true]);

    // The flag stays until the task has ended.
    emit({ id: 2, state: 'cancelled', started_at: 2_000, finished_at: 9 });
    await update(element);
    expect(rows(element).map((row) => row.cancelling)).toEqual([false, false]);
  });

  it('shows the latest title of a task in its row and in the trigger', async () => {
    const element = await show({ title: 'Move 3 files' });

    emit({ title: 'Move 2 files' });
    await update(element);
    await rowsReady(element);

    expect(visibleLabel(element)).toBe('Move 2 files');
    expect(need(element, 'apm-task-row .truncate').textContent).toBe('Move 2 files');
  });

  it('cancels a task from its row', async () => {
    post.mockResolvedValue(makeTask({ id: 7, state: 'cancelled', finished_at: 9 }));
    const element = await show({ id: 7 });
    await rowsReady(element);

    need<HTMLButtonElement>(element, 'button[aria-label="Cancel Move 3 files"]').click();

    expect(post).toHaveBeenCalledExactlyOnceWith('/api/tasks/7/cancel');
  });

  it('keeps the row events to itself', async () => {
    post.mockResolvedValue(makeTask({ id: 7, state: 'cancelled', finished_at: 9 }));
    const element = await show({ id: 7 }, { id: 8, state: 'failed', error: 'x', finished_at: 5 });
    await rowsReady(element);
    const heard = vi.fn();
    document.body.addEventListener('task-cancel', heard);
    document.body.addEventListener('task-dismiss', heard);

    need<HTMLButtonElement>(element, 'button[aria-label^="Cancel"]').click();
    need<HTMLButtonElement>(element, 'button[aria-label^="Dismiss"]').click();
    await update(element);
    document.body.removeEventListener('task-cancel', heard);
    document.body.removeEventListener('task-dismiss', heard);

    // Both events were acted on, yet neither got past the indicator.
    expect(post).toHaveBeenCalledExactlyOnceWith('/api/tasks/7/cancel');
    expect(tasks.value.map((task) => task.id)).toEqual([7]);
    expect(heard).not.toHaveBeenCalled();
  });

  it('dismisses a failed task from its row and remembers it', async () => {
    const element = await show(
      { id: 3, state: 'failed', error: 'boom', finished_at: 5 },
      { id: 4 },
    );
    await rowsReady(element);

    need<HTMLButtonElement>(element, 'button[aria-label="Dismiss Move 3 files"]').click();
    await update(element);

    expect(rows(element).map((row) => row.task?.id)).toEqual([4]);
    expect(JSON.parse(localStorage.getItem(`${STORAGE_PREFIX}tasks.dismissed`) ?? 'null')).toEqual([
      3,
    ]);
  });

  it('offers to clear the finished tasks only while there are some', async () => {
    const element = await show({ id: 1 });
    const clear = () =>
      [...element.querySelectorAll('button')].find(
        (button) => button.textContent?.trim() === 'Clear finished',
      );
    expect(clear()).toBeUndefined();

    emit({ id: 2, state: 'failed', error: 'boom', finished_at: 5 });
    await update(element);

    expect(clear()).toBeDefined();
  });

  it('clears every finished task and keeps the running ones', async () => {
    const element = await show(
      { id: 1 },
      { id: 2, state: 'failed', error: 'boom', finished_at: 5 },
      { id: 3, state: 'interrupted', error: 'interrupted by restart', finished_at: 6 },
      { id: 4, state: 'done', finished_at: 7 },
    );
    const clear = [...element.querySelectorAll('button')].find(
      (button) => button.textContent?.trim() === 'Clear finished',
    );

    clear?.click();
    await update(element);

    expect(rows(element).map((row) => row.task?.id)).toEqual([1]);
    expect(tasks.value.map((task) => task.id)).toEqual([1]);
    expect(trigger(element).querySelector('.badge-error')).toBeNull();
  });
});

describe('apm-task-indicator opening and closing', () => {
  it('is closed to begin with', async () => {
    const element = await show({ id: 1 });

    expect(isOpen(element)).toBe(false);
    expect(dropdown(element).classList.contains('dropdown-close')).toBe(true);
    expect(trigger(element).getAttribute('aria-expanded')).toBe('false');
  });

  it('opens while the pointer is over it', async () => {
    const element = await show({ id: 1 });

    await hover(element);
    expect(isOpen(element)).toBe(true);
    expect(dropdown(element).classList.contains('dropdown-close')).toBe(false);
    expect(trigger(element).getAttribute('aria-expanded')).toBe('true');

    await unhover(element);
    expect(isOpen(element)).toBe(false);
    expect(trigger(element).getAttribute('aria-expanded')).toBe('false');
  });

  it('is not opened by a touch, which only emulates the hover around a tap', async () => {
    const element = await show({ id: 1 });

    pointer(dropdown(element), 'pointerenter', 'touch');
    await update(element);
    expect(isOpen(element)).toBe(false);

    await click(element);
    pointer(dropdown(element), 'pointerleave', 'touch');
    await update(element);
    expect(isOpen(element)).toBe(true);
  });

  it('opens while the trigger has the focus and closes when the focus goes elsewhere', async () => {
    const element = await show({ id: 1 });

    focusChange(trigger(element), 'focusin');
    await update(element);
    expect(isOpen(element)).toBe(true);

    focusChange(trigger(element), 'focusout', document.body);
    await update(element);
    expect(isOpen(element)).toBe(false);
  });

  it('stays open while the focus moves into the panel and back', async () => {
    const element = await show({ id: 1 });
    await rowsReady(element);
    const inside = need<HTMLButtonElement>(element, 'button[aria-label^="Cancel"]');

    focusChange(trigger(element), 'focusin');
    focusChange(trigger(element), 'focusout', inside);
    await update(element);
    expect(isOpen(element)).toBe(true);

    focusChange(inside, 'focusin', trigger(element));
    focusChange(inside, 'focusout', trigger(element));
    await update(element);
    expect(isOpen(element)).toBe(true);

    focusChange(trigger(element), 'focusin', inside);
    focusChange(trigger(element), 'focusout', document.body);
    await update(element);
    expect(isOpen(element)).toBe(false);
  });

  it('stays open while the pointer is inside the panel, though the trigger lost the focus', async () => {
    const element = await show({ id: 1 });

    focusChange(trigger(element), 'focusin');
    await hover(element);
    focusChange(trigger(element), 'focusout', null);
    await update(element);

    expect(isOpen(element)).toBe(true);
  });

  it('is pinned open by a click, also after the pointer has left', async () => {
    const element = await show({ id: 1 });
    await hover(element);

    await click(element);
    await unhover(element);

    expect(isOpen(element)).toBe(true);
  });

  it('is closed by a second click, though the pointer is still over it', async () => {
    const element = await show({ id: 1 });
    await hover(element);
    await click(element);

    await click(element);

    expect(isOpen(element)).toBe(false);
  });

  it('is opened again by the next click', async () => {
    const element = await show({ id: 1 });
    await click(element);
    await click(element);
    expect(isOpen(element)).toBe(false);

    await click(element);

    expect(isOpen(element)).toBe(true);
  });

  it('stays pinned when the focus goes to nothing, as a click on bare page does', async () => {
    const element = await show({ id: 1 });
    focusChange(trigger(element), 'focusin');
    await click(element);

    focusChange(trigger(element), 'focusout', null);
    await update(element);

    expect(isOpen(element)).toBe(true);
  });

  it('lets go of a pinned panel when the focus moves to another element', async () => {
    const element = await show({ id: 1 });
    focusChange(trigger(element), 'focusin');
    await click(element);

    focusChange(trigger(element), 'focusout', document.body);
    await update(element);

    expect(isOpen(element)).toBe(false);
  });

  describe('a press outside', () => {
    it('closes a pinned panel', async () => {
      const element = await show({ id: 1 });
      await click(element);

      pointer(document.body, 'pointerdown');
      await update(element);

      expect(isOpen(element)).toBe(false);
    });

    it('does not close it when the press is inside the panel', async () => {
      const element = await show({ id: 1 });
      await rowsReady(element);
      await click(element);

      pointer(need(element, 'button[aria-label^="Cancel"]'), 'pointerdown');
      pointer(trigger(element), 'pointerdown');
      await update(element);

      expect(isOpen(element)).toBe(true);
    });

    it('closes a panel the focus keeps open', async () => {
      const element = await show({ id: 1 });
      focusChange(trigger(element), 'focusin');
      await update(element);

      pointer(document.body, 'pointerdown');
      await update(element);

      expect(isOpen(element)).toBe(false);
    });
  });

  describe('Escape', () => {
    it('closes the panel from anywhere on the page and keeps the key to itself', async () => {
      const element = await show({ id: 1 });
      await hover(element);
      const reachedPage = vi.fn();
      window.addEventListener('keydown', reachedPage);

      const event = press(document.body, 'Escape');
      window.removeEventListener('keydown', reachedPage);
      await update(element);

      expect(isOpen(element)).toBe(false);
      expect(event.defaultPrevented).toBe(true);
      expect(reachedPage).not.toHaveBeenCalled();
    });

    it('keeps the panel closed while the pointer stays, and opens it on the next hover', async () => {
      const element = await show({ id: 1 });
      await hover(element);
      press(document.body, 'Escape');
      await update(element);
      expect(isOpen(element)).toBe(false);

      await unhover(element);
      await hover(element);

      expect(isOpen(element)).toBe(true);
    });

    it('unpins a panel that a click opened', async () => {
      const element = await show({ id: 1 });
      await click(element);

      press(document.body, 'Escape');
      await update(element);

      expect(isOpen(element)).toBe(false);
    });

    it('puts the focus back on the trigger when it was inside the panel', async () => {
      const element = await show({ id: 1 });
      await rowsReady(element);
      await hover(element);
      const inside = need<HTMLButtonElement>(element, 'button[aria-label^="Cancel"]');
      inside.focus();
      expect(document.activeElement).toBe(inside);

      press(inside, 'Escape');
      await update(element);

      expect(document.activeElement).toBe(trigger(element));
      expect(isOpen(element)).toBe(false);
    });

    it('leaves the focus alone when it was not in the indicator', async () => {
      const element = await show({ id: 1 });
      const elsewhere = document.createElement('button');
      document.body.append(elsewhere);
      elsewhere.focus();
      await hover(element);

      press(elsewhere, 'Escape');
      await update(element);
      const focused = document.activeElement;
      elsewhere.remove();

      expect(focused).toBe(elsewhere);
      expect(isOpen(element)).toBe(false);
    });

    it('stays closed when the trigger already had the focus and gets it back', async () => {
      const element = await show({ id: 1 });
      focusChange(trigger(element), 'focusin');
      await update(element);

      press(trigger(element), 'Escape');
      await update(element);
      focusChange(trigger(element), 'focusin', trigger(element));
      await update(element);

      expect(isOpen(element)).toBe(false);
    });

    it('leaves the key alone while the panel is closed', async () => {
      await show({ id: 1 });

      const event = press(document.body, 'Escape');

      expect(event.defaultPrevented).toBe(false);
    });

    it('ignores other keys and Escape with a modifier', async () => {
      const element = await show({ id: 1 });
      await hover(element);

      const keys = [
        press(document.body, 'Enter'),
        press(document.body, 'Escape', { ctrlKey: true }),
        press(document.body, 'Escape', { altKey: true }),
        press(document.body, 'Escape', { metaKey: true }),
      ];
      await update(element);

      expect(keys.map((event) => event.defaultPrevented)).toEqual([false, false, false, false]);
      expect(isOpen(element)).toBe(true);
    });

    it('leaves the key to a modal dialog', async () => {
      const element = await show({ id: 1 });
      await hover(element);
      const dialog = document.createElement('dialog');
      dialog.setAttribute('open', '');
      document.body.append(dialog);

      const event = press(dialog, 'Escape');
      dialog.remove();
      await update(element);

      expect(event.defaultPrevented).toBe(false);
      expect(isOpen(element)).toBe(true);
    });

    it('is not listened to any more once the panel is closed', async () => {
      const element = await show({ id: 1 });
      await hover(element);
      await unhover(element);

      expect(press(document.body, 'Escape').defaultPrevented).toBe(false);
    });
  });

  it('closes and forgets how it was opened when the last task goes away', async () => {
    const element = await mount('apm-task-indicator');
    vi.useFakeTimers();
    emit({ id: 1 });
    await update(element);
    await hover(element);
    await click(element);
    expect(isOpen(element)).toBe(true);

    emit({ id: 1, state: 'done', finished_at: 5 });
    vi.advanceTimersByTime(FINISHED_LINGER_MS);
    await update(element);

    expect(element.children).toHaveLength(0);
    expect(press(document.body, 'Escape').defaultPrevented).toBe(false);

    emit({ id: 9 });
    await update(element);

    expect(isOpen(element)).toBe(false);
  });

  it('gives back its listeners when it leaves the page', async () => {
    const element = await show({ id: 1 });
    await hover(element);

    element.remove();

    expect(press(document.body, 'Escape').defaultPrevented).toBe(false);
  });

  it('hands the focus to the trigger when a dismissed row takes it away', async () => {
    const element = await show(
      { id: 3, state: 'failed', error: 'boom', finished_at: 5 },
      { id: 4 },
    );
    await rowsReady(element);
    await hover(element);
    const dismissButton = need<HTMLButtonElement>(element, 'button[aria-label^="Dismiss"]');
    dismissButton.focus();
    expect(document.activeElement).toBe(dismissButton);

    dismissButton.click();
    await update(element);
    await Promise.resolve();

    expect(document.activeElement).toBe(trigger(element));
  });

  it('leaves the focus alone when a row is dismissed with the pointer', async () => {
    const element = await show(
      { id: 3, state: 'failed', error: 'boom', finished_at: 5 },
      { id: 4 },
    );
    await rowsReady(element);
    const elsewhere = document.createElement('button');
    document.body.append(elsewhere);
    elsewhere.focus();

    need<HTMLButtonElement>(element, 'button[aria-label^="Dismiss"]').click();
    await update(element);
    await Promise.resolve();
    const focused = document.activeElement;
    elsewhere.remove();

    expect(focused).toBe(elsewhere);
  });
});

describe('apm-task-indicator announcements', () => {
  it('has ONE polite live region, and it is hidden from sight', async () => {
    const element = await show({ id: 1 });

    expect(element.querySelectorAll('[aria-live]')).toHaveLength(1);
    expect(liveRegion(element).getAttribute('aria-live')).toBe('polite');
    expect(liveRegion(element).getAttribute('aria-atomic')).toBe('false');
    expect(liveRegion(element).classList.contains('sr-only')).toBe(true);
    expect(announced(element)).toEqual([]);
  });

  it.each([
    ['done', 'Move 120 files', 'Move 120 files finished'],
    ['failed', 'Indexing', 'Indexing failed'],
    ['cancelled', 'Move 120 files', 'Move 120 files was cancelled'],
    ['interrupted', 'Indexing', 'Indexing was interrupted'],
  ] as const)('says when a task that was %s ended', async (state, title, text) => {
    const element = await show({ id: 1, title });

    emit({ id: 1, title, state, error: state === 'failed' ? 'boom' : null, finished_at: 5 });
    await update(element);

    expect(announced(element)).toEqual([text]);
    expect(liveRegion(element).textContent?.trim()).toBe(text);
  });

  it('never announces progress', async () => {
    const element = await show({ id: 1, items_done: 1, items_total: 100 });

    for (let done = 2; done <= 90; done += 8) {
      emit({ id: 1, items_done: done, items_total: 100 });
    }
    emit({ id: 2, state: 'queued', started_at: null });
    emit({ id: 2, state: 'running', started_at: 2_000 });
    await update(element);

    expect(announced(element)).toEqual([]);
  });

  it('says it once, however often the end is reported', async () => {
    const element = await show({ id: 1, title: 'Move 3 files' });

    emit({ id: 1, state: 'done', finished_at: 5 });
    emit({ id: 1, state: 'done', finished_at: 5 });
    await update(element);

    expect(announced(element)).toEqual(['Move 3 files finished']);
  });

  it('keeps every task that ended together, even with the same title', async () => {
    const element = await show({ id: 1 }, { id: 2, started_at: 2_000 });

    emit({ id: 1, state: 'done', finished_at: 5 });
    emit({ id: 2, state: 'done', started_at: 2_000, finished_at: 5 });
    await update(element);

    expect(announced(element)).toEqual(['Move 3 files finished', 'Move 3 files finished']);
  });

  it('keeps only the latest few', async () => {
    const element = await show(
      ...Array.from({ length: 7 }, (_, index) => ({ id: index + 1, title: `Task ${index + 1}` })),
    );

    for (let id = 1; id <= 7; id += 1) {
      emit({ id, title: `Task ${id}`, state: 'done', finished_at: 5 });
    }
    await update(element);

    expect(announced(element)).toEqual([
      'Task 3 finished',
      'Task 4 finished',
      'Task 5 finished',
      'Task 6 finished',
      'Task 7 finished',
    ]);
  });

  it('is emptied when the indicator has nothing left to show', async () => {
    const element = await mount('apm-task-indicator');
    emit({ id: 1 });
    await update(element);
    vi.useFakeTimers();

    emit({ id: 1, state: 'done', finished_at: 5 });
    await update(element);
    expect(announced(element)).toEqual(['Move 3 files finished']);

    vi.advanceTimersByTime(FINISHED_LINGER_MS);
    await update(element);
    expect(element.children).toHaveLength(0);

    emit({ id: 2 });
    await update(element);
    expect(announced(element)).toEqual([]);
  });

  it('does not announce a task that was already over when the page loaded', async () => {
    vi.spyOn(api, 'get').mockResolvedValue([
      makeTask({ id: 8, state: 'failed', error: 'boom', finished_at: Date.now() / 1000 - 60 }),
    ]);
    const element = await mount('apm-task-indicator');
    stop?.();
    stop = startTasks();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await update(element);

    expect(trigger(element).getAttribute('aria-label')).toBe('1 failed');
    expect(announced(element)).toEqual([]);
  });

  it('stops listening when it leaves the page', async () => {
    const element = await show({ id: 1 });

    element.remove();
    emit({ id: 1, state: 'done', finished_at: 5 });
    await update(element);

    expect(announced(element)).toEqual([]);
  });

  it('listens again when it comes back', async () => {
    const element = await show({ id: 1 });
    element.remove();
    document.body.append(element);

    emit({ id: 1, state: 'done', finished_at: 5 });
    await update(element);

    expect(announced(element)).toEqual(['Move 3 files finished']);
  });
});
