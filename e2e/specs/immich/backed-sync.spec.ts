import { connectThroughUi, mapAndSync } from '../../src/immich';
import { expect, test } from '../../src/immich-test';

test('Settings connection and full sync match fixture files to the same SQL-backed assets', async ({
  backedImmich: { app, database, mock },
  page,
  shell,
  settingsPage,
}) => {
  test.setTimeout(180_000);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app);
  const asset = mock.state.assetByPath('/mnt/photos/sidecars/IMG_0010.JPG');
  expect(asset).toBeDefined();
  if (!asset) throw new Error('Expected fixture asset was not scanned');
  const id = String(asset.id);
  expect(
    await database.sql(`SELECT "originalPath" FROM public.asset WHERE id = '${id}'::uuid`),
  ).toBe('/mnt/photos/sidecars/IMG_0010.JPG');
  await expect
    .poll(async () => {
      const listing = await app.api.listing('photos', 'sidecars');
      return listing.items.find((item) => item.name === 'IMG_0010.JPG')?.in_immich;
    })
    .toBe(true);
});
