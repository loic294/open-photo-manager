import { readFile, rename, rm } from 'node:fs/promises';
import { basename, dirname } from 'node:path';
import { writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';
import { executePlan, indexedFile, movePlan, terminalTask } from '../../src/tool-oracles';
import { expectStaysFalse } from '../../src/waits';

test.use({
  appOptions: {
    settings: (settings) => ({ ...settings, index: { ...settings.index, watch_enabled: true } }),
  },
});

test.beforeEach(async ({ app }) => {
  await expect
    .poll(
      async () =>
        ['watching', 'degraded'].includes((await app.api.indexStatus()).watcher?.state ?? ''),
      { timeout: 30_000 },
    )
    .toBe(true);
  test.skip(
    (await app.api.indexStatus()).watcher?.state === 'degraded',
    'Native watcher exhausted the host watch budget; runtime watching not covered',
  );
});

test('native create, sidecar edit, delete and subtree rename converge without re-index @slow', async ({
  app,
  manifest,
  page,
  source,
  filterBar,
}) => {
  test.slow();
  test.setTimeout(180_000);
  const status = (await app.api.indexStatus()).watcher;
  test.skip(status?.state === 'degraded', 'Native watcher exhausted the host watch budget');
  expect(status?.unwatched_dirs).toBe(0);
  const template = manifest.filesOf('stale.rename')[0];
  const bytes = await readFile(app.volumePath(`${template.volume}/${template.path}`));
  await page.goto(`${app.url}/browse?src=photos/stale&cmp=photos/stale`);
  await writeFile(app, 'photos', 'stale/watched.jpg', bytes);
  await expect(source.card('watched.jpg').element).toBeVisible({ timeout: 30_000 });
  expect((await indexedFile(app, 'photos', 'stale/watched.jpg')).hash).toBe(template.sha1);
  const owner = manifest.filesOf('stale.touch_xmp').find((file) => file.kind !== 'sidecar');
  const sidecar = manifest.filesOf('stale.touch_xmp').find((file) => file.kind === 'sidecar');
  if (!owner || !sidecar) throw new Error('XMP watcher fixtures missing');
  const xmp = await readFile(app.volumePath(`photos/${sidecar.path}`), 'utf8');
  await writeFile(
    app,
    'photos',
    sidecar.path,
    xmp.replace(/Rating="2"/, 'Rating="4"').replace(/<xmp:Rating>2</, '<xmp:Rating>4<'),
  );
  await expect
    .poll(async () => (await indexedFile(app, 'photos', owner.path)).item.rating, {
      timeout: 30_000,
    })
    .toBe(4);
  await filterBar.rating(4);
  await expect(source.card(basename(owner.path)).element).toBeVisible();
  await filterBar.clear();
  await rm(app.volumePath('photos/stale/watched.jpg'));
  await expect(source.card('watched.jpg').element).toHaveCount(0, { timeout: 30_000 });
  const baseline = (await app.api.tasks()).map((task) => task.id);
  for (let i = 0; i < 20; i++)
    await writeFile(app, 'photos', `stale/watched-tree/photo-${i}.jpg`, bytes);
  await expect(source.folder('watched-tree')).toBeVisible({ timeout: 30_000 });
  await expect
    .poll(async () => (await app.api.listing('photos', 'stale/watched-tree')).total, {
      timeout: 30_000,
    })
    .toBe(20);
  const batches = (await app.api.tasks()).filter(
    (task) => !baseline.includes(task.id) && task.kind === 'index',
  );
  expect(batches.length).toBeLessThanOrEqual(2);
  await rename(
    app.volumePath('photos/stale/watched-tree'),
    app.volumePath('photos/stale/renamed-tree'),
  );
  await writeFile(app, 'photos', 'stale/renamed-tree/after-rename.jpg', bytes);
  await expect(source.folder('renamed-tree')).toBeVisible({ timeout: 30_000 });
  await expect(source.folder('watched-tree')).toHaveCount(0);
  await expect
    .poll(
      async () =>
        (await app.api.listing('photos', 'stale/renamed-tree')).items.some(
          (item) => item.name === 'after-rename.jpg',
        ),
      { timeout: 30_000 },
    )
    .toBe(true);
  await rm(app.volumePath('photos/stale/renamed-tree'), { recursive: true });
  await expect(source.folder('renamed-tree')).toHaveCount(0, { timeout: 30_000 });
  expect(
    app.sql("SELECT count(*) AS n FROM files WHERE dir_path LIKE 'stale/renamed-tree%'")[0]?.n,
  ).toBe(0);
});

test('junk, trash and product moves do not schedule redundant watcher scans @slow', async ({
  app,
  manifest,
  page,
  source,
}) => {
  test.slow();
  const fixture = manifest.filesOf('dup.unique_controls').find((file) => file.volume === 'photos');
  if (!fixture) throw new Error('Move control missing');
  await page.goto(
    `${app.url}/browse?src=photos/${dirname(fixture.path)}&cmp=photos/${dirname(fixture.path)}`,
  );
  const plan = await movePlan(
    app,
    { volume: 'photos', dir: dirname(fixture.path) },
    { volume: 'archive', dir: 'watcher-move' },
  );
  const task = await executePlan(app, plan);
  expect((await terminalTask(app, task.id)).state).toBe('done');
  await expect(source.card(basename(fixture.path)).element).toHaveCount(0);
  expect((await indexedFile(app, 'archive', `watcher-move/${basename(fixture.path)}`)).hash).toBe(
    fixture.sha1,
  );
  await writeFile(app, 'photos', '.opm-trash/watch-junk/test.jpg', 'ignored trash');
  await writeFile(app, 'photos', '@eaDir/watch-junk/test.jpg', 'ignored thumbnails');
  const after = Math.max(...(await app.api.tasks()).map((task) => task.id));
  // Neither ignored writes nor a self-write emits an accepted watcher job; cover its 5 s quiet window.
  await expectStaysFalse(
    async () => (await app.api.tasks()).some((task) => task.id > after && task.kind === 'index'),
    { forMs: 7_000 },
  );
});

test('watch settings disable and re-enable registration without restarting @slow', async ({
  app,
  filterBar,
  page,
  shell,
  settingsPage,
  source,
}) => {
  test.slow();
  await page.goto(`${app.url}/browse?src=photos/stale&cmp=photos/stale`);
  await shell.navigate('Settings');
  await settingsPage.toggle('Watch for changes', false);
  await settingsPage.saveGeneral();
  await expect.poll(async () => (await app.api.indexStatus()).watcher?.state).toBe('disabled');
  await writeFile(app, 'photos', 'stale/disabled-watch.txt', 'manual only');
  // Disabled registration has no success event to wait for; cover a native quiet window.
  await expectStaysFalse(
    async () =>
      (await app.api.listing('photos', 'stale')).items.some(
        (item) => item.name === 'disabled-watch.txt',
      ),
    { forMs: 6_000 },
  );
  await shell.navigate('Browse');
  // Finish initial facets before index completion intentionally supersedes an in-flight request.
  await filterBar.waitForCountedFacets();
  await shell.reindex();
  await expect(source.card('disabled-watch.txt').element).toBeVisible({ timeout: 30_000 });
  await shell.navigate('Settings');
  await settingsPage.toggle('Watch for changes', true);
  await settingsPage.saveGeneral();
  await expect.poll(async () => (await app.api.indexStatus()).watcher?.state).toBe('watching');
  await shell.navigate('Browse');
  await writeFile(app, 'photos', 'stale/enabled-watch.txt', 'native again');
  await expect(source.card('enabled-watch.txt').element).toBeVisible({ timeout: 30_000 });
});
