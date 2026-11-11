import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { formatFileSize } from '../../../web/src/util/format';
import { diffTrees, snapshotTree } from '../../src/disk';
import { expect, test } from '../../src/test';
import { terminalTask } from '../../src/tool-oracles';

test.use({
  allowedErrors: [
    [
      /^requestfailed: .*\/api\/events net::ERR_ABORTED$/,
      /^requestfailed: .*\/api\/browse\/(?:folders|items) net::ERR_ABORTED$/,
    ],
    { scope: 'test' },
  ],
});

for (const selectionMode of [
  'source-only',
  'comparison-only',
  'both-panes-source-active',
  'both-panes-comparison-active',
] as const) {
  test(`duplicate Move files hands off an explicit ${selectionMode} selection`, async ({
    app,
    page,
    source,
    comparison,
    duplicatesPage,
    movePage,
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
    const before = await snapshotTree(app.volumesDir, { hash: true });
    const sourceFile = (await app.api.listing('photos', 'dupes/m1')).items.find(
      (item) => item.name === 'IMG_0300.jpg',
    );
    const comparisonFile = (await app.api.listing('archive', 'm1')).items.find(
      (item) => item.name === 'vacation_copy.jpg',
    );
    if (!sourceFile || !comparisonFile) throw new Error('Expected duplicate files are not indexed');

    if (selectionMode !== 'comparison-only') {
      await source.card('IMG_0300.jpg').select();
      await expect(source.card('IMG_0300.jpg').isSelected()).resolves.toBe(true);
    }
    if (selectionMode !== 'source-only') {
      await comparison.card('vacation_copy.jpg').select();
      await expect(comparison.card('vacation_copy.jpg').isSelected()).resolves.toBe(true);
    }
    await duplicatesPage.chooseAction('Move files');
    const active = selectionMode.endsWith('source-active') ? source : comparison;
    if (selectionMode.startsWith('both-panes')) {
      await active.breadcrumbs().getByRole('link', { name: 'm1', exact: true }).focus();
      await expect(active.element).toHaveAttribute('aria-current', 'true');
      await expect(source.card('IMG_0300.jpg').isSelected()).resolves.toBe(true);
      await expect(comparison.card('vacation_copy.jpg').isSelected()).resolves.toBe(true);
    }
    const expectedFile =
      selectionMode === 'source-only' || selectionMode === 'both-panes-source-active'
        ? sourceFile
        : comparisonFile;

    const selectionResponse = page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' &&
        new URL(response.url()).pathname === '/api/selections',
    );
    await duplicatesPage.actionButton().click();
    const selectionResult = await selectionResponse;
    expect(selectionResult.request().postDataJSON()).toMatchObject({ ids: [expectedFile.id] });
    const selection = (await selectionResult.json()) as SelectionDto;
    expect(selection.item_count).toBe(1);

    await expect(page).toHaveURL(/\/move(?:\?|$)/);
    await expect(
      page.getByText(`${selection.item_count} files from Duplicate finder`),
    ).toBeVisible();
    await expect(movePage.planNotices()).toContainText('Choose the destination folder');
    await expect(comparison.breadcrumbs()).toContainText('archive');
    await expect(comparison.breadcrumbs()).toContainText('m1');
    const movePlanResponse = page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
    );
    await comparison.useFolder();
    const movePlan = await movePlanResponse;
    const body = movePlan.request().postDataJSON() as {
      params?: { source?: { filter?: { selection_id?: number } } };
    };
    expect(body.params?.source?.filter?.selection_id).toBe(selection.id);
    await expect(page.getByRole('button', { name: 'Remove selection filter' })).toBeVisible();
    expect(await snapshotTree(app.volumesDir, { hash: true })).toEqual(before);
  });
}

test('duplicate Move files hands off comparison matches as a clearable restriction', async ({
  app,
  page,
  source,
  comparison,
  duplicatesPage,
  movePage,
  confirmDialog,
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
  const file = (await app.api.listing('archive', 'm1')).items.find(
    (item) => item.name === 'vacation_copy.jpg',
  );
  if (!file) throw new Error('Expected comparison file is not indexed');
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

  await expect(movePage.planNotices()).toContainText('Choose the destination folder');
  await comparison.breadcrumbs().getByRole('link', { name: 'Volumes' }).click();
  await comparison.openFolder('photos');
  const movePlanResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  await comparison.useFolder();
  const planned = await movePlanResponse;
  const params = planned.request().postDataJSON() as {
    params: { source: { filter: { selection_id: number | null } } };
  };
  expect(params.params.source.filter.selection_id).toBeNull();
  const movePlan = (await planned.json()) as PlanDto;
  await expect(source.card(file.name).element).toBeVisible();
  await source.card(file.name).select();
  await expect(movePage.destinationCard(file.name).element).toBeVisible();
  await expect(movePage.destinationCard(file.name).isGhost()).resolves.toBe(true);

  const moveSelectionResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === `/api/plans/${movePlan.id}/selection`,
  );
  await movePage.actionButton().click();
  const moveSelection = (await (await moveSelectionResponse).json()) as SelectionDto;
  expect(moveSelection.item_count).toBe(1);
  expect(moveSelection.total_size).toBe(file.size);
  await expect(confirmDialog.dialog()).toContainText('1 file');
  await expect(confirmDialog.dialog()).toContainText(formatFileSize(file.size));
  const executeResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === `/api/plans/${movePlan.id}/execute`,
  );
  await confirmDialog.confirm('Move 1 file');
  const task = (await (await executeResponse).json()) as { id: number };
  expect((await terminalTask(app, task.id)).state).toBe('done');
  const result = (await app.api.request<PlanDto>('GET', `/api/plans/${movePlan.id}`)).result;
  expect(result).toMatchObject({ done: 1, skipped: 0, failed: 0, bytes: file.size });
  const changes = diffTrees(before, await snapshotTree(app.volumesDir, { hash: true }));
  expect(changes.moved).toContainEqual({
    from: `archive/m1/${file.name}`,
    to: `photos/${file.name}`,
  });
  expect(
    changes.removed.filter((entry) => !changes.moved.some((move) => move.from === entry.path)),
  ).toEqual([]);
  expect(
    changes.added.filter(
      (entry) => entry.type === 'file' && !changes.moved.some((move) => move.to === entry.path),
    ),
  ).toEqual([]);
  expect(changes.changed).toEqual([]);
  await expect
    .poll(() => app.api.listing('photos', ''))
    .toMatchObject({
      items: expect.arrayContaining([expect.objectContaining({ name: file.name })]),
    });
  await expect
    .poll(() => app.api.listing('archive', 'm1'))
    .toMatchObject({
      items: expect.not.arrayContaining([expect.objectContaining({ name: file.name })]),
    });
});
