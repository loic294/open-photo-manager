import { testUsers } from '../../../tools/mock-immich/index.ts';
import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import { expect, test } from '../../src/test';

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
  const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(status.mode).toBe('limited');
  expect(status.connection?.email).toBe(testUsers.user.email);
});
