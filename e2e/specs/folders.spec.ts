import { browseUrl, fixture } from '../src/scenarios';
import { expect, test } from '../src/test';

// A full reload/history navigation intentionally closes the old EventSource.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('volume root lists both mounted shares @smoke', async ({
  page,
  sharedApp,
  source,
  comparison,
}) => {
  await page.goto(sharedApp.url);
  await expect.poll(() => source.folderNames()).toEqual(['archive', 'photos']);
  expect(await comparison.folderNames()).toEqual(['archive', 'photos']);
  await expect(source.folder('photos')).toContainText('items');
});

test.describe('full history navigation', () => {
  // Leaving/reloading media cancels still-loading thumbnails as well as the old EventSource.
  test.use({
    allowedErrors: [
      /^requestfailed: .*\/api\/events net::ERR_ABORTED$/,
      /^requestfailed: http:\/\/127\.0\.0\.1:\d+\/api\/files\/\d+\/thumb\?size=(sm|lg)&v=\d+ net::ERR_ABORTED$/,
    ],
  });
  test('folder navigation is independent and survives history and reload', async ({
    page,
    sharedApp,
    source,
    comparison,
  }) => {
    await page.goto(sharedApp.url);
    await source.openFolder('photos');
    await comparison.openFolder('archive');
    expect(new URL(page.url()).searchParams.get('src')).toBe('photos');
    expect(new URL(page.url()).searchParams.get('cmp')).toBe('archive');
    await source.openFolder('media');
    await page.reload();
    await expect(source.breadcrumbs()).toContainText('media');
    await expect(comparison.breadcrumbs()).toContainText('archive');
    await page.goBack();
    await expect(source.breadcrumbs()).not.toContainText('media');
    await page.goForward();
    await expect(source.breadcrumbs()).toContainText('media');
  });
});

test('folders precede files with right-aligned counts and filtered footer totals', async ({
  page,
  sharedApp,
  source,
  bottomBar,
  filterBar,
  manifest,
}) => {
  const file = fixture(manifest, 'sidecar.exact');
  await page.goto(browseUrl(sharedApp.url, file.volume));
  await expect.poll(() => source.folderNames()).not.toEqual([]);
  const volume = (await sharedApp.api.volumes()).find((entry) => entry.name === file.volume);
  if (!volume) throw new Error(`Missing fixture volume ${file.volume}`);
  const request = {
    volume_id: volume.id,
    dir: '',
    plan: null,
    filter: { cameras: [], ratings: [], flags: [], tags: [], selection_id: null },
  };
  const folders = await sharedApp.api.folders(request);
  expect(await source.foldersBeforeFiles()).toBe(true);
  const folder = folders.folders.find((entry) => entry.path === file.dir);
  if (!folder) throw new Error(`Missing scenario folder ${file.dir}`);
  expect((await source.folderState(folder.name)).count).toBe(folder.count.toLocaleString('en-US'));
  const geometry = await source.folderGeometry(folder.name);
  expect(geometry.countLeft).toBeGreaterThanOrEqual(geometry.nameRight ?? 0);
  await expect
    .poll(() => bottomBar.counts('source'))
    .toContain(`${folders.counts.in_folder.toLocaleString('en-US')} in folder`);
  await expect
    .poll(() => bottomBar.counts('source'))
    .toContain(`${folders.counts.in_subfolders.toLocaleString('en-US')} in subfolders`);
  await filterBar.camera(file.camera_expected ?? '');
  const filtered = await sharedApp.api.folders({
    ...request,
    filter: { ...request.filter, cameras: [file.camera_expected ?? ''] },
  });
  expect(filtered.counts.in_subfolders).toBeLessThan(folders.counts.in_subfolders);
  await expect
    .poll(() => bottomBar.counts('source'))
    .toContain(`${filtered.counts.in_subfolders.toLocaleString('en-US')} in subfolders`);
});
