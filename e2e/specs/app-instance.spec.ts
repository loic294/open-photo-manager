import { access } from 'node:fs/promises';
import { AppInstance } from '../src/app-instance';
import {
  diffTrees,
  makeDir,
  makeSparseFile,
  removeFile,
  snapshotTree,
  touch,
  trashEntries,
  writeFile,
} from '../src/disk';
import { assertReadOnly, captureReadOnly, expect, indexSnapshot, test } from '../src/test';
import { waitForFile, waitForIndexed } from '../src/waits';

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

test('failure during settings setup cleans the private instance', async () => {
  let failure: unknown;
  try {
    await AppInstance.start({
      settings: () => {
        throw new Error('deliberate setup failure');
      },
    });
  } catch (error) {
    failure = error;
  }
  expect(failure).toBeInstanceOf(Error);
  const directory = /App start failed at ([^:]+):/.exec((failure as Error).message)?.[1];
  if (!directory) throw new Error('Missing failed instance directory');
  await expect(access(directory)).rejects.toThrow();
});

test('external-file helpers and indexed waits use the private fixture tree', async ({
  app,
  manifest,
}) => {
  const readOnly = await captureReadOnly(app);
  const before = await snapshotTree(app.volumesDir, { hash: true });
  await makeDir(app, 'photos', 'e2e-helpers');
  const escapeEntry = manifest.filesOf('symlink.escape_dir')[0];
  await expect(
    writeFile(app, escapeEntry.volume, `${escapeEntry.path}/blocked.txt`, 'blocked'),
  ).rejects.toThrow('symlink');
  await writeFile(app, 'photos', 'e2e-helpers/test.txt', 'a new file');
  await expect(assertReadOnly(app, readOnly)).rejects.toThrow('sharedApp volume mutation');
  await waitForFile(app, 'photos/e2e-helpers/test.txt');
  await touch(app, 'photos', 'e2e-helpers/test.txt', new Date('2026-01-01T00:00:00Z'));
  await makeSparseFile(app, 'photos', 'e2e-helpers/sparse.bin', 1024 * 1024);
  await expect(
    makeSparseFile(app, 'photos', 'e2e-helpers/too-large.bin', 4 * 1024 ** 3 + 1),
  ).rejects.toThrow('4 GiB');
  const delta = diffTrees(before, await snapshotTree(app.volumesDir, { hash: true }));
  expect(delta.added.map((entry) => entry.path)).toContain('photos/e2e-helpers/test.txt');
  expect(await trashEntries(app)).toEqual([]);
  await app.rescan();
  await waitForIndexed(app, 'photos/e2e-helpers/test.txt');
  await removeFile(app, 'photos', 'e2e-helpers/test.txt');
  expect(diffTrees(before, await snapshotTree(app.volumesDir)).added.length).toBeGreaterThan(0);
});

test.describe('explicit native watching', () => {
  test.use({ appOptions: { watcher: true } });
  test('watcher option opts into the generated status API', async ({ app }) => {
    expect((await app.api.indexStatus()).watcher?.state).toBe('watching');
    expect((await app.api.settings()).index.watch_enabled).toBe(true);
  });
});

test('shared mutations script returns the seven external fixture changes', async ({ app }) => {
  const mutations = await app.applyFixtureMutations();
  expect(mutations).toHaveLength(7);
  await app.rescan();
});
