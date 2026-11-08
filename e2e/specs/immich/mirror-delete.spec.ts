import { readFile, stat } from 'node:fs/promises';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { trashEntries } from '../../src/disk';
import { connectThroughUi, mapAndSync, waitForTask } from '../../src/immich';
import { expect, test } from '../../src/immich-test';

test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('@smoke Duplicate finder trashes the file before removing its Immich asset', async ({
  backedImmich: { app, database, mock, expectRemovedFiles },
  page,
  shell,
  settingsPage,
  comparison,
  duplicatesPage,
  confirmDialog,
}) => {
  test.setTimeout(240_000);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app);
  const asset = mock.state.assetByPath('/mnt/archive/m1/vacation_copy.jpg');
  if (!asset) throw new Error('Expected scanned Immich fixture asset');
  const id = String(asset.id);
  const bytes = await readFile(app.volumePath('archive/m1/vacation_copy.jpg'));
  const sourceBytes = await readFile(app.volumePath('photos/dupes/m1/IMG_0300.jpg'));
  const url = new URL('/duplicates', app.url);
  url.searchParams.set('src', 'photos/dupes/m1');
  url.searchParams.set('cmp', 'archive/m1');
  const planned = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
  );
  await page.goto(url.href);
  const plan = (await (await planned).json()) as PlanDto;
  expect(plan.stats.comparison_matched).toBeGreaterThan(0);
  const card = comparison.card('vacation_copy.jpg');
  await expect(card.element).toHaveAttribute('aria-label', /In Immich/);
  await card.select();
  await expect(duplicatesPage.actionButton()).toContainText('Delete comparison items');
  const selection = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/selection$/.test(new URL(response.url()).pathname),
  );
  await duplicatesPage.actionButton().click();
  expect(((await (await selection).json()) as SelectionDto).item_count).toBe(1);
  const executing = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/execute$/.test(new URL(response.url()).pathname),
  );
  expectRemovedFiles(['archive/m1/vacation_copy.jpg']);
  await confirmDialog.confirm('Delete 1 file');
  const task = (await (await executing).json()) as { id: number };
  await waitForTask(app, task.id);
  expect((await app.api.request<PlanDto>('GET', `/api/plans/${plan.id}`)).result).toMatchObject({
    done: 1,
    skipped: 0,
    failed: 0,
  });
  await expect(stat(app.volumePath('archive/m1/vacation_copy.jpg'))).rejects.toMatchObject({
    code: 'ENOENT',
  });
  const entries = await trashEntries(app);
  const trashed = entries.find((entry) => entry.path.endsWith('/m1/vacation_copy.jpg'));
  if (!trashed) throw new Error('Deleted comparison file was not found in application trash');
  expect(await readFile(app.volumePath(trashed.path))).toEqual(bytes);
  await expect
    .poll(() => database.sql(`SELECT count(*) FROM public.asset WHERE id = '${id}'::uuid`), {
      timeout: 180_000,
    })
    .toBe('0');
  expect(await readFile(app.volumePath(trashed.path))).toEqual(bytes);
  expect(await readFile(app.volumePath('photos/dupes/m1/IMG_0300.jpg'))).toEqual(sourceBytes);
  expect(mock.state.assetByPath('/mnt/archive/m1/vacation_copy.jpg')).toBeUndefined();
  const deletes = mock.state
    .requests()
    .entries.filter((entry) => entry.method === 'DELETE' && entry.path === '/api/assets');
  expect(deletes).toHaveLength(1);
  expect(deletes[0]?.bodySummary).toMatchObject({ ids: [id], force: true });
  expect(mock.state.violations()).toEqual([]);
});
