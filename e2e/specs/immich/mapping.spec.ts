import { readFile } from 'node:fs/promises';
import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import type { TaskDto } from '../../../web/src/api/generated/TaskDto';
import { writeFile } from '../../src/disk';
import { connectThroughUi, mapAndSync, waitForTask } from '../../src/immich';
import { expect, test } from '../../src/immich-readonly-test';
import { browseUrl } from '../../src/scenarios';
import { waitForIndexed } from '../../src/waits';

test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('mapping detects a scanned library, syncs it, and excludes the application trash', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  test.setTimeout(180_000);
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  const scans = await mock.control.scan();
  expect(scans.map(({ status }) => status)).toEqual(scans.map(() => 'completed'));
  await connectThroughUi(app, page, shell, settingsPage, mock.url);

  const row = page.getByRole('row').filter({ has: page.getByLabel('Path prefix for photos') });
  const library = page.getByLabel('Immich library for photos', { exact: true });
  await expect(library.locator('option', { hasText: 'Photos' })).toHaveCount(1);
  await library.selectOption({ label: 'Photos' });
  await row.getByRole('button', { name: 'Auto-detect', exact: true }).click();
  const suggestion = row.getByRole('listitem').filter({ hasText: '/mnt/photos' });
  await expect(suggestion).toContainText('sampled files');
  await suggestion.getByRole('button', { name: 'Use', exact: true }).click();
  await expect(page.getByLabel('Path prefix for photos')).toHaveValue('/mnt/photos');
  await page.getByRole('button', { name: 'Save', exact: true }).click();

  await expect
    .poll(
      async () => {
        const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
        return status.sync?.last_sync_at ?? null;
      },
      { timeout: 120_000 },
    )
    .not.toBeNull();
  const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(status.mappings).toEqual([
    expect.objectContaining({ volume_name: 'photos', path_prefix: '/mnt/photos' }),
  ]);
  const photosLibrary = mock.state.libraries().find((item) => item.name === 'Photos');
  expect(photosLibrary).toBeDefined();
  expect(status.sync?.assets_total).toBe(
    mock.state.assets().filter((asset) => asset.libraryId === photosLibrary?.id).length,
  );
  await page.getByRole('button', { name: 'Keep the trash out of Immich' }).click();
  await expect(page.getByText('Exclusion pattern:', { exact: false })).toBeVisible();
  const updatedLibrary = mock.state.libraries().find((item) => item.name === 'Photos');
  expect(updatedLibrary?.exclusionPatterns).toContain('**/.opm-trash/**');
});

test('a later indexed file gains the Immich badge after its library scan and sync', async ({
  app,
  page,
  shell,
  settingsPage,
  source,
  comparison,
  mockImmich,
}) => {
  test.setTimeout(180_000);
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app, ['photos']);
  const original = await readFile(app.volumePath('photos/sidecars/IMG_0010.JPG'));
  await writeFile(app, 'photos', 'sidecars/e2e-later.jpg', original);
  const volume = (await app.api.volumes()).find((entry) => entry.name === 'photos');
  if (!volume) throw new Error('Expected fixture photos volume');
  const task = await app.api.rescan({
    targets: [{ volume_id: volume.id, dir: 'sidecars', recursive: false }],
    force: false,
    confirm_removals: false,
  });
  await waitForTask(app, task.id);
  await waitForIndexed(app, 'photos/sidecars/e2e-later.jpg');
  await page.goto(browseUrl(app.url, 'photos/sidecars'));
  await expect(source.card('e2e-later.jpg').element).toBeVisible();
  await expect(source.card('e2e-later.jpg').element).not.toHaveAttribute('aria-label', /In Immich/);
  expect(
    (await app.api.listing('photos', 'sidecars')).items.find(
      (entry) => entry.name === 'e2e-later.jpg',
    )?.in_immich,
  ).toBe(false);
  const [scan] = await mock.control.scan('Photos');
  expect(scan?.status, scan?.error ?? undefined).toBe('completed');
  expect(mock.state.assetByPath('/mnt/photos/sidecars/e2e-later.jpg')).toBeDefined();
  await shell.navigate('Settings');
  await settingsPage.tab('Immich');
  const started = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      new URL(response.url()).pathname === '/api/immich/sync',
  );
  await page.getByRole('button', { name: 'Sync now', exact: true }).click();
  const sync = (await (await started).json()) as TaskDto;
  await waitForTask(app, sync.id);
  await page.goto(browseUrl(app.url, 'photos/sidecars'));
  await expect(source.card('e2e-later.jpg').element).toHaveAttribute('aria-label', /In Immich/);
  await expect(comparison.card('e2e-later.jpg').element).toHaveAttribute('aria-label', /In Immich/);
  expect(
    (await app.api.listing('photos', 'sidecars')).items.find(
      (entry) => entry.name === 'e2e-later.jpg',
    )?.in_immich,
  ).toBe(true);
});
