import type { ImmichLibrariesResponse } from '../../../web/src/api/generated/ImmichLibrariesResponse';
import type { TaskDto } from '../../../web/src/api/generated/TaskDto';
import { connectThroughUi } from '../../src/immich';
import { expect, test } from '../../src/immich-test';

test('Settings connection and full sync match fixture files to the same SQL-backed assets', async ({
  backedImmich: { app, database, mock },
  page,
  shell,
  settingsPage,
}) => {
  test.setTimeout(180_000);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  const volumes = await app.api.volumes();
  const libraries = await app.api.request<ImmichLibrariesResponse>('GET', '/api/immich/libraries');
  expect(libraries.limited).toBe(false);
  const mappings = volumes.map((volume) => {
    const path = `/mnt/${volume.name}`;
    const library = libraries.libraries.find((entry) => entry.import_paths.includes(path));
    if (!library) throw new Error(`Missing mock library for ${path}`);
    return { volume_id: volume.id, library_id: library.id, path_prefix: path };
  });
  await app.api.request('PUT', '/api/immich/mappings', { mappings });
  const task = await app.api.request<TaskDto>('POST', '/api/immich/sync', { full: true });
  await expect
    .poll(
      async () => {
        const current = (await app.api.tasks()).find((entry) => entry.id === task.id);
        if (current?.state === 'failed' || current?.state === 'interrupted') {
          throw new Error(`Immich sync failed: ${current.error}`);
        }
        return current?.state;
      },
      { timeout: 120_000 },
    )
    .toBe('done');
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
