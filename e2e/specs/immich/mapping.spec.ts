import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import { connectThroughUi } from '../../src/immich';
import { expect, test } from '../../src/immich-readonly-test';

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
  expect(updatedLibrary?.exclusionPatterns).toContain('**/.apm-trash/**');
});
