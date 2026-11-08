import { testUsers } from '../../../tools/mock-immich/index.ts';
import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { connectThroughUi, mapAndSync } from '../../src/immich';
import { expect, test } from '../../src/immich-readonly-test';

test.use({ allowedErrors: ['API 401:'] });

test('revoked key requests reconnection without losing the saved mapping', async ({
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
  await mapAndSync(app, ['photos']);
  const before = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(before.mappings).toHaveLength(1);
  expect(mock.control.revokeKey()).toBe(1);
  await page.getByRole('button', { name: 'Check connection', exact: true }).click();
  await expect(
    page.getByText('Immich no longer accepts the stored key.', { exact: false }),
  ).toBeVisible();
  await expect(settingsPage.field('Server URL')).toHaveValue(mock.url);
  await expect(settingsPage.field('Email')).toHaveValue(testUsers.admin.email);
  await expect(settingsPage.field('Password')).toBeEmpty();
  await expect(shell.menu().getByLabel('Needs attention')).toBeVisible();
  await expectNoSeriousA11yViolations(page, { include: ['apm-settings-immich'] });
  const revoked = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(revoked.needs_reconnect).toBe(true);
  expect(revoked.mappings).toEqual(before.mappings);
  await expect(page.getByRole('button', { name: 'Sync now', exact: true })).toBeDisabled();
  await settingsPage.fill('Password', testUsers.admin.password);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByText('Full mode:', { exact: false })).toBeVisible();
  await expect(shell.menu().getByLabel('Needs attention')).toHaveCount(0);
  const recovered = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(recovered.needs_reconnect).toBe(false);
  expect(recovered.mappings).toEqual(before.mappings);
});
