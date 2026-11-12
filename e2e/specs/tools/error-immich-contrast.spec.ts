import { testUsers } from '../../../tools/mock-immich/index.ts';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/test';

test.use({
  allowedErrors: [
    /^(API 400: http:\/\/127\.0\.0\.1:\d+\/api\/immich\/connect|console\.error: Failed to load resource: the server responded with a status of 400 \(Bad Request\))$/,
  ],
});

test('Immich validation and connected error controls remain readable in both themes', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  await page.goto(app.url);
  await shell.navigate('Settings');
  await settingsPage.tab('Immich');
  await settingsPage.fill('Server URL', 'ftp://example.invalid');
  await settingsPage.fill('Email', testUsers.admin.email);
  await settingsPage.fill('Password', testUsers.admin.password);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByText('Enter a valid HTTP or HTTPS address for Immich.')).toBeVisible();
  for (const theme of ['light', 'dark'] as const) {
    await shell.theme(theme);
    await expectNoSeriousA11yViolations(page, { include: ['opm-settings-immich'] });
  }
  await settingsPage.fill('Server URL', mock.url);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByText('Full mode:', { exact: false })).toBeVisible();
  const disconnect = page.getByRole('button', { name: 'Disconnect', exact: true });
  await expect(disconnect).toBeEnabled();
  for (const theme of ['light', 'dark'] as const) {
    await shell.theme(theme);
    await expectNoSeriousA11yViolations(page, { include: ['opm-settings-immich'] });
    await disconnect.hover();
    await expectNoSeriousA11yViolations(page, { include: ['opm-settings-immich'] });
    await disconnect.focus();
    await expectNoSeriousA11yViolations(page, { include: ['opm-settings-immich'] });
  }
});
