import type { ImmichStatusDto } from '../../web/src/api/generated/ImmichStatusDto';
import { expect, test } from '../src/test';

test('real Settings connects to the fixture-mounted shared mock', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  const mock = await mockImmich();
  await page.goto(app.url);
  await shell.navigate('Settings');
  await settingsPage.tab('Immich');
  await settingsPage.fill('Server URL', mock.url);
  await settingsPage.fill('Email', 'admin@example.test');
  await settingsPage.fill('Password', 'admin-password');
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(
    page.getByText('Full mode: Libraries are listed and the trash folder can be excluded.', {
      exact: true,
    }),
  ).toBeVisible();
  const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(status.connected).toBe(true);
  expect(status.connection?.server_url).toBe(mock.url);
  await mock.control.scan();
  expect(mock.state.assets().length).toBeGreaterThan(500);
  expect(mock.state.violations()).toEqual([]);
});
