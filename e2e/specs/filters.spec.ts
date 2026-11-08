import { browseUrl, fixture } from '../src/scenarios';
import { expect, test } from '../src/test';

// Reload intentionally closes the EventSource; filters are session-only by design.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('camera, stars, flags and sidecar tags narrow both panes and clear together', async ({
  page,
  sharedApp,
  source,
  comparison,
  filterBar,
  manifest,
  bottomBar,
}) => {
  const file = fixture(manifest, 'sidecar.exact');
  await page.goto(browseUrl(sharedApp.url, `${file.volume}/${file.dir}`));
  await expect(source.card(file.name).element).toBeVisible();
  await filterBar.camera(file.camera_expected ?? '');
  await filterBar.rating(file.rating_expected ?? 0);
  await filterBar.flag('Selected');
  await filterBar.tag(file.tags_expected?.[0] ?? '');
  await expect.poll(() => source.names()).toEqual([file.name]);
  await expect.poll(() => comparison.names()).toEqual([file.name]);
  await expect.poll(() => bottomBar.counts('source')).toContain('1 in folder');
  await filterBar.clear();
  await expect.poll(() => source.names()).not.toEqual([file.name]);
  await filterBar.rating(file.rating_expected ?? 0);
  await page.reload();
  await expect.poll(() => source.names()).not.toEqual([file.name]);
});

test('rejected flag and facet counts agree with real index', async ({
  page,
  sharedApp,
  source,
  filterBar,
  manifest,
}) => {
  const file = fixture(manifest, 'exif.rating_rejected');
  await page.goto(browseUrl(sharedApp.url, `${file.volume}/${file.dir}`));
  await filterBar.flag('Rejected');
  const items = (await sharedApp.api.listing(file.volume, file.dir)).items.filter(
    (item) => item.flag === -1,
  );
  await expect.poll(() => source.names()).toEqual(items.map((item) => item.name));
  await filterBar.clear();
  const cameraFile = fixture(manifest, 'sidecar.exact');
  await page.goto(browseUrl(sharedApp.url, `${cameraFile.volume}/${cameraFile.dir}`));
  const search = await filterBar.searchCamera('');
  await expect(search).toBeVisible();
  const facets = filterBar.facets('Cameras');
  await expect(facets.first()).toHaveAttribute('aria-label', /\(\d+\)/);
});

test('unmatched folders are greyed or hidden without changing their names', async ({
  page,
  sharedApp,
  source,
  filterBar,
  manifest,
}) => {
  const file = fixture(manifest, 'sidecar.exact');
  await page.goto(browseUrl(sharedApp.url, file.volume));
  await filterBar.camera(file.camera_expected ?? '');
  const volumes = await sharedApp.api.volumes();
  const response = await sharedApp.api.folders({
    volume_id: volumes.find((v) => v.name === file.volume)?.id ?? null,
    dir: '',
    plan: null,
    filter: {
      cameras: [file.camera_expected ?? ''],
      ratings: [],
      flags: [],
      tags: [],
      selection_id: null,
    },
  });
  const empty = response.folders.find((folder) => folder.count === 0);
  if (!empty) throw new Error('Fixture must have an unmatched folder');
  await expect(source.folder(empty.name)).toHaveClass(/opacity-50/);
  await filterBar.hideUnmatched(true);
  await expect(source.folder(empty.name)).toHaveCount(0);
  await filterBar.hideUnmatched(false);
  await expect(source.folder(empty.name)).toBeVisible();
});
