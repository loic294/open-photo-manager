import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { testUsers } from '../../../tools/mock-immich/index.ts';
import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import { connectThroughUi } from '../../src/immich';
import { expect, test } from '../../src/immich-readonly-test';

async function assertNoSecrets(directory: string, secrets: readonly string[]): Promise<void> {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      await assertNoSecrets(path, secrets);
    } else if (entry.isFile()) {
      const contents = await readFile(path);
      for (const secret of secrets) {
        expect(contents.includes(Buffer.from(secret)), `credential in ${path}`).toBe(false);
      }
    } else {
      throw new Error(`Unexpected non-file in private app data directory: ${path}`);
    }
  }
}

test('@smoke Settings connects without persisting the password or API key in plaintext', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  let secret: string | undefined;
  const mock = await mockImmich({
    serverVersion: '3.3.3',
    scanDelayMs: 0,
    onApiKeyCreated: (value) => {
      if (secret !== undefined) throw new Error('Unexpected second API key');
      secret = value;
    },
  });
  const scans = await mock.control.scan();
  expect(scans.map(({ status }) => status)).toEqual(scans.map(() => 'completed'));
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  expect(secret).toBeDefined();
  if (!secret) throw new Error('Mock did not create an API key');
  await expect(page.getByText('admin@example.test', { exact: true })).toBeVisible();
  await expect(page.getByText('Immich v3.3.3', { exact: false })).toBeVisible();
  await expect(settingsPage.field('Password')).toHaveCount(0);
  const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
  expect(status.connection).toMatchObject({
    server_url: mock.url,
    email: testUsers.admin.email,
    is_admin: true,
    server_version: 'v3.3.3',
  });
  for (const value of [testUsers.admin.password, secret]) {
    expect(await page.locator('body').textContent()).not.toContain(value);
    expect(app.logs()).not.toContain(value);
    expect(JSON.stringify(mock.state.requests())).not.toContain(value);
  }
  await assertNoSecrets(app.dataDir, [testUsers.admin.password, secret]);
});
