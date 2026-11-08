import { writeFile } from '../src/disk';
import { browseUrl, fixture } from '../src/scenarios';
import { expect, test } from '../src/test';

// Reload intentionally closes the old EventSource.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

for (const key of ['exif.full_nikon', 'exif.no_date', 'media.heic', 'media.video_mp4']) {
  test(`filename, date and real thumbnail: ${key}`, async ({ sharedApp, source, manifest }) => {
    const file = fixture(manifest, key);
    await source.goto(sharedApp.url, file);
    const card = source.card(file.name);
    await card.expectThumbnail();
    const item = (await sharedApp.api.listing(file.volume, file.dir)).items.find(
      (item) => item.name === file.name,
    );
    expect(item?.capture_src).toBe(file.capture_src_expected);
    await expect(card.date()).toContainText(
      new Date((item?.capture_ts ?? 0) * 1000).getUTCFullYear().toString(),
    );
    await expect(card.date()).toHaveAttribute(
      'title',
      key === 'exif.no_date' ? /File date/ : /Capture date/,
    );
  });
}

test('thumbnail slider changes card dimensions and survives reload', async ({
  page,
  sharedApp,
  source,
  bottomBar,
  manifest,
}) => {
  const file = fixture(manifest, 'sidecar.exact');
  await source.goto(sharedApp.url, file);
  const before = await source.card(file.name).element.boundingBox();
  await bottomBar.size('source', 240);
  await expect
    .poll(async () => (await source.card(file.name).element.boundingBox())?.width)
    .toBeGreaterThan(before?.width ?? 0);
  await page.reload();
  await expect.poll(() => bottomBar.sizeValue('source')).toBe('240');
});

test('thousands of real indexed items render only the visible cards and scroll to the last', async ({
  app,
  page,
  source,
  manifest,
}) => {
  test.setTimeout(120_000);
  const file = fixture(manifest, 'exif.full_nikon');
  // Plain files exercise the same grid without 2,500 external metadata tool invocations.
  const bytes = 'A private virtualization fixture';
  await Promise.all(
    Array.from({ length: 2_500 }, (_, i) =>
      writeFile(app, file.volume, `e2e-grid/ITEM_${String(i).padStart(5, '0')}.txt`, bytes),
    ),
  );
  await app.rescan();
  await page.goto(browseUrl(app.url, `${file.volume}/e2e-grid`));
  await expect.poll(() => source.names()).not.toEqual([]);
  expect(await source.element.getByRole('option').count()).toBeLessThan(100);
  await source.scrollToEnd();
  await expect(source.card('ITEM_02499.txt').element).toBeVisible();
  expect(await source.element.getByRole('option').count()).toBeLessThan(100);
  expect((await app.api.listing(file.volume, 'e2e-grid')).total).toBe(2_500);
});
