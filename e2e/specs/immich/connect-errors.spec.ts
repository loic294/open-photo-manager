import { testUsers } from '../../../tools/mock-immich/index.ts';
import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { expect, test } from '../../src/immich-readonly-test';

test.use({
  appOptions: { env: { OPM_IMMICH_QUIET_SECS: '1' } },
  allowedErrors: [
    /^(API 400: http:\/\/127\.0\.0\.1:\d+\/api\/immich\/connect|console\.error: Failed to load resource: the server responded with a status of 400 \(Bad Request\))$/,
  ],
});

test('an unsupported URL is explained without contacting Immich', async ({
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
  await expect(settingsPage.field('Server URL')).toHaveAttribute('aria-invalid', 'true');
  await expect(settingsPage.field('Email')).toHaveValue(testUsers.admin.email);
  expect(mock.state.requests().entries.filter((entry) => entry.path === '/api/auth/login')).toEqual(
    [],
  );
  expect((await app.api.request<ImmichStatusDto>('GET', '/api/immich/status')).connected).toBe(
    false,
  );
});

test('a wrong password preserves the address and email without retrying the login', async ({
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
  await settingsPage.fill('Email', testUsers.admin.email);
  await settingsPage.fill('Password', 'wrong-test-password');
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(
    page.getByText('Immich refused the email or password.', { exact: false }),
  ).toBeVisible();
  await expectNoSeriousA11yViolations(page, { include: ['opm-settings-immich'] });
  await expect(settingsPage.field('Server URL')).toHaveValue(mock.url);
  await expect(settingsPage.field('Email')).toHaveValue(testUsers.admin.email);
  const attempts = mock.state
    .requests()
    .entries.filter((entry) => entry.path === '/api/auth/login');
  expect(attempts.map(({ status }) => status)).toEqual([401]);
  expect((await app.api.request<ImmichStatusDto>('GET', '/api/immich/status')).connected).toBe(
    false,
  );
  await settingsPage.fill('Password', testUsers.admin.password);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByText('Full mode:', { exact: false })).toBeVisible();
  await expectNoSeriousA11yViolations(page, { include: ['opm-settings-immich'] });
  const after = mock.state.requests().entries.filter((entry) => entry.path === '/api/auth/login');
  expect(after.map(({ status }) => status)).toEqual([401, 201]);
});

test('a pending login leaves the main menu responsive and cannot be submitted twice', async ({
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
  await settingsPage.fill('Email', testUsers.admin.email);
  await settingsPage.fill('Password', testUsers.admin.password);
  mock.control.gate('POST /api/auth/login');
  try {
    await page.getByRole('button', { name: 'Connect', exact: true }).click();
    await mock.control.waitForHeld(1);
    await expect(page.getByRole('button', { name: 'Connecting…' })).toBeDisabled();
    await shell.navigate('Browse');
    expect(
      mock.state.requests().entries.filter((entry) => entry.path === '/api/auth/login'),
    ).toHaveLength(1);
  } finally {
    mock.control.releaseGate();
  }
  await expect
    .poll(
      async () => (await app.api.request<ImmichStatusDto>('GET', '/api/immich/status')).connected,
    )
    .toBe(true);
  await shell.navigate('Settings');
  await settingsPage.tab('Immich');
  await expect(page.getByText('Full mode:', { exact: false })).toBeVisible();
  await expect(shell.menu()).toBeVisible();
  expect(
    mock.state.requests().entries.filter((entry) => entry.path === '/api/auth/login'),
  ).toHaveLength(1);
});
