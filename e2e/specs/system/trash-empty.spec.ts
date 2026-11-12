import { basename } from 'node:path';
import { emptyFilter } from '../../src/api';
import { snapshotTree, trashEntries } from '../../src/disk';
import { expect, test } from '../../src/test';
import { taskAction, trashFixtures, trashList } from '../../src/tool-oracles';

test('empty-trash confirmation names the visible scope; cancellation preserves every byte @smoke', async ({
  app,
  manifest,
  page,
  trashPage,
  confirmDialog,
}) => {
  test.setTimeout(90_000);
  const files = manifest.filesOf('deep.grid_500').slice(0, 3);
  await trashFixtures(app, files);
  await page.goto(`${app.url}/trash`);
  await trashPage.tab('Items');
  await expect(trashPage.rows()).toHaveCount(3);
  const before = await snapshotTree(app.volumesDir, { hash: true });
  const listing = await trashList(app);
  await trashPage.empty();
  await expect(confirmDialog.dialog()).toContainText('Permanently delete 3 items');
  await expect(confirmDialog.dialog()).toContainText('from all volumes? This cannot be undone.');
  await expect(
    confirmDialog.dialog().getByRole('button', { name: 'Empty trash', exact: true }),
  ).toHaveClass(/btn-error/);
  await confirmDialog.cancel();
  await expect(confirmDialog.dialog()).not.toBeVisible();
  expect(await snapshotTree(app.volumesDir, { hash: true })).toEqual(before);
  expect(await trashList(app)).toEqual(listing);
  await trashPage.empty();
  expect(
    (await taskAction(app, page, '/api/trash/empty', () => confirmDialog.confirm('Empty trash')))
      .state,
  ).toBe('done');
  await expect(page.locator('opm-trash-items')).toContainText('The trash is empty');
  expect((await trashList(app)).summary.items).toBe(0);
  expect(await trashEntries(app)).toEqual([]);
  const after = await snapshotTree(app.volumesDir, { hash: true });
  expect(after.filter((entry) => !entry.path.includes('/.opm-trash'))).toEqual(
    before.filter((entry) => !entry.path.includes('/.opm-trash')),
  );
});

test('empty uses the dialog snapshot up_to_id; a later real deletion survives', async ({
  app,
  manifest,
  page,
  trashPage,
  confirmDialog,
}) => {
  test.setTimeout(90_000);
  const files = manifest.filesOf('deep.grid_500').slice(0, 2);
  await trashFixtures(app, files.slice(0, 1));
  await page.goto(`${app.url}/trash`);
  await trashPage.tab('Items');
  await expect(trashPage.rows()).toHaveCount(1);
  const first = await trashList(app);
  await trashPage.empty();
  await expect(confirmDialog.dialog()).toContainText('Permanently delete 1 item');
  await trashFixtures(app, files.slice(1));
  const later = (await trashList(app)).items.find((item) => item.id > first.summary.max_id);
  if (!later) throw new Error('Later deletion did not receive a new trash id');
  const submission = page.waitForRequest(
    (request) =>
      new URL(request.url()).pathname === '/api/trash/empty' && request.method() === 'POST',
  );
  expect(
    (await taskAction(app, page, '/api/trash/empty', () => confirmDialog.confirm('Empty trash')))
      .state,
  ).toBe('done');
  expect((await submission).postDataJSON()).toEqual({
    volume_id: null,
    up_to_id: first.summary.max_id,
  });
  await expect(trashPage.rows()).toHaveCount(1);
  await expect(trashPage.rows()).toContainText(basename(files[1].path));
  expect((await trashList(app)).items.map((item) => item.id)).toEqual([later.id]);
  expect(
    (await trashEntries(app)).some((entry) => entry.path.endsWith(`/grid/${later.name}`)),
  ).toBe(true);
});

test('volume-filtered empty cannot destroy another volume; trash is hidden from every browse surface', async ({
  app,
  manifest,
  page,
  shell,
  trashPage,
  confirmDialog,
  source,
  comparison,
}) => {
  test.setTimeout(120_000);
  const photos = manifest.filesOf('deep.grid_500').slice(0, 1);
  const archive = manifest.data.files
    .filter(
      (file) =>
        file.volume === 'archive' && file.kind === 'image' && !file.skipped && file.indexable,
    )
    .slice(0, 1);
  expect(archive).toHaveLength(1);
  await trashFixtures(app, photos);
  await trashFixtures(app, archive);
  const summary = (await trashList(app)).summary;
  await page.goto(`${app.url}/trash`);
  await trashPage.tab('Items');
  await expect(trashPage.rows()).toHaveCount(summary.items);
  const photoVolume = summary.volumes.find((volume) => volume.name === 'photos');
  if (!photoVolume) throw new Error('Photos trash volume missing');
  await trashPage.volume(photoVolume.volume_id);
  await expect(trashPage.rows()).toHaveCount(photoVolume.items);
  await trashPage.empty();
  await expect(confirmDialog.dialog()).toContainText(
    `Permanently delete ${photoVolume.items} item`,
  );
  await expect(confirmDialog.dialog()).toContainText('from photos');
  expect(
    (
      await taskAction(app, page, '/api/trash/empty', () =>
        confirmDialog.confirm('Empty trash on photos'),
      )
    ).state,
  ).toBe('done');
  expect((await trashList(app)).items.every((item) => item.volume === 'archive')).toBe(true);
  expect((await trashList(app)).summary.items).toBe(summary.items - photoVolume.items);
  for (const route of [
    'Browse',
    'Asset move',
    'Folder formatting',
    'Duplicate finder',
    'Empty folder cleanup',
    'Batch rename',
  ]) {
    await shell.navigate(route);
    await expect(page.getByRole('button', { name: /delete permanently/i })).toHaveCount(0);
    await expect(page.getByText('.opm-trash', { exact: true })).toHaveCount(0);
  }
  await shell.navigate('Browse');
  if (await source.breadcrumbs().getByRole('button', { name: 'Volumes', exact: true }).count())
    await source.breadcrumbs().getByRole('button', { name: 'Volumes', exact: true }).click();
  if (await comparison.breadcrumbs().getByRole('button', { name: 'Volumes', exact: true }).count())
    await comparison.breadcrumbs().getByRole('button', { name: 'Volumes', exact: true }).click();
  await source.openFolder('archive');
  await comparison.openFolder('photos');
  await expect(source.folder('.opm-trash')).toHaveCount(0);
  const volumes = await app.api.volumes();
  for (const volume of volumes) {
    const folders = await app.api.request<{ folders: { name: string }[] }>(
      'POST',
      '/api/browse/folders',
      {
        volume_id: volume.id,
        dir: '',
        filter: emptyFilter,
        plan: null,
      },
    );
    expect(folders.folders.some((folder) => folder.name === '.opm-trash')).toBe(false);
  }
});
