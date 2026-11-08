import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { snapshotTree, writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';

test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('cleanup refuses a folder that gained a file after preview', async ({
  app,
  page,
  comparison,
  cleanupPage,
  confirmDialog,
}) => {
  const planResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  const url = new URL('/cleanup', app.url);
  url.searchParams.set('src', 'photos/empty');
  url.searchParams.set('cmp', 'photos/empty');
  await page.goto(url.href);
  const plan = (await (await planResponse).json()) as PlanDto;
  await expect(cleanupPage.actionButton()).toBeEnabled();

  await writeFile(app, 'photos', 'empty/nested/a/b/c/arrived-after-preview.txt', 'keep me');
  await cleanupPage.actionButton().click();
  await expect(confirmDialog.dialog()).toContainText('empty folders');
  const executeResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/execute$/.test(new URL(response.url()).pathname),
  );
  await confirmDialog.confirm(`Delete ${plan.stats.empty_folders} empty folders`);
  const task = (await (await executeResponse).json()) as { id: number };
  await expect
    .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task.id)?.state)
    .toBe('done');

  const executed = (await app.api.request<PlanDto>('GET', `/api/plans/${plan.id}`)).result;
  expect(executed?.skipped).toBeGreaterThan(0);
  expect(executed?.failures).toEqual(
    expect.arrayContaining([expect.objectContaining({ code: 'not_empty' })]),
  );
  const tree = await snapshotTree(app.volumesDir);
  expect(
    tree.some((entry) => entry.path === 'photos/empty/nested/a/b/c/arrived-after-preview.txt'),
  ).toBe(true);
  await expect.poll(() => comparison.folderState('nested')).toMatchObject({ red: false });
});
