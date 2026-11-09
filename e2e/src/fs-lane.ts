import { expect, type Page } from '@playwright/test';
import type { TaskDto } from '../../web/src/api/generated/TaskDto';
import { formatFileSize } from '../../web/src/util/format';
import type { AppInstance } from './app-instance';
import type { ConfirmDialogPage } from './pages/confirm-dialog';
import type { MovePage } from './pages/move-page';
import type { PanePage } from './pages/pane';

export async function startFileMove(
  app: AppInstance,
  page: Page,
  source: PanePage,
  movePage: MovePage,
  confirmDialog: ConfirmDialogPage,
  options: {
    sourceVolume: string;
    sourceDir: string;
    fileName: string;
    destinationVolume: string;
    fileBytes: number;
  },
) {
  const url = new URL('/move', app.url);
  url.searchParams.set('src', `${options.sourceVolume}/${options.sourceDir}`);
  url.searchParams.set('cmp', options.destinationVolume);
  await page.goto(url.href);
  await source.useFolder();
  const destinationPlan = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
  await destinationPlan;
  await source.card(options.fileName).select();
  const selectionResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/selection$/.test(new URL(response.url()).pathname),
  );
  await movePage.actionButton().click();
  const selection = await (await selectionResponse).json();
  expect(selection).toMatchObject({ item_count: 1, total_size: options.fileBytes });
  await expect(confirmDialog.dialog()).toContainText('1 file');
  await expect(confirmDialog.dialog()).toContainText(formatFileSize(options.fileBytes));
  const executeResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/execute$/.test(new URL(response.url()).pathname),
  );
  await confirmDialog.confirm('Move 1 file');
  const response = await executeResponse;
  const task = (await response.json()) as { id: number };
  return { id: task.id, planId: Number(new URL(response.url()).pathname.split('/')[3]) };
}

export interface RunningCopy {
  task: TaskDto;
  observedAt: number;
}

/** Call only for a real copy onto a CrossDevice volume, not an ordinary same-device move. */
export async function observeRunningCopy(app: AppInstance, taskId: number): Promise<RunningCopy> {
  let observed: TaskDto | undefined;
  await expect
    .poll(async () => {
      observed = (await app.api.tasks()).find((task) => task.id === taskId);
      return (
        observed?.state === 'running' &&
        observed.bytes_done > 0 &&
        observed.bytes_total !== null &&
        observed.bytes_done < observed.bytes_total
      );
    })
    .toBe(true);
  if (!observed) throw new Error(`Running copy ${taskId} is missing from the task list`);
  return { task: observed, observedAt: performance.now() };
}

/** Both states come from one task-list response; a completed blocker cannot prove queueing. */
export async function expectQueuedBehindCopy(
  app: AppInstance,
  copy: RunningCopy,
  queuedTaskId: number,
) {
  await expect
    .poll(async () => {
      const tasks = await app.api.tasks();
      const blocker = tasks.find((task) => task.id === copy.task.id);
      const queued = tasks.find((task) => task.id === queuedTaskId);
      return (
        blocker?.state === 'running' &&
        blocker.bytes_done > 0 &&
        blocker.bytes_total !== null &&
        blocker.bytes_done < blocker.bytes_total &&
        queued?.state === 'queued'
      );
    })
    .toBe(true);
}

export async function waitForSustainedCopy(app: AppInstance, copy: RunningCopy) {
  await expect
    .poll(
      async () => {
        const current = (await app.api.tasks()).find((task) => task.id === copy.task.id);
        return current?.state === 'running' && performance.now() - copy.observedAt >= 15_000;
      },
      { timeout: 45_000, intervals: [500, 1000] },
    )
    .toBe(true);
}
