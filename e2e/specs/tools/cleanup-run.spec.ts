import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { diffTrees, snapshotTree, trashEntries } from '../../src/disk';
import { expect, test } from '../../src/test';
import { expectedEmptyDirs } from '../../src/tool-oracles';

// Navigating to the Trash page closes the server-sent events connection.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('@smoke cleanup removes confirmed empty folders and trashes their junk', async ({
  app,
  page,
  manifest,
  comparison,
  cleanupPage,
  confirmDialog,
  trashPage,
}) => {
  const expected = expectedEmptyDirs(manifest, { volume: 'photos', dir: 'empty' });
  const planResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  const url = new URL('/cleanup', app.url);
  url.searchParams.set('src', 'photos/empty');
  url.searchParams.set('cmp', 'photos/empty');
  await page.goto(url.href);
  const plan = (await (await planResponse).json()) as PlanDto;
  await expect(cleanupPage.actionButton()).toContainText(
    `Delete empty folders (${expected.length})`,
  );
  const before = await snapshotTree(app.volumesDir, { hash: true });
  await cleanupPage.actionButton().click();
  await expect(confirmDialog.dialog()).toContainText(`${expected.length} empty folders`);
  await expect(confirmDialog.dialog()).toContainText('junk files');
  const executeResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/execute$/.test(new URL(response.url()).pathname),
  );
  await confirmDialog.confirm(`Delete ${expected.length} empty folders`);
  const task = (await (await executeResponse).json()) as { id: number };
  await expect
    .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task.id)?.state)
    .toBe('done');

  const after = await snapshotTree(app.volumesDir, { hash: true });
  const changes = diffTrees(before, after);
  const removedFolders = changes.removed
    .filter((entry) => entry.type === 'directory')
    .map((entry) => entry.path.replace(/^photos\//, ''))
    .filter((path) => path.startsWith('empty/'))
    .filter((path) => !path.endsWith('/@eaDir'))
    .sort();
  expect(removedFolders).toEqual(expected.sort());
  const removedFiles = changes.removed.filter((entry) => entry.type === 'file');
  const trashedFiles = after.filter(
    (entry) => entry.type === 'file' && entry.path.includes('/.apm-trash/'),
  );
  for (const removed of removedFiles) {
    expect(
      trashedFiles.some((entry) => entry.hash === removed.hash),
      removed.path,
    ).toBe(true);
  }
  expect(await trashEntries(app)).not.toHaveLength(0);
  const executedPlan = (await app.api.request<PlanDto>('GET', `/api/plans/${plan.id}`)).result;
  expect(executedPlan).toMatchObject({ done: expected.length, skipped: 0, failed: 0 });
  await expect.poll(() => comparison.folderState('mixed')).toMatchObject({ red: false });

  await page.goto(new URL('/trash', app.url).href);
  await trashPage.tab('Deletions');
  const batchRow = page
    .locator('apm-trash-deletions tbody tr')
    .filter({ hasText: 'photos' })
    .first();
  await expect(batchRow).toBeVisible();
  await batchRow.getByRole('button', { name: 'Restore all', exact: true }).click();
  await expect(confirmDialog.dialog()).toBeVisible();
  const restoreLabel = (await confirmDialog.text())?.match(/Restore (\d+ items?)(?:\s|\?)/)?.[1];
  expect(restoreLabel).toBeDefined();
  const restoreResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/trash/restore',
  );
  await confirmDialog.confirm(`Restore ${restoreLabel}`);
  const restoreTask = (await (await restoreResponse).json()) as { id: number };
  await expect
    .poll(async () => (await app.api.tasks()).find((entry) => entry.id === restoreTask.id)?.state)
    .toBe('done');
  const restored = await snapshotTree(app.volumesDir);
  expect(restored.some((entry) => entry.path === 'photos/empty/junk_only/.DS_Store')).toBe(true);
  expect(restored.some((entry) => entry.path === 'photos/empty/junk_only/Thumbs.db')).toBe(true);
  expect(await trashEntries(app)).toHaveLength(0);
});
