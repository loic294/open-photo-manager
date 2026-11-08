import { readFile, rm } from 'node:fs/promises';
import { writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';
import { indexedFile, taskAction, trashFixtures, trashList } from '../../src/tool-oracles';
import { expectStaysFalse } from '../../src/waits';

test('sync adopts timestamped untracked bytes, removes missing rows and restores an adopted photo', async ({
  app,
  manifest,
  page,
  trashPage,
}) => {
  test.setTimeout(90_000);
  const template = manifest.filesOf('exif.full_nikon')[0];
  const bytes = await readFile(app.volumePath(`${template.volume}/${template.path}`));
  await writeFile(app, 'photos', '.apm-trash/20200102-030405/recovered/adopted.jpg', bytes);
  await writeFile(app, 'photos', '.apm-trash/unknown-batch/recovered/missing.jpg', bytes);
  await page.goto(`${app.url}/trash`);
  const before = Math.floor(Date.now() / 1000);
  expect((await taskAction(app, page, '/api/trash/sync', () => trashPage.sync())).state).toBe(
    'done',
  );
  await trashPage.tab('Items');
  await expect(trashPage.rows()).toHaveCount(2);
  await expect(trashPage.row('adopted.jpg')).toContainText('Found on disk');
  const items = (await trashList(app)).items;
  const adopted = items.find((item) => item.name === 'adopted.jpg');
  const missing = items.find((item) => item.name === 'missing.jpg');
  expect(adopted).toMatchObject({
    trashed_at: Date.parse('2020-01-02T03:04:05Z') / 1000,
    has_snapshot: false,
    reason: null,
  });
  expect(missing?.trashed_at).toBeGreaterThanOrEqual(before);
  await rm(app.volumePath('photos/.apm-trash/unknown-batch/recovered/missing.jpg'));
  expect((await taskAction(app, page, '/api/trash/sync', () => trashPage.sync())).state).toBe(
    'done',
  );
  await expect(trashPage.rows()).toHaveCount(1);
  expect((await trashList(app)).summary.items).toBe(1);
  expect(
    (
      await taskAction(app, page, '/api/trash/restore', () =>
        trashPage.row('adopted.jpg').getByRole('button', { name: 'Restore', exact: true }).click(),
      )
    ).state,
  ).toBe('done');
  await expect(trashPage.rows()).toHaveCount(0);
  expect(await readFile(app.volumePath('photos/recovered/adopted.jpg'))).toEqual(bytes);
  await expect
    .poll(async () => (await indexedFile(app, 'photos', 'recovered/adopted.jpg')).hash)
    .toBe(template.sha1);
});

test('changing retention immediately sweeps only old trash; turning it off retains later old items @slow', async ({
  app,
  manifest,
  page,
  shell,
  settingsPage,
  trashPage,
}) => {
  test.setTimeout(120_000);
  await trashFixtures(app, manifest.filesOf('deep.grid_500').slice(0, 1));
  await writeFile(
    app,
    'photos',
    '.apm-trash/20000101-000000/old/due.txt',
    'old private retained bytes',
  );
  await page.goto(`${app.url}/trash`);
  expect((await taskAction(app, page, '/api/trash/sync', () => trashPage.sync())).state).toBe(
    'done',
  );
  expect((await trashList(app)).summary.items).toBe(2);
  await shell.navigate('Settings');
  await settingsPage.toggle('Delete trashed files automatically', true);
  await settingsPage.fill('Delete after', '1');
  await settingsPage.saveGeneral();
  await expect
    .poll(
      async () =>
        (await app.api.tasks()).find((task) => task.title === 'Removing trash older than 1 day')
          ?.state,
      { timeout: 30_000 },
    )
    .toBe('done');
  expect((await trashList(app)).items.map((item) => item.name)).not.toContain('due.txt');
  expect((await trashList(app)).summary.items).toBe(1);
  await shell.navigate('Trash');
  await expect(page.locator('apm-trash-page')).toContainText(
    'Items are deleted automatically 1 day',
  );
  await shell.navigate('Settings');
  await settingsPage.toggle('Delete trashed files automatically', false);
  await settingsPage.saveGeneral();
  await writeFile(
    app,
    'photos',
    '.apm-trash/20000102-000000/old/kept.txt',
    'must stay while retention disabled',
  );
  await shell.navigate('Trash');
  expect((await taskAction(app, page, '/api/trash/sync', () => trashPage.sync())).state).toBe(
    'done',
  );
  await expect(trashPage.row('kept.txt')).toBeVisible();
  await expect(page.locator('apm-trash-page')).toContainText(
    'Items stay in the trash until you empty it',
  );
  // No event is scheduled while retention is disabled; check a bounded absence window after sync.
  await expectStaysFalse(
    async () => !(await trashList(app)).items.some((item) => item.name === 'kept.txt'),
    { forMs: 5_000 },
  );
});
