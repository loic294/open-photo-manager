import { expect, type Page } from '@playwright/test';
import { testUsers } from '../../tools/mock-immich/index.ts';
import type { ImmichLibrariesResponse } from '../../web/src/api/generated/ImmichLibrariesResponse';
import type { TaskDto } from '../../web/src/api/generated/TaskDto';
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

export async function waitForTask(app: AppInstance, id: number) {
  await expect
    .poll(
      async () => {
        const current = (await app.api.tasks()).find((entry) => entry.id === id);
        if (current?.state === 'failed' || current?.state === 'interrupted') {
          throw new Error(`Task ${id} failed: ${current.error}`);
        }
        return current?.state;
      },
      { timeout: 120_000 },
    )
    .toBe('done');
}

/** Read through the mock API using a separate test key, never the application's stored key. */
export async function readMockAsset(mockUrl: string, id: string): Promise<unknown> {
  const session = await fetch(`${mockUrl}/api/auth/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ email: testUsers.admin.email, password: testUsers.admin.password }),
  });
  expect(session.status).toBe(201);
  const auth = (await session.json()) as { accessToken: string };
  const keys = await fetch(`${mockUrl}/api/api-keys`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${auth.accessToken}` },
    body: JSON.stringify({ name: 'asset-assertion', permissions: ['asset.read'] }),
  });
  expect(keys.status).toBe(201);
  const key = (await keys.json()) as { secret: string };
  const response = await fetch(`${mockUrl}/api/assets/${id}`, {
    headers: { 'x-api-key': key.secret },
  });
  expect(response.status).toBe(200);
  return response.json();
}

/** Connect first; use this to map all private fixture volumes and sync them. */
export async function mapAndSync(app: AppInstance, names = ['photos', 'archive']) {
  const volumes = await app.api.volumes();
  const libraries = await app.api.request<ImmichLibrariesResponse>('GET', '/api/immich/libraries');
  expect(libraries.limited).toBe(false);
  const mappings = names.map((name) => {
    const volume = volumes.find((entry) => entry.name === name);
    if (!volume) throw new Error(`Missing fixture volume ${name}`);
    const path = `/mnt/${volume.name}`;
    const library = libraries.libraries.find((entry) => entry.import_paths.includes(path));
    if (!library) throw new Error(`Missing mock library for ${path}`);
    return { volume_id: volume.id, library_id: library.id, path_prefix: path };
  });
  await app.api.request('PUT', '/api/immich/mappings', { mappings });
  const task = await app.api.request<TaskDto>('POST', '/api/immich/sync', { full: true });
  await waitForTask(app, task.id);
}
