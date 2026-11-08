import { createHash } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { connectThroughUi, mapAndSync, readMockAsset, waitForTask } from '../../src/immich';
import { expect, test } from '../../src/immich-test';

test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('@smoke Asset move preserves a tracked asset and its album inside the library', async ({
  backedImmich: { app, database, mock, expectRemovedFiles },
  page,
  shell,
  settingsPage,
  source,
  movePage,
  confirmDialog,
}) => {
  test.setTimeout(240_000);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app, ['photos']);
  const asset = mock.state.assetByPath('/mnt/photos/sidecars/IMG_0010.JPG');
  if (!asset) throw new Error('Expected scanned Immich fixture asset');
  const id = String(asset.id);
  const album = mock.state.albums()[0]?.id;
  const tag = mock.state.tags()[0]?.id;
  if (!album || !tag) throw new Error('Expected fixture album and tag');
  await database.sql(`INSERT INTO public.album_asset ("albumId", "assetId")
    VALUES ('${album}'::uuid, '${id}'::uuid);
    INSERT INTO public.tag_asset ("tagId", "assetId") VALUES ('${tag}'::uuid, '${id}'::uuid);
    UPDATE public.asset SET "isFavorite" = true WHERE id = '${id}'::uuid;
    UPDATE public.asset_exif SET rating = 5 WHERE "assetId" = '${id}'::uuid;
    INSERT INTO public.asset_file ("assetId", type, path)
    VALUES ('${id}'::uuid, 'sidecar', '/mnt/photos/sidecars/IMG_0010.JPG.xmp')`);
  const photo = await readFile(app.volumePath('photos/sidecars/IMG_0010.JPG'));
  const sidecar = await readFile(app.volumePath('photos/sidecars/IMG_0010.JPG.xmp'));
  const before = await database.sql(`SELECT json_build_object('asset', a."updateId",
    'sidecar', f."updateId") FROM public.asset a JOIN public.asset_file f ON f."assetId" = a.id
    WHERE a.id = '${id}'::uuid AND f.type = 'sidecar'`);
  const cursor = mock.state.requests().next;
  const url = new URL('/move', app.url);
  url.searchParams.set('src', 'photos/sidecars');
  url.searchParams.set('cmp', 'photos/dupes/m1');
  await page.goto(url.href);
  await source.useFolder();
  await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
  await movePage.chooseMoveType('flat');
  const sourceCard = source.card('IMG_0010.JPG');
  await expect(sourceCard.element).toHaveAttribute('aria-label', /In Immich/);
  await sourceCard.select();
  await expect(movePage.actionButton()).toBeEnabled();
  const selected = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/selection$/.test(new URL(response.url()).pathname),
  );
  await movePage.actionButton().click();
  expect(((await (await selected).json()) as SelectionDto).item_count).toBe(1);
  const executing = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/execute$/.test(new URL(response.url()).pathname),
  );
  expectRemovedFiles(['photos/sidecars/IMG_0010.JPG', 'photos/sidecars/IMG_0010.JPG.xmp']);
  await confirmDialog.confirm('Move 1 file');
  const response = await executing;
  await waitForTask(app, ((await response.json()) as { id: number }).id);
  const planId = Number(new URL(response.url()).pathname.split('/')[3]);
  const plan = await app.api.request<PlanDto>('GET', `/api/plans/${planId}`);
  expect(plan.result).toMatchObject({ done: 1, skipped: 0, failed: 0 });
  const newPath = '/mnt/photos/dupes/m1/IMG_0010.JPG';
  expect(await readFile(app.volumePath('photos/dupes/m1/IMG_0010.JPG'))).toEqual(photo);
  expect(await readFile(app.volumePath('photos/dupes/m1/IMG_0010.JPG.xmp'))).toEqual(sidecar);
  await expect(stat(app.volumePath('photos/sidecars/IMG_0010.JPG'))).rejects.toMatchObject({
    code: 'ENOENT',
  });
  await expect
    .poll(() => database.sql(`SELECT "originalPath" FROM public.asset WHERE id = '${id}'::uuid`), {
      timeout: 120_000,
    })
    .toBe(newPath);
  const after = JSON.parse(
    await database.sql(`SELECT json_build_object('path', a."originalPath",
      'name', a."originalFileName", 'checksum', encode(a.checksum, 'hex'),
      'asset', a."updateId", 'sidecar', f."updateId", 'sidecar_path', f.path,
      'associated', EXISTS(SELECT 1 FROM public.album_asset aa
        WHERE aa."assetId" = a.id AND aa."albumId" = '${album}'::uuid),
      'tagged', EXISTS(SELECT 1 FROM public.tag_asset ta
        WHERE ta."assetId" = a.id AND ta."tagId" = '${tag}'::uuid),
      'favorite', a."isFavorite",
      'rating', (SELECT rating FROM public.asset_exif WHERE "assetId" = a.id))
      FROM public.asset a JOIN public.asset_file f ON f."assetId" = a.id
      WHERE a.id = '${id}'::uuid AND f.type = 'sidecar'`),
  ) as {
    path: string;
    name: string;
    checksum: string;
    asset: string;
    sidecar: string;
    sidecar_path: string;
    associated: boolean;
    tagged: boolean;
    favorite: boolean;
    rating: number;
  };
  expect(after).toMatchObject({
    path: newPath,
    name: 'IMG_0010.JPG',
    checksum: createHash('sha1').update(`path:${newPath}`).digest('hex'),
    sidecar_path: `${newPath}.xmp`,
    associated: true,
    tagged: true,
    favorite: true,
    rating: 5,
  });
  const cursors = JSON.parse(before) as { asset: string; sidecar: string };
  expect(after.asset).not.toBe(cursors.asset);
  expect(after.sidecar).not.toBe(cursors.sidecar);
  expect(await readMockAsset(mock.url, id)).toMatchObject({
    id,
    originalPath: newPath,
    isFavorite: true,
    exifInfo: expect.objectContaining({ rating: 5 }),
    tags: expect.arrayContaining([expect.objectContaining({ id: tag })]),
  });
  expect(mock.state.albumsOf(id).map((entry) => entry.id)).toContain(album);
  await expect
    .poll(
      async () =>
        (await app.api.listing('photos', 'dupes/m1')).items.find(
          (item) => item.name === 'IMG_0010.JPG',
        )?.in_immich,
    )
    .toBe(true);
  const calls = mock.state.requests(cursor).entries;
  expect(
    calls.filter((entry) => entry.method === 'DELETE' && entry.path === '/api/assets'),
  ).toEqual([]);
});
