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

for (const selectionMode of ['source-only', 'both-panes'] as const) {
  test(`duplicate Move files hands off an explicit ${selectionMode} selection`, async ({
    app,
    page,
    source,
    comparison,
    duplicatesPage,
  }) => {
    const url = new URL('/duplicates', app.url);
    url.searchParams.set('src', 'photos/dupes/m1');
    url.searchParams.set('cmp', 'archive/m1');
    const initialPlan = page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
    );
    await page.goto(url.href);
    await initialPlan;
    await source.waitReady();
    await comparison.waitReady();

    await source.card('IMG_0300.jpg').select();
    if (selectionMode === 'both-panes') await comparison.card('vacation_copy.jpg').select();
    await expect(source.card('IMG_0300.jpg').isSelected()).resolves.toBe(true);
    if (selectionMode === 'both-panes')
      await expect(comparison.card('vacation_copy.jpg').isSelected()).resolves.toBe(true);

    await duplicatesPage.chooseAction('Move files');
    const selectionResponse = page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' &&
        new URL(response.url()).pathname === '/api/selections',
    );
    await duplicatesPage.actionButton().click();
    const selection = (await (await selectionResponse).json()) as SelectionDto;
    expect(selection.item_count).toBe(1);

    const movePlanResponse = page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
    );
    await expect(page).toHaveURL(/\/move(?:\?|$)/);
    await expect(
      page.getByText(`${selection.item_count} files from Duplicate finder`),
    ).toBeVisible();
    const movePlan = await movePlanResponse;
    const body = movePlan.request().postDataJSON() as {
      params?: { source?: { filter?: { selection_id?: number } } };
    };
    expect(body.params?.source?.filter?.selection_id).toBe(selection.id);
    await expect(page.getByRole('button', { name: 'Remove selection filter' })).toBeVisible();
  });
}

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
