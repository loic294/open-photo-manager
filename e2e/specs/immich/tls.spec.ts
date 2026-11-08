import { testUsers } from '../../../tools/mock-immich/index.ts';
import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import { expect, test } from '../../src/immich-readonly-test';

test.use({ allowedErrors: ['API 502:'] });

test('a self-signed Immich certificate requires an explicit persisted opt-in', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0, tls: true });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await page.goto(app.url);
  await shell.navigate('Settings');
  await settingsPage.tab('Immich');
  await settingsPage.fill('Server URL', mock.url);
  await settingsPage.fill('Email', testUsers.admin.email);
  await settingsPage.fill('Password', testUsers.admin.password);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByRole('alert').filter({ hasText: /certificate|TLS/i })).toBeVisible();
  expect((await app.api.request<ImmichStatusDto>('GET', '/api/immich/status')).connected).toBe(
    false,
  );
  await expect(settingsPage.field('Server URL')).toHaveValue(mock.url);
  await expect(settingsPage.field('Email')).toHaveValue(testUsers.admin.email);
  await page.getByLabel('Allow a self-signed certificate', { exact: true }).check();
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByText('Full mode:', { exact: false })).toBeVisible();
  const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(status.connection?.allow_insecure_tls).toBe(true);
  await app.restart();
  const persisted = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(persisted.connection?.allow_insecure_tls).toBe(true);
});
