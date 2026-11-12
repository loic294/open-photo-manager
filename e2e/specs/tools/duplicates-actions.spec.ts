import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { formatFileSize } from '../../../web/src/util/format';
import { diffTrees, snapshotTree, trashEntries } from '../../src/disk';
import { expect, test } from '../../src/test';

// Navigating away closes the server-sent events connection.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

function taskResponse(page: import('@playwright/test').Page, path: RegExp) {
  return page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && path.test(new URL(response.url()).pathname),
  );
}

async function waitForTask(app: import('../../src/app-instance').AppInstance, id: number) {
  await expect
    .poll(async () => (await app.api.tasks()).find((task) => task.id === id)?.state)
    .toBe('done');
}

test('duplicate action choice survives reload and browser context restart', async ({
  app,
  page,
  browser,
  duplicatesPage,
}) => {
  const url = new URL('/duplicates', app.url);
  url.searchParams.set('src', 'photos/dupes/m1');
  url.searchParams.set('cmp', 'archive/m1');
  const planResponse = taskResponse(page, /^\/api\/plans$/);
  await page.goto(url.href);
  await planResponse;
  await duplicatesPage.chooseAction('Delete source items');
  await expect(duplicatesPage.actionButton()).toContainText('Delete source items');

  await page.reload();
  await expect(duplicatesPage.actionButton()).toContainText('Delete source items');

  const storageState = await page.context().storageState();
  const restartedContext = await browser.newContext({ storageState });
  try {
    const restartedPage = await restartedContext.newPage();
    await restartedPage.goto(url.href);
    await expect(
      restartedPage.locator('[data-floating-actions] opm-split-button .join > button').first(),
    ).toContainText('Delete source items');
  } finally {
    await restartedContext.close();
  }
});

test('@smoke duplicate deletion is counted, trashed and restorable', async ({
  app,
  page,
  manifest,
  comparison,
  duplicatesPage,
  confirmDialog,
  trashPage,
}) => {
  const sourceFile = manifest.file('photos', 'dupes/m1/IMG_0300.jpg');
  const comparisonFile = manifest.file('archive', 'm1/vacation_copy.jpg');
  const url = new URL('/duplicates', app.url);
  url.searchParams.set('src', 'photos/dupes/m1');
  url.searchParams.set('cmp', 'archive/m1');
  const planResponse = taskResponse(page, /^\/api\/plans$/);
  await page.goto(url.href);
  const plan = (await (await planResponse).json()) as PlanDto;
  expect(plan.stats.comparison_matched).toBeGreaterThan(0);
  await expect(duplicatesPage.actionButton()).toContainText('Delete comparison items');
  const before = await snapshotTree(app.volumesDir, { hash: true });

  const selectionResponse = taskResponse(page, /^\/api\/plans\/\d+\/selection$/);
  await duplicatesPage.actionButton().click();
  const selection = (await (await selectionResponse).json()) as SelectionDto;
  expect(selection.item_count).toBeGreaterThan(0);
  await expect(confirmDialog.dialog()).toBeVisible();
  const confirmation = await confirmDialog.text();
  expect(confirmation).toContain(`Delete ${selection.item_count} files?`);
  expect(confirmation).toContain(formatFileSize(selection.total_size));
  expect(confirmation).toContain('moved to the trash');
  await confirmDialog.cancel();
  expect(diffTrees(before, await snapshotTree(app.volumesDir, { hash: true }))).toEqual({
    added: [],
    removed: [],
    changed: [],
    moved: [],
  });

  const secondSelection = taskResponse(page, /^\/api\/plans\/\d+\/selection$/);
  await duplicatesPage.actionButton().click();
  const confirmedSelection = (await (await secondSelection).json()) as SelectionDto;
  const executeResponse = taskResponse(page, /^\/api\/plans\/\d+\/execute$/);
  await confirmDialog.confirm(`Delete ${confirmedSelection.item_count} files`);
  const task = (await (await executeResponse).json()) as { id: number };
  await waitForTask(app, task.id);

  const after = await snapshotTree(app.volumesDir, { hash: true });
  const changes = diffTrees(before, after);
  const removedFiles = changes.removed.filter((entry) => entry.type === 'file');
  const trashedFiles = after.filter(
    (entry) => entry.type === 'file' && entry.path.includes('/.opm-trash/'),
  );
  expect(removedFiles.length).toBeGreaterThanOrEqual(confirmedSelection.item_count);
  expect(changes.removed.filter((entry) => entry.type !== 'file')).toEqual([]);
  for (const removed of removedFiles) {
    expect(
      trashedFiles.some((entry) => entry.hash === removed.hash),
      removed.path,
    ).toBe(true);
  }
  expect(await trashEntries(app)).not.toHaveLength(0);
  expect(removedFiles.some((entry) => entry.path.endsWith(sourceFile.path))).toBe(false);
  expect(removedFiles.some((entry) => entry.path.endsWith(comparisonFile.path))).toBe(true);

  const executedPlan = (await app.api.request<PlanDto>('GET', `/api/plans/${plan.id}`)).result;
  expect(executedPlan).not.toBeNull();
  expect(executedPlan?.done).toBe(confirmedSelection.item_count);
  await expect.poll(() => matchedNames(comparison)).toEqual([]);
  expect(
    (await app.api.listing('archive', 'm1')).items.some(
      (item) => item.name === 'vacation_copy.jpg',
    ),
  ).toBe(false);

  await page.goto(new URL('/trash', app.url).href);
  await trashPage.tab('Deletions');
  const batchRow = page
    .locator('opm-trash-deletions tbody tr')
    .filter({ hasText: 'archive' })
    .first();
  await expect(batchRow).toBeVisible();
  await batchRow.getByRole('button', { name: 'Restore all', exact: true }).click();
  await expect(confirmDialog.dialog()).toBeVisible();
  const restoreLabel = (await confirmDialog.text())?.match(/Restore (\d+ items?)(?:\s|\?)/)?.[1];
  expect(restoreLabel).toBeDefined();
  const restoreResponse = taskResponse(page, /^\/api\/trash\/restore$/);
  await confirmDialog.confirm(`Restore ${restoreLabel}`);
  const restoreTask = (await (await restoreResponse).json()) as { id: number };
  await waitForTask(app, restoreTask.id);
  await expect
    .poll(() => app.api.listing('archive', 'm1'))
    .toMatchObject({
      items: expect.arrayContaining([expect.objectContaining({ name: 'vacation_copy.jpg' })]),
    });
  expect(await trashEntries(app)).toHaveLength(0);
  const restoredPlanResponse = taskResponse(page, /^\/api\/plans$/);
  const restoredUrl = new URL('/duplicates', app.url);
  restoredUrl.searchParams.set('src', 'photos/dupes/m1');
  restoredUrl.searchParams.set('cmp', 'archive/m1');
  await page.goto(restoredUrl.href);
  await restoredPlanResponse;
  await expect.poll(() => matchedNames(comparison)).toContain('vacation_copy.jpg');
});

test('duplicate deletion can target the source while preserving its comparison copy', async ({
  app,
  page,
  manifest,
  source,
  comparison,
  duplicatesPage,
  confirmDialog,
}) => {
  const sourceFile = manifest.file('photos', 'dupes/m1/IMG_0300.jpg');
  const url = new URL('/duplicates', app.url);
  url.searchParams.set('src', 'photos/dupes/m1');
  url.searchParams.set('cmp', 'archive/m1');
  const planResponse = taskResponse(page, /^\/api\/plans$/);
  await page.goto(url.href);
  const plan = (await (await planResponse).json()) as PlanDto;
  expect(plan.stats.source_matched).toBeGreaterThan(0);

  await duplicatesPage.chooseAction('Delete source items');
  await expect(duplicatesPage.actionButton()).toContainText('Delete source items');
  const selectionResponse = taskResponse(page, /^\/api\/plans\/\d+\/selection$/);
  await duplicatesPage.actionButton().click();
  const selection = (await (await selectionResponse).json()) as SelectionDto;
  expect(selection.item_count).toBeGreaterThan(0);
  await expect(confirmDialog.dialog()).toContainText(`Delete ${selection.item_count} files?`);
  await expect(confirmDialog.dialog()).toContainText(formatFileSize(selection.total_size));

  const executeResponse = taskResponse(page, /^\/api\/plans\/\d+\/execute$/);
  await confirmDialog.confirm(`Delete ${selection.item_count} files`);
  const executed = await executeResponse;
  const task = (await executed.json()) as { id: number };
  await waitForTask(app, task.id);

  const after = await snapshotTree(app.volumesDir, { hash: true });
  expect(
    after.some((entry) => entry.path.includes('/.opm-trash/') && entry.hash === sourceFile.sha1),
  ).toBe(true);
  await expect.poll(() => matchedNames(source)).toEqual([]);
  await expect
    .poll(() => app.api.listing('photos', 'dupes/m1'))
    .toMatchObject({
      items: expect.not.arrayContaining([expect.objectContaining({ name: 'IMG_0300.jpg' })]),
    });
  await expect
    .poll(() => app.api.listing('archive', 'm1'))
    .toMatchObject({
      items: expect.arrayContaining([expect.objectContaining({ name: 'vacation_copy.jpg' })]),
    });
  await expect.poll(() => matchedNames(comparison)).toEqual([]);
  await expect(comparison.card('vacation_copy.jpg').element).toBeVisible();

  const planId = Number(new URL(executed.url()).pathname.split('/')[3]);
  const result = (await app.api.request<PlanDto>('GET', `/api/plans/${planId}`)).result;
  expect(result).toMatchObject({
    done: selection.item_count,
    skipped: 0,
    failed: 0,
    bytes: selection.total_size,
  });
});

async function matchedNames(pane: import('../../src/pages/pane').PanePage) {
  return pane.element.getByRole('option').evaluateAll((rows) =>
    rows
      .filter((row) => row.getAttribute('aria-label')?.includes(', matched'))
      .map((row) => row.getAttribute('aria-label')?.split(',')[0]?.trim() ?? '')
      .filter(Boolean)
      .sort(),
  );
}
