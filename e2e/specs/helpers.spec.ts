import { mkdir, mkdtemp, rename, rm, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { expect, test } from '@playwright/test';
import { diffTrees, expectTree, snapshotTree } from '../src/disk';
import { Manifest } from '../src/manifest';
import { PageGuard } from '../src/page-guard';
import { cache, root } from '../src/paths';
import { expectStaysFalse } from '../src/waits';

test('manifest scenario, exact file, duplicates and empty directories are typed', async () => {
  const manifest = await Manifest.load(join(root, 'fixtures/out'));
  const file = manifest.filesOf('exif.full_nikon')[0];
  expect(manifest.file(file.volume, file.path)).toEqual(file);
  expect(manifest.scenario('media.heic')).toHaveProperty('skipped');
  expect(manifest.duplicateGroups(1).length).toBeGreaterThan(0);
  expect(manifest.emptyDirs().length).toBeGreaterThan(0);
  expect(() => manifest.filesOf('missing')).toThrow('Unknown fixture scenario');
});
test('tree diff detects moves and preserves symlinks without following them', async () => {
  await mkdir(cache, { recursive: true });
  const dir = await mkdtemp(join(cache, 'unit-'));
  try {
    await writeFile(join(dir, 'old'), 'same bytes');
    await symlink('old', join(dir, 'link'));
    const before = await snapshotTree(dir, { hash: true });
    await expectTree(dir, before);
    await rename(join(dir, 'old'), join(dir, 'new'));
    const after = await snapshotTree(dir, { hash: true });
    expect(diffTrees(before, after).moved).toEqual([{ from: 'old', to: 'new' }]);
    expect(after.find((entry) => entry.path === 'link')?.link).toBe('old');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
test('page guard reports console and network errors and supports exact allow-list entries', async ({
  page,
}) => {
  const guard = new PageGuard(page, ['approved warning']);
  await page.evaluate(() => {
    console.error('deliberate failure');
    console.warn('approved warning');
  });
  await expect.poll(() => guard.errors.length).toBe(1);
  expect(() => guard.assertClean()).toThrow('deliberate failure');
});
test('bounded negative wait rejects as soon as its condition becomes true', async () => {
  await expectStaysFalse(async () => false, { forMs: 20 });
  await expect(expectStaysFalse(async () => true, { forMs: 1_000 })).rejects.toThrow();
});
