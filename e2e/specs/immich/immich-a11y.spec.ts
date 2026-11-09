import { testUsers } from '../../../tools/mock-immich/index.ts';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/immich-readonly-test';
import { captureScreens } from '../../src/screens';

test.use({ allowedErrors: ['API 401:'] });

test('Immich settings remain accessible through keyboard connection and reconnect', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 800, height: 900 });
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await page.goto(app.url);
  await page.getByRole('button', { name: 'Menu', exact: true }).click();
  await shell.navigate('Settings');
  await settingsPage.tab('Immich');
  const check = async (name: string) => {
    for (const theme of ['light', 'dark'] as const) {
      await shell.theme(theme);
      await expectNoSeriousA11yViolations(page, { include: ['apm-settings-immich'] });
    }
    await captureScreens(page, name);
  };
  await check('immich-disconnected');
  await settingsPage.field('Server URL').focus();
  await page.keyboard.type(mock.url);
  await page.keyboard.press('Tab');
  await expect(settingsPage.field('Email')).toBeFocused();
  await page.keyboard.type(testUsers.admin.email);
  await page.keyboard.press('Tab');
  await expect(settingsPage.field('Password')).toBeFocused();
  await page.keyboard.type(testUsers.admin.password);
  await page.keyboard.press('Tab');
  await expect(page.getByLabel('Allow a self-signed certificate')).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(page.getByRole('button', { name: 'Connect', exact: true })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByText('Full mode:', { exact: false })).toBeVisible();
  await check('immich-connected');
  expect(mock.control.revokeKey()).toBe(1);
  await page.getByRole('button', { name: 'Check connection', exact: true }).click();
  await expect(
    page.getByText('Immich no longer accepts the stored key.', { exact: false }),
  ).toBeVisible();
  await check('immich-reconnect');
});
