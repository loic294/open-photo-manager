import { readFile } from 'node:fs/promises';
import { scenarioFiles, testUsers } from '../../../tools/mock-immich/index.ts';
import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/immich-readonly-test';

test('a regular account connects in limited mode with manual mappings', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await page.goto(app.url);
  await shell.navigate('Settings');
  await settingsPage.tab('Immich');
  await settingsPage.fill('Server URL', mock.url);
  await settingsPage.fill('Email', testUsers.user.email);
  await settingsPage.fill('Password', testUsers.user.password);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByText('Limited mode:', { exact: false }).first()).toBeVisible();
  await expect(
    page.getByText('Library selection and trash exclusion need an administrator account.', {
      exact: false,
    }),
  ).toBeVisible();
  await expect(page.getByLabel('Immich library for photos', { exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Keep the trash out of Immich' })).toHaveCount(0);
  const prefix = page.getByLabel('Path prefix for photos', { exact: true });
  await prefix.fill('/mnt/photos');
  await expect(prefix).toHaveValue('/mnt/photos');
  await expectNoSeriousA11yViolations(page, { include: ['opm-settings-immich'] });
  const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(status.mode).toBe('limited');
  expect(status.connection?.email).toBe(testUsers.user.email);
});

test('a regular account maps a manual prefix and syncs its own scanned assets', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  test.setTimeout(180_000);
  const scenario = JSON.parse(await readFile(scenarioFiles.fixtures, 'utf8')) as {
    users: Array<{ email: string }>;
  };
  scenario.users = scenario.users.filter((user) => user.email === testUsers.user.email);
  const mock = await mockImmich({ scenario, serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await page.goto(app.url);
  await shell.navigate('Settings');
  await settingsPage.tab('Immich');
  await settingsPage.fill('Server URL', mock.url);
  await settingsPage.fill('Email', testUsers.user.email);
  await settingsPage.fill('Password', testUsers.user.password);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByText('Limited mode:', { exact: false }).first()).toBeVisible();
  await settingsPage.fill('Path prefix for photos', '/mnt/photos');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect
    .poll(
      async () =>
        (await app.api.request<ImmichStatusDto>('GET', '/api/immich/status')).sync?.last_sync_at ??
        null,
      { timeout: 120_000 },
    )
    .not.toBeNull();
  const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(status.mappings).toEqual([
    expect.objectContaining({
      volume_name: 'photos',
      path_prefix: '/mnt/photos',
      library_id: null,
    }),
  ]);
  const library = mock.state.libraries().find((entry) => entry.name === 'Photos');
  const assetCount = mock.state.assets().filter((asset) => asset.libraryId === library?.id).length;
  expect(assetCount).toBeGreaterThan(0);
  expect(status.sync?.assets_total).toBe(assetCount);
  await expect
    .poll(
      async () =>
        (await app.api.listing('photos', 'sidecars')).items.find(
          (item) => item.name === 'IMG_0010.JPG',
        )?.in_immich,
    )
    .toBe(true);
});
