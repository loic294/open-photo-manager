import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import { connectThroughUi } from '../../src/immich';
import { expect, test } from '../../src/test';

test.use({ appOptions: { env: { APM_IMMICH_QUIET_SECS: '1' } } });

test('a connected server becoming unreachable keeps the connection and recovers', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  mock.control.fail({ match: 'GET /api/server/about', status: 503 });
  await page.getByRole('button', { name: 'Check connection', exact: true }).click();
  await expect(page.getByText('Immich is not answering.', { exact: false })).toBeVisible();
  const unreachable = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(unreachable.connected).toBe(true);
  expect(unreachable.reachable).toBe(false);
  mock.control.clearFailures();
  await page.getByRole('button', { name: 'Check connection', exact: true }).click();
  await expect(page.getByText('Reachable', { exact: true })).toBeVisible();
  const recovered = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(recovered.connected).toBe(true);
  expect(recovered.reachable).toBe(true);
});

test('failed key revocation warns after disconnect and leaves the mock key intact', async ({
  app,
  page,
  shell,
  settingsPage,
  confirmDialog,
  mockImmich,
}) => {
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  const keys = mock.state.keys();
  expect(keys).toHaveLength(1);
  mock.control.fail({ match: 'DELETE /api/api-keys/*', status: 503 });
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await confirmDialog.confirm('Disconnect');
  await expect(page.getByText('Remove the key named', { exact: false })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Connect', exact: true })).toBeVisible();
  const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(status.connected).toBe(false);
  expect(mock.state.keys()).toEqual(keys);
});
