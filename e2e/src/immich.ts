import { expect, type Page } from '@playwright/test';
import { testUsers } from '../../tools/mock-immich/index.ts';
import type { AppInstance } from './app-instance';
import type { SettingsPage } from './pages/settings';
import type { ShellPage } from './pages/shell';

/** Connect the shared fixture-mounted mock through the actual Settings form. */
export async function connectThroughUi(
  app: AppInstance,
  page: Page,
  shell: ShellPage,
  settings: SettingsPage,
  mockUrl: string,
) {
  await page.goto(app.url);
  await shell.navigate('Settings');
  await settings.tab('Immich');
  await settings.fill('Server URL', mockUrl);
  await settings.fill('Email', testUsers.admin.email);
  await settings.fill('Password', testUsers.admin.password);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByText('Full mode:', { exact: false })).toBeVisible();
}
