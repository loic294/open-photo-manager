import { readFile } from 'node:fs/promises';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { formatFileSize } from '../../../web/src/util/format';
import { emptyFilter } from '../../src/api';
import { diffTrees, snapshotTree, trashEntries, writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';
import { expectedDestination } from '../../src/tool-oracles';

// Navigating to the second formatting journey closes the server-sent events connection.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

function isResponse(path: RegExp) {
  return (response: import('@playwright/test').Response) =>
    response.request().method() === 'POST' && path.test(new URL(response.url()).pathname);
}

async function waitForTask(app: import('../../src/app-instance').AppInstance, id: number) {
  await expect
    .poll(async () => (await app.api.tasks()).find((task) => task.id === id)?.state)
    .toBe('done');
}

test('existing formatting folders show their final counts before any files move', async ({
  app,
  page,
  formatPage,
  confirmDialog,
}) => {
  const photo = await readFile(app.volumePath('photos/dupes/m1/IMG_0300.jpg'));
  const base = 'e2e-format-counts';
  await writeFile(app, 'photos', `${base}/from/a.jpg`, photo);
  await writeFile(app, 'photos', `${base}/existing/stay.jpg`, photo);
  const volume = (await app.api.volumes()).find((entry) => entry.name === 'photos');
  if (!volume) throw new Error('Missing photos volume');
  await app.rescan([{ volume_id: volume.id, dir: base, recursive: true }]);
  const request = { volume_id: volume.id, dir: base, filter: emptyFilter, plan: null };
  const current = await app.api.folders(request);
  expect(current.folders.map((folder) => [folder.name, folder.count])).toEqual([
    ['existing', 1],
    ['from', 1],
  ]);
  const before = await snapshotTree(app.volumesDir, { hash: true });
  const initialPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await page.goto(`${app.url}/format?src=photos/${base}&cmp=photos/${base}`);
  await initialPlan;
  await formatPage.edit();
  const updatedPlan = page.waitForResponse(
    (response) =>
      isResponse(/^\/api\/plans$/)(response) &&
      response.request().postDataJSON().params.pattern === 'existing',
  );
  await formatPage.pattern().fill('existing');
  expect((await updatedPlan).ok()).toBe(true);
  await formatPage.done();
  const count = (name: string) =>
    formatPage.destinationFolder(name).locator('.badge[aria-hidden="true"]');
  await expect(count('existing')).toHaveText('2');
  await expect(count('from')).toHaveText('0');
  expect(await snapshotTree(app.volumesDir, { hash: true })).toEqual(before);
  expect((await app.api.folders(request)).folders).toEqual(current.folders);

  await formatPage.actionButton().click();
  const executed = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/execute$/));
  await confirmDialog.confirm('Move and format 1 file');
  const task = (await (await executed).json()) as { id: number };
  await waitForTask(app, task.id);
  expect(
    (await app.api.folders(request)).folders.map((folder) => [folder.name, folder.count]),
  ).toEqual([
    ['existing', 2],
    ['from', 0],
  ]);
  expect((await app.api.listing('photos', `${base}/from`)).items).toHaveLength(0);
  expect(
    (await app.api.listing('photos', `${base}/existing`)).items.map((item) => item.name),
  ).toEqual(['a.jpg', 'stay.jpg']);
});

test('folder formatting moves to the pattern path shown in its preview', async ({
  app,
  page,
  manifest,
  source,
  formatPage,
  confirmDialog,
}) => {
  const file = manifest.file('photos', 'dupes/m1/IMG_0300.jpg');
  test.skip(file.skipped, file.skip_reason ?? 'fixture scenario was skipped');
  const pattern = '{YYYY}/{filename}';
  const url = new URL('/format', app.url);
  url.searchParams.set('src', 'photos/dupes/m1');
  url.searchParams.set('cmp', 'photos/dupes/m1');
  const initialPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  const initialPreview = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  await page.goto(url.href);
  await initialPlan;
  await initialPreview;

  for (let i = 0; i < 3; i += 1) {
    await formatPage
      .destinationPane()
      .getByRole('button', { name: 'Up one folder', exact: true })
      .click();
    await expect(formatPage.destinationPane().locator('[data-pane-body]')).not.toHaveAttribute(
      'aria-busy',
      'true',
    );
  }
  await formatPage.openDestinationFolder('archive');
  const basePlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await formatPage
    .destinationPane()
    .getByRole('button', { name: 'Use this folder', exact: true })
    .click();
  await basePlan;
  await formatPage.edit();

  const previewResponse = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  const patternPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await formatPage.pattern().fill(pattern);
  const preview = await previewResponse;
  expect(preview.ok()).toBe(true);
  await patternPlan;
  await formatPage.livePreview().locator('summary').click();
  await expect(formatPage.livePreview()).toContainText(expectedDestination(file, pattern));
  await formatPage.done();
  const fileName = file.path.split('/').at(-1) ?? file.path;
  await source.card(fileName).select();
  const expectedPath = `archive/${expectedDestination(file, pattern)}`;
  const destinationFolder = expectedDestination(file, pattern).split('/')[0];
  if (!destinationFolder) throw new Error('Expected a formatted destination folder');
  await expect
    .poll(() => formatPage.destinationFolder(destinationFolder).getAttribute('class'))
    .toContain('border-dashed');
  const before = await snapshotTree(app.volumesDir, { hash: true });

  const selectionResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/selection$/));
  await formatPage.actionButton().click();
  const selection = (await (await selectionResponse).json()) as SelectionDto;
  expect(selection.item_count).toBe(1);
  expect(selection.total_size).toBe(file.size);
  await expect(confirmDialog.dialog()).toContainText('Move and format 1 file?');
  await expect(confirmDialog.dialog()).toContainText(formatFileSize(file.size ?? 0));
  const executeResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/execute$/));
  await confirmDialog.confirm('Move and format 1 file');
  const executed = await executeResponse;
  const task = (await executed.json()) as { id: number };
  await waitForTask(app, task.id);

  const destinationDir = expectedDestination(file, pattern).split('/').slice(0, -1).join('/');
  const changes = diffTrees(before, await snapshotTree(app.volumesDir, { hash: true }));
  expect(changes.moved).toContainEqual({
    from: `photos/${file.path}`,
    to: expectedPath,
  });
  expect(
    changes.removed.filter((entry) => !changes.moved.some((move) => move.from === entry.path)),
  ).toEqual([]);
  expect(changes.changed).toEqual([]);
  expect(await trashEntries(app)).toHaveLength(0);
  const planId = Number(new URL(executed.url()).pathname.split('/')[3]);
  const result = (await app.api.request<PlanDto>('GET', `/api/plans/${planId}`)).result;
  expect(result).toMatchObject({ done: 1, skipped: 0, failed: 0, bytes: file.size });
  await expect
    .poll(() => app.api.listing('photos', 'dupes/m1'))
    .toMatchObject({
      items: expect.not.arrayContaining([expect.objectContaining({ name: fileName })]),
    });
  await expect
    .poll(() => app.api.listing('archive', destinationDir))
    .toMatchObject({
      items: expect.arrayContaining([expect.objectContaining({ name: fileName })]),
    });
  const secondUrl = new URL('/format', app.url);
  secondUrl.searchParams.set('src', `archive/${destinationDir}`);
  const secondInitialPreview = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  const secondInitialPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await page.goto(secondUrl.href);
  await secondInitialPlan;
  await secondInitialPreview;
  await formatPage.edit();
  const secondPreviewResponse = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  const secondPlanResponse = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await formatPage.pattern().fill('{filename}');
  await secondPreviewResponse;
  await secondPlanResponse;
  await formatPage.done();
  await expect(formatPage.actionButton()).toBeDisabled();
  await expect(page.getByText('Nothing to move', { exact: true }).first()).toBeVisible();
});
