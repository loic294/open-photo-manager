import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { readdir, stat } from 'node:fs/promises';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { formatFileSize } from '../../../web/src/util/format';
import { CrossDevice } from '../../src/cross-device';
import { makeSparseFile, writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';

const COPY_BYTES = 4 * 1024 ** 3;
const SOURCE_DIR = 'e2e-cross-device';
const FILE_NAME = 'large-copy.bin';
const SOURCE_PATH = `${SOURCE_DIR}/${FILE_NAME}`;
const PART_SUFFIX = '.apm-part';

test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

async function startMove(
  app: import('../../src/app-instance').AppInstance,
  page: import('@playwright/test').Page,
  source: import('../../src/pages/pane').PanePage,
  movePage: import('../../src/pages/move-page').MovePage,
  confirmDialog: import('../../src/pages/confirm-dialog').ConfirmDialogPage,
  options: {
    sourceDir?: string;
    fileName?: string;
    destinationVolume?: string;
    fileBytes?: number;
  } = {},
) {
  const sourceDir = options.sourceDir ?? SOURCE_DIR;
  const fileName = options.fileName ?? FILE_NAME;
  const destinationVolume = options.destinationVolume ?? 'system-cross-device';
  const fileBytes = options.fileBytes ?? COPY_BYTES;
  const url = new URL('/move', app.url);
  url.searchParams.set('src', `photos/${sourceDir}`);
  url.searchParams.set('cmp', destinationVolume);
  await page.goto(url.href);
  await source.useFolder();
  const destinationPlan = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
  await destinationPlan;
  await source.card(fileName).select();
  const selectionResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/selection$/.test(new URL(response.url()).pathname),
  );
  await movePage.actionButton().click();
  const selection = await (await selectionResponse).json();
  expect(selection).toMatchObject({ item_count: 1, total_size: fileBytes });
  await expect(confirmDialog.dialog()).toContainText('1 file');
  await expect(confirmDialog.dialog()).toContainText(formatFileSize(fileBytes));
  const executeResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/execute$/.test(new URL(response.url()).pathname),
  );
  await confirmDialog.confirm('Move 1 file');
  const response = await executeResponse;
  const task = (await response.json()) as { id: number };
  return {
    id: task.id,
    planId: Number(new URL(response.url()).pathname.split('/')[3]),
  };
}

async function waitForCopyProgress(
  app: import('../../src/app-instance').AppInstance,
  shell: import('../../src/pages/shell').ShellPage,
  taskId: number,
) {
  await expect
    .poll(async () => (await app.api.tasks()).find((task) => task.id === taskId)?.state)
    .toBe('running');
  const taskTitle = (await app.api.tasks()).find((task) => task.id === taskId)?.title;
  if (!taskTitle) throw new Error(`Running task ${taskId} is missing from the task list`);
  await shell.hoverTasks();
  const row = shell
    .taskIndicator()
    .getByRole('group', { name: 'Background tasks' })
    .locator('apm-task-row')
    .filter({ hasText: taskTitle });
  await expect(row).toBeVisible();
  await expect(row.getByRole('progressbar')).toBeVisible();
  await expect
    .poll(async () => Number(await row.getByRole('progressbar').getAttribute('value')))
    .toBeGreaterThan(0);
  await expect(row).toContainText(/\d+(?:\.\d+)?\s*MB\/s/);
  await expect(row).toContainText(/left/);
  return row;
}

async function writeCopySource(app: import('../../src/app-instance').AppInstance) {
  // Keep the private fixture sparse while the copy still reads and verifies all 4 GiB.
  await makeSparseFile(app, 'photos', SOURCE_PATH, COPY_BYTES);
  const hash = createHash('sha1');
  const zeros = Buffer.alloc(16 * 1024 ** 2);
  for (let written = 0; written < COPY_BYTES; written += zeros.byteLength) hash.update(zeros);
  return hash.digest('hex');
}

async function hashFile(path: string) {
  const hash = createHash('sha1');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

async function expectNoPartFiles(volumePath: string) {
  async function visit(path: string): Promise<string[]> {
    const files: string[] = [];
    for (const entry of await readdir(path, { withFileTypes: true })) {
      const child = `${path}/${entry.name}`;
      if (entry.isDirectory()) files.push(...(await visit(child)));
      else if (entry.name.endsWith(PART_SUFFIX)) files.push(child);
    }
    return files;
  }
  expect(await visit(volumePath)).toEqual([]);
}

async function waitForSustainedProgress(
  app: import('../../src/app-instance').AppInstance,
  taskId: number,
  startedAt: number,
) {
  await expect
    .poll(async () => {
      const current = (await app.api.tasks()).find((entry) => entry.id === taskId);
      return current?.state === 'running' && performance.now() - startedAt >= 15_000;
    })
    .toBe(true);
}

test('cross-volume move reports progress, queues later work, and preserves a cancelled source', async ({
  app,
  page,
  source,
  movePage,
  confirmDialog,
  shell,
}) => {
  test.setTimeout(240_000);
  test.skip(process.platform !== 'darwin', 'Private APFS cross-device fixtures require macOS');
  const device = await CrossDevice.create(app, COPY_BYTES);
  try {
    expect(device.evidence.source).not.toBe(device.evidence.destination);
    expect(device.evidence.renameErrno).toBe('EXDEV');
    const sourceHash = await writeCopySource(app);
    const queuedName = 'queued-small.txt';
    const queuedBytes = 'A small move queued behind the cross-volume copy';
    await writeFile(app, 'photos', `${SOURCE_DIR}/${queuedName}`, queuedBytes);
    await app.rescan();
    const task = await startMove(app, page, source, movePage, confirmDialog);
    await waitForCopyProgress(app, shell, task.id);
    await waitForSustainedProgress(app, task.id, performance.now());

    const queuedTask = await startMove(app, page, source, movePage, confirmDialog, {
      fileName: queuedName,
      destinationVolume: 'archive',
      fileBytes: queuedBytes.length,
    });
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === queuedTask.id)?.state)
      .toBe('queued');
    const row = await waitForCopyProgress(app, shell, task.id);
    const cancel = row.getByRole('button', { name: /^Cancel / });
    await cancel.click();
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task.id)?.state)
      .toBe('cancelled');
    const cancelledPlan = await app.api.request<PlanDto>('GET', `/api/plans/${task.planId}`);
    expect(cancelledPlan.result).toMatchObject({ cancelled: true });
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === queuedTask.id)?.state)
      .toBe('done');
    await expect
      .poll(() => app.api.listing('archive', SOURCE_DIR))
      .toMatchObject({
        items: expect.arrayContaining([expect.objectContaining({ name: queuedName })]),
      });
    await expect
      .poll(() => app.api.listing('photos', SOURCE_DIR))
      .toMatchObject({
        items: expect.not.arrayContaining([expect.objectContaining({ name: queuedName })]),
      });
    const queuedPlan = await app.api.request<PlanDto>('GET', `/api/plans/${queuedTask.planId}`);
    expect(queuedPlan.result).toMatchObject({
      done: 1,
      skipped: 0,
      failed: 0,
      bytes: queuedBytes.length,
    });
    const sourcePath = app.volumePath(`photos/${SOURCE_PATH}`);
    const sourceBeforeSuccessfulMove = await stat(sourcePath);
    expect(sourceBeforeSuccessfulMove.size).toBe(COPY_BYTES);
    expect(await hashFile(sourcePath)).toBe(sourceHash);
    await expect(stat(app.volumePath(`${device.volume}/${FILE_NAME}`))).rejects.toMatchObject({
      code: 'ENOENT',
    });
    await expect
      .poll(() => app.api.listing('photos', SOURCE_DIR))
      .toMatchObject({
        items: expect.arrayContaining([expect.objectContaining({ name: FILE_NAME })]),
      });
    await expect
      .poll(() => app.api.listing(device.volume, ''))
      .toMatchObject({
        items: expect.not.arrayContaining([expect.objectContaining({ name: FILE_NAME })]),
      });
    await expectNoPartFiles(app.volumePath(`photos/${SOURCE_DIR}`));
    await expectNoPartFiles(device.mount);

    const completedTask = await startMove(app, page, source, movePage, confirmDialog);
    await waitForCopyProgress(app, shell, completedTask.id);
    await waitForSustainedProgress(app, completedTask.id, performance.now());
    await expect
      .poll(
        async () => (await app.api.tasks()).find((entry) => entry.id === completedTask.id)?.state,
      )
      .toBe('done');
    const destinationPath = app.volumePath(`${device.volume}/${FILE_NAME}`);
    const destinationStat = await stat(destinationPath);
    expect(destinationStat.size).toBe(COPY_BYTES);
    expect(destinationStat.mtimeMs).toBe(sourceBeforeSuccessfulMove.mtimeMs);
    expect(await hashFile(destinationPath)).toBe(sourceHash);
    await expect(stat(app.volumePath(`photos/${SOURCE_PATH}`))).rejects.toMatchObject({
      code: 'ENOENT',
    });
    await expect
      .poll(() => app.api.listing(device.volume, ''))
      .toMatchObject({
        items: expect.arrayContaining([expect.objectContaining({ name: FILE_NAME })]),
      });
    await expect
      .poll(() => app.api.listing('photos', SOURCE_DIR))
      .toMatchObject({
        items: expect.not.arrayContaining([expect.objectContaining({ name: FILE_NAME })]),
      });
    const plan = await app.api.request<PlanDto>('GET', `/api/plans/${completedTask.planId}`);
    expect(plan.result).toMatchObject({ done: 1, skipped: 0, failed: 0, bytes: COPY_BYTES });
    await expectNoPartFiles(app.volumePath(`photos/${SOURCE_DIR}`));
    await expectNoPartFiles(device.mount);
  } finally {
    await device.close();
  }
});

test('server restart interrupts an in-progress cross-volume copy without losing the source', async ({
  app,
  page,
  source,
  movePage,
  confirmDialog,
  shell,
}) => {
  test.setTimeout(240_000);
  test.skip(process.platform !== 'darwin', 'Private APFS cross-device fixtures require macOS');
  const device = await CrossDevice.create(app, COPY_BYTES);
  try {
    expect(device.evidence.renameErrno).toBe('EXDEV');
    const sourceHash = await writeCopySource(app);
    await app.rescan();
    const sourcePath = app.volumePath(`photos/${SOURCE_PATH}`);
    const sourceBeforeRestart = await stat(sourcePath);
    const task = await startMove(app, page, source, movePage, confirmDialog);
    await waitForCopyProgress(app, shell, task.id);
    await waitForSustainedProgress(app, task.id, performance.now());

    await app.kill();
    await app.restart();
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task.id)?.state)
      .toBe('interrupted');
    await page.reload();
    const sourceAfterRestart = await stat(sourcePath);
    expect(sourceAfterRestart.size).toBe(COPY_BYTES);
    expect(await hashFile(sourcePath)).toBe(sourceHash);
    await expect(stat(app.volumePath(`${device.volume}/${FILE_NAME}`))).rejects.toMatchObject({
      code: 'ENOENT',
    });
    await expect
      .poll(() => app.api.listing('photos', SOURCE_DIR))
      .toMatchObject({
        items: expect.arrayContaining([expect.objectContaining({ name: FILE_NAME })]),
      });
    await expect
      .poll(() => app.api.listing(device.volume, ''))
      .toMatchObject({
        items: expect.not.arrayContaining([expect.objectContaining({ name: FILE_NAME })]),
      });
    const interruptedPlan = await app.api.request<PlanDto>('GET', `/api/plans/${task.planId}`);
    expect(interruptedPlan.result).toMatchObject({
      done: 0,
      cancelled: true,
      note: 'interrupted by restart',
    });
    expect(sourceAfterRestart.mtimeMs).toBe(sourceBeforeRestart.mtimeMs);
    await expectNoPartFiles(app.volumePath(`photos/${SOURCE_DIR}`));
    await expectNoPartFiles(device.mount);
  } finally {
    await device.close();
  }
});
