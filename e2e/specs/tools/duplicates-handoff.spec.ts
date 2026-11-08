import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { snapshotTree } from '../../src/disk';
import { expect, test } from '../../src/test';

test.use({
  allowedErrors: [
    [
      /^requestfailed: .*\/api\/events net::ERR_ABORTED$/,
      /^requestfailed: .*\/api\/browse\/(?:folders|items) net::ERR_ABORTED$/,
    ],
    { scope: 'test' },
  ],
});

test('duplicate Move files hands off comparison matches as a clearable restriction', async ({
  app,
  page,
  duplicatesPage,
}) => {
  const url = new URL('/duplicates', app.url);
  url.searchParams.set('src', 'photos/dupes/m1');
  url.searchParams.set('cmp', 'archive/m1');
  const planResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  await page.goto(url.href);
  const plan = (await (await planResponse).json()) as PlanDto;
  expect(plan.stats.comparison_matched).toBeGreaterThan(0);
  const before = await snapshotTree(app.volumesDir, { hash: true });

  await duplicatesPage.chooseAction('Move files');
  const selectionResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === `/api/plans/${plan.id}/selection`,
  );
  await duplicatesPage.actionButton().click();
  const selection = (await (await selectionResponse).json()) as SelectionDto;
  await expect(page).toHaveURL(/\/move(?:\?|$)/);
  await expect(page.getByText(`${selection.item_count} files from Duplicate finder`)).toBeVisible();
  await expect(page.getByRole('button', { name: 'Remove selection filter' })).toBeVisible();
  expect(await snapshotTree(app.volumesDir, { hash: true })).toEqual(before);

  await page.getByRole('button', { name: 'Remove selection filter' }).click();
  await expect(page.getByRole('button', { name: 'Remove selection filter' })).toHaveCount(0);
  expect(await snapshotTree(app.volumesDir, { hash: true })).toEqual(before);
});
