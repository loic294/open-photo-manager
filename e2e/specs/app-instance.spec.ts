import { access } from 'node:fs/promises';
import { AppInstance } from '../src/app-instance';
import { writeFile } from '../src/disk';
import { expect, indexSnapshot, test } from '../src/test';

test('pristine instances use separate ports and private copies @smoke', async ({ sharedApp }) => {
  const other = await AppInstance.start();
  try {
    expect(other.url).not.toBe(sharedApp.url);
    expect(other.volumesDir).not.toBe(sharedApp.volumesDir);
    expect((await other.api.volumes()).map((volume) => volume.name)).toEqual(['archive', 'photos']);
    expect(indexSnapshot(other).length).toBeGreaterThan(500);
    await writeFile(other, 'photos', 'e2e/private.txt', 'private');
    expect(indexSnapshot(sharedApp)).not.toContainEqual(
      expect.objectContaining({ name: 'private.txt' }),
    );
  } finally {
    await other.stop();
  }
  await expect(access(other.directory)).rejects.toThrow();
});

test('startup task is new even when terminal tasks were copied', async ({ app }) => {
  const tasks = await app.api.tasks();
  expect(
    tasks.filter((task) => task.kind === 'index' && task.state === 'done').length,
  ).toBeGreaterThanOrEqual(2);
  expect((await app.api.settings()).index.watch_enabled).toBe(false);
});

test('cold scan, suspend, resume, kill and restart preserve data and port', async ({ app }) => {
  test.setTimeout(120_000);
  const url = app.url;
  const before = indexSnapshot(app);
  await app.suspend();
  await expect(fetch(`${url}/api/health`, { signal: AbortSignal.timeout(300) })).rejects.toThrow();
  await app.resume();
  await app.waitForIdle();
  expect(app.url).toBe(url);
  await app.kill();
  await app.restart({ newPort: true });
  expect(app.url).not.toBe(url);
  expect(indexSnapshot(app)).toEqual(before);
});

test.describe('cold instance', () => {
  test.use({ appOptions: { cold: true } });
  test('waitForIdle observes completion rather than the initial empty task window', async ({
    app,
  }) => {
    test.setTimeout(120_000);
    await app.waitForIdle();
    expect(
      (await app.api.tasks()).some((task) => task.kind === 'index' && task.state === 'done'),
    ).toBe(true);
    expect(indexSnapshot(app).length).toBeGreaterThan(500);
  });
});
