import { chmod, readFile, rename } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { snapshotTree, writeFile } from '../../src/disk';
import { captureScreens } from '../../src/screens';
import { expect, test } from '../../src/test';
import {
  indexedFile,
  taskAction,
  trashBatches,
  trashFixtures,
  trashList,
} from '../../src/tool-oracles';

test.use({
  // Full page navigation closes the previous page's event stream.
  allowedErrors: [[/requestfailed: .*\/api\/events net::ERR_ABORTED/], { scope: 'test' }],
});

test('a deletion exposes its exact batch; restoring all recovers original bytes and index @smoke', async ({
  app,
  manifest,
  page,
  trashPage,
  confirmDialog,
  source,
}) => {
  test.setTimeout(90_000);
  const files = manifest.filesOf('deep.grid_500').slice(0, 3);
  const originals = await Promise.all(
    files.map((file) => readFile(app.volumePath(`${file.volume}/${file.path}`))),
  );
  const deletion = await trashFixtures(app, files);
  const batches = await trashBatches(app);
  expect(batches.batches).toHaveLength(1);
  const batch = batches.batches[0];
  expect(batch.task_id).toBe(deletion.id);
  expect(batch.items).toBe(files.length);
  expect(batch.bytes).toBe(originals.reduce((total, bytes) => total + bytes.length, 0));
  await page.goto(`${app.url}/trash`);
  await trashPage.tab('Deletions');
  await expect(trashPage.batches()).toHaveCount(1);
  await expect(trashPage.batches().first()).toContainText(batch.volume);
  // TrashBatch.reasons is caller-provided free text, not the table's "Deleted" heading.
  for (const reason of batch.reasons)
    await expect(trashPage.batches().first()).toContainText(reason);
  await trashPage.batches().getByRole('button', { name: 'View items', exact: true }).click();
  await expect(page.getByRole('tab', { name: 'Items', exact: true })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  for (const file of files) await expect(trashPage.row(basename(file.path))).toBeVisible();
  await trashPage.tab('Deletions');
  await trashPage.batches().getByRole('button', { name: 'Restore all', exact: true }).click();
  await expect(confirmDialog.dialog()).toContainText(`Restore ${files.length} items?`);
  await expect(confirmDialog.dialog()).toContainText(
    'go back to their original folders on volume photos',
  );
  await expect(confirmDialog.dialog()).toContainText('gets a number');
  expect(
    (
      await taskAction(app, page, '/api/trash/restore', () =>
        confirmDialog.confirm(`Restore ${files.length} items`),
      )
    ).state,
  ).toBe('done');
  await expect(trashPage.batches()).toHaveCount(0);
  expect((await trashList(app)).summary.items).toBe(0);
  for (const [index, file] of files.entries()) {
    expect(await readFile(app.volumePath(`${file.volume}/${file.path}`))).toEqual(originals[index]);
    expect((await indexedFile(app, file.volume, file.path)).hash).toBe(file.sha1);
  }
  await page.goto(`${app.url}/browse?src=photos/grid&cmp=photos/grid`);
  for (const file of files) await expect(source.card(basename(file.path)).element).toBeVisible();
});

test('selection and single-item restores preserve unrelated trash; name collisions are numbered', async ({
  app,
  manifest,
  page,
  trashPage,
}) => {
  test.setTimeout(90_000);
  const files = manifest.filesOf('deep.grid_500').slice(0, 4);
  await trashFixtures(app, files);
  await page.goto(`${app.url}/trash`);
  await trashPage.tab('Items');
  for (const file of files.slice(0, 2))
    await trashPage.row(basename(file.path)).getByRole('checkbox').check();
  await expect(trashPage.selectedRestore()).toHaveText('Restore selected (2)');
  expect((await taskAction(app, page, '/api/trash/restore', () => trashPage.restore())).state).toBe(
    'done',
  );
  await expect(trashPage.rows()).toHaveCount(2);
  expect((await trashList(app)).total).toBe(2);
  const collision = files[2];
  await writeFile(app, collision.volume, collision.path, 'new content must not be overwritten');
  await app.rescan();
  expect(
    (
      await taskAction(app, page, '/api/trash/restore', () =>
        trashPage
          .row(basename(collision.path))
          .getByRole('button', { name: 'Restore', exact: true })
          .click(),
      )
    ).state,
  ).toBe('done');
  await expect(trashPage.rows()).toHaveCount(1);
  expect(await readFile(app.volumePath(`${collision.volume}/${collision.path}`), 'utf8')).toBe(
    'new content must not be overwritten',
  );
  const tree = await snapshotTree(app.volumesDir, { hash: true });
  const recovered = tree.filter(
    (entry) => entry.path.startsWith('photos/grid/') && entry.hash === collision.sha1,
  );
  expect(recovered).toHaveLength(1);
  expect(recovered[0].path).not.toBe(`photos/${collision.path}`);
  expect((await indexedFile(app, 'photos', recovered[0].path.slice('photos/'.length))).hash).toBe(
    collision.sha1,
  );
  expect((await trashList(app)).items[0].name).toBe(basename(files[3].path));
});

for (const state of ['offline', 'read-only'] as const) {
  test(`trash on a ${state} volume cannot be restored and explains why`, async ({
    app,
    manifest,
    page,
    trashPage,
  }) => {
    test.setTimeout(90_000);
    test.skip(
      state === 'read-only' && process.getuid?.() === 0,
      'Root bypasses Unix write permission checks',
    );
    await trashFixtures(app, manifest.filesOf('deep.grid_500').slice(0, 1));
    const volume = app.volumePath('photos');
    const held = join(app.directory, 'system-offline-photos');
    if (state === 'offline') await rename(volume, held);
    else await chmod(volume, 0o555);
    try {
      await app.rescan();
      await page.goto(`${app.url}/trash`);
      await trashPage.tab('Items');
      await expect(trashPage.rows()).toHaveCount(1);
      const restore = trashPage.rows().getByRole('button', { name: 'Restore', exact: true });
      await expect(restore).toBeDisabled();
      await expect(restore).toHaveAttribute('title', new RegExp(`photos is ${state}`));
      await expect(trashPage.rows().getByRole('checkbox')).toBeDisabled();
    } finally {
      if (state === 'offline') await rename(held, volume);
      else await chmod(volume, 0o755);
    }
  });
}

test('250 real deletions paginate, sort with the keyboard, search and filter by volume @slow', async ({
  app,
  manifest,
  page,
  shell,
  trashPage,
}) => {
  test.setTimeout(180_000);
  const files = manifest.filesOf('deep.grid_500').slice(0, 250);
  await trashFixtures(app, files);
  await page.goto(`${app.url}/trash`);
  await trashPage.tab('Items');
  await expect(trashPage.rows()).toHaveCount(50);
  await expect(page.locator('opm-trash-items')).toContainText('of 250');
  for (const [label, sort] of [
    ['Name', 'name'],
    ['Original folder', 'path'],
    ['Size', 'size'],
    ['Deleted', 'trashed_at'],
  ] as const) {
    const control = page
      .getByRole('columnheader')
      .getByRole('button', { name: label, exact: true });
    await control.focus();
    const response = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname === '/api/trash' &&
        new URL(response.url()).searchParams.get('sort') === sort,
    );
    await page.keyboard.press('Enter');
    const listing = await (await response).json();
    await expect
      .poll(() =>
        trashPage
          .rows()
          .getByRole('checkbox')
          .evaluateAll((nodes) =>
            nodes.map((node) => node.getAttribute('aria-label')?.replace(/^Select /, '')),
          ),
      )
      .toEqual(listing.items.map((item: { name: string }) => item.name));
    expect(listing.total).toBe(250);
  }
  await trashPage.sort('Name');
  await expect
    .poll(() =>
      trashPage
        .rows()
        .getByRole('checkbox')
        .evaluateAll((nodes) =>
          nodes.map((node) => node.getAttribute('aria-label')?.replace(/^Select /, '')),
        ),
    )
    .toEqual(
      (await trashList(app, 'sort=name&order=asc')).items.slice(0, 50).map((item) => item.name),
    );
  const seen = new Set<string>();
  for (let offset = 0; offset < 250; offset += 50) {
    const names = await trashPage
      .rows()
      .getByRole('checkbox')
      .evaluateAll((nodes) =>
        nodes.map((node) => node.getAttribute('aria-label')?.replace(/^Select /, '') ?? ''),
      );
    for (const name of names) seen.add(name);
    if (offset < 200) {
      const next = page.waitForResponse(
        (response) =>
          new URL(response.url()).pathname === '/api/trash' &&
          new URL(response.url()).searchParams.get('offset') === String(offset + 50),
      );
      await trashPage.next();
      const nextItems = await (await next).json();
      await expect
        .poll(() =>
          trashPage
            .rows()
            .getByRole('checkbox')
            .evaluateAll((nodes) =>
              nodes.map((node) => node.getAttribute('aria-label')?.replace(/^Select /, '')),
            ),
        )
        .toEqual(nextItems.items.map((item: { name: string }) => item.name));
      await expect(page.locator('opm-trash-items')).toContainText(`${offset + 51}`);
    }
  }
  expect(seen.size).toBe(250);
  await trashPage.search('GRID_0001');
  await expect(trashPage.rows()).toHaveCount(1);
  await expect(trashPage.rows()).toContainText('GRID_0001.jpg');
  expect((await trashList(app, 'q=GRID_0001')).total).toBe(1);
  await trashPage.search('');
  const photos = (await app.api.volumes()).find((volume) => volume.name === 'photos');
  if (!photos) throw new Error('Photos fixture volume missing');
  await trashPage.volume(photos.id);
  await expect(trashPage.rows()).toHaveCount(50);
  expect((await trashList(app, `volume_id=${photos.id}`)).total).toBe(250);
  for (const theme of ['light', 'dark'] as const) {
    await shell.theme(theme);
    await expectNoSeriousA11yViolations(page, { include: ['opm-trash-page'] });
  }
  await captureScreens(page, 'system-trash-populated');
});
