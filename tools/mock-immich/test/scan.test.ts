import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import {
  call,
  connect,
  eventually,
  scratchDir,
  startMock,
  touch,
  typedBody,
  writeTree,
} from './helpers.ts';

const T0 = Date.parse('2024-03-01T10:00:00.000Z');

/** A mock whose Photos library (`/mnt/photo`) is a scratch directory with the files given. */
async function mounted(t: Parameters<typeof startMock>[0], files: Record<string, string>) {
  const dir = scratchDir(t, 'scan');
  writeTree(dir, files);
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir } });
  return { mock, dir };
}

const paths = (mock: Awaited<ReturnType<typeof startMock>>) =>
  mock.state
    .assets()
    .map((asset) => asset.originalPath as string)
    .sort();

test('a scan imports the media files, and only those, with the dates of the files', async (t) => {
  const { mock, dir } = await mounted(t, {
    '2024/IMG_0001.jpg': 'a',
    '2024/IMG_0002.HEIC': 'b',
    '2024/clip.mp4': 'c',
    '2024/raw.dng': 'd',
    'notes.txt': 'not media',
    '.hidden.jpg': 'hidden file',
    '.opm-trash/old.jpg': 'hidden directory',
    '2024/.cache/thumb.jpg': 'nested hidden directory',
  });
  touch(join(dir, '2024/IMG_0001.jpg'), T0);
  const [job] = await mock.control.scan('Photos');
  assert.equal(job?.status, 'completed');
  assert.deepEqual(job?.summary, {
    libraryId: job?.libraryId,
    imported: 4,
    updated: 0,
    restored: 0,
    offlined: 0,
    unchanged: 0,
    seen: 4,
    unmounted: [],
  });
  assert.deepEqual(paths(mock), [
    '/mnt/photo/2024/IMG_0001.jpg',
    '/mnt/photo/2024/IMG_0002.HEIC',
    '/mnt/photo/2024/clip.mp4',
    '/mnt/photo/2024/raw.dng',
  ]);
  const first = mock.state.assetByPath('/mnt/photo/2024/IMG_0001.jpg');
  assert.equal(first?.fileCreatedAt, new Date(T0).toISOString());
  assert.equal(first?.type, 'IMAGE');
  assert.equal(first?.originalFileName, 'IMG_0001.jpg');
  assert.equal(first?.libraryId, mock.state.libraries()[0]?.id);
  assert.equal(mock.state.assetByPath('/mnt/photo/2024/clip.mp4')?.type, 'VIDEO');
  assert.equal(mock.state.assetByPath('/mnt/photo/2024/raw.dng')?.type, 'IMAGE');
  assert.equal(mock.state.assetByPath('/mnt/photo/2024/IMG_0002.HEIC')?.type, 'IMAGE');
});

test('scanning again changes nothing', async (t) => {
  const { mock } = await mounted(t, { 'a.jpg': 'a', 'b.jpg': 'b' });
  await mock.control.scan();
  const before = mock.state.assets();
  const [job] = await mock.control.scan();
  assert.equal(job?.summary?.unchanged, 2);
  assert.equal(job?.summary?.imported, 0);
  assert.deepEqual(mock.state.assets(), before);
});

test('a file whose date changed is updated, and updatedAt moves for the sync', async (t) => {
  const { mock, dir } = await mounted(t, { 'a.jpg': 'a', 'b.jpg': 'b' });
  touch(join(dir, 'a.jpg'), T0);
  touch(join(dir, 'b.jpg'), T0 + 1000);
  await mock.control.scan();
  const { key } = await connect(mock);
  const seen = await call(mock, 'POST', '/api/search/metadata', { key, body: typedBody(null) });
  const highWater = seen.body.assets.items
    .map((item: { updatedAt: string }) => item.updatedAt)
    .sort()
    .at(-1);

  touch(join(dir, 'a.jpg'), T0 + 60_000);
  const [job] = await mock.control.scan();
  assert.equal(job?.summary?.updated, 1);
  assert.equal(job?.summary?.unchanged, 1);
  const changed = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(null, {}, { updatedAt: { gt: highWater } }),
  });
  assert.deepEqual(
    changed.body.assets.items.map((item: { originalFileName: string }) => item.originalFileName),
    ['a.jpg'],
  );
  assert.equal(changed.body.assets.items[0].fileCreatedAt, new Date(T0 + 60_000).toISOString());
});

test('a file that disappears makes its asset offline and trashed, and its return restores it', async (t) => {
  const { mock, dir } = await mounted(t, { 'a.jpg': 'a', 'b.jpg': 'b' });
  await mock.control.scan();
  const { key } = await connect(mock);
  const original = mock.state.assetByPath('/mnt/photo/a.jpg');

  rmSync(join(dir, 'a.jpg'));
  const [gone] = await mock.control.scan();
  assert.equal(gone?.summary?.offlined, 1);
  const offline = mock.state.assetByPath('/mnt/photo/a.jpg');
  assert.equal(offline?.isOffline, true);
  assert.notEqual(offline?.deletedAt, null);
  assert.equal(offline?.id, original?.id);
  assert.equal(mock.state.assetByPath('/mnt/photo/b.jpg')?.isOffline, false);

  const view = await call(mock, 'GET', `/api/assets/${original?.id}`, { key });
  assert.equal(view.body.isOffline, true);
  assert.equal(view.body.isTrashed, true);
  const live = await call(mock, 'POST', '/api/search/metadata', { key, body: typedBody(null) });
  assert.deepEqual(
    live.body.assets.items.map((item: { originalFileName: string }) => item.originalFileName),
    ['b.jpg'],
  );
  const lost = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(null, {}, { trashedAt: { ne: null }, isOffline: { eq: true } }),
  });
  assert.equal(lost.body.assets.items.length, 1);
  const [library] = mock.state.libraries();
  assert.equal(library?.assetCount, 1);

  writeFileSync(join(dir, 'a.jpg'), 'a again');
  const [back] = await mock.control.scan();
  assert.equal(back?.summary?.restored, 1);
  const restored = mock.state.assetByPath('/mnt/photo/a.jpg');
  assert.equal(restored?.isOffline, false);
  assert.equal(restored?.deletedAt, null);
  assert.equal(restored?.id, original?.id);
  assert.ok(String(restored?.updatedAt) > String(offline?.updatedAt));
});

test('a moved file is one offline asset and one new asset', async (t) => {
  const { mock, dir } = await mounted(t, { 'old/a.jpg': 'a' });
  await mock.control.scan();
  const before = mock.state.assetByPath('/mnt/photo/old/a.jpg');
  mkdirSync(join(dir, 'new'));
  renameSync(join(dir, 'old/a.jpg'), join(dir, 'new/a.jpg'));
  const [job] = await mock.control.scan();
  assert.equal(job?.summary?.imported, 1);
  assert.equal(job?.summary?.offlined, 1);
  const old = mock.state.assetByPath('/mnt/photo/old/a.jpg');
  const moved = mock.state.assetByPath('/mnt/photo/new/a.jpg');
  assert.equal(old?.isOffline, true);
  assert.equal(old?.id, before?.id);
  assert.equal(moved?.isOffline, false);
  assert.notEqual(moved?.id, before?.id);
});

test('a link to a file is a file; a link to a directory, a loop and a dangling link are not', async (t) => {
  const { mock, dir } = await mounted(t, { 'real/a.jpg': 'a', 'real/sub/b.jpg': 'b' });
  mkdirSync(join(dir, 'links'));
  symlinkSync('../real/a.jpg', join(dir, 'links/to_file.jpg'));
  symlinkSync('../real', join(dir, 'links/to_dir'));
  symlinkSync('..', join(dir, 'links/up'));
  symlinkSync('../missing.jpg', join(dir, 'links/dangling.jpg'));
  symlinkSync('loop_b.jpg', join(dir, 'links/loop_a.jpg'));
  symlinkSync('loop_a.jpg', join(dir, 'links/loop_b.jpg'));
  const [job] = await mock.control.scan();
  assert.equal(job?.status, 'completed', job?.error ?? '');
  assert.deepEqual(paths(mock), [
    '/mnt/photo/links/to_file.jpg',
    '/mnt/photo/real/a.jpg',
    '/mnt/photo/real/sub/b.jpg',
  ]);
});

test('a directory that cannot be listed is skipped, but the import path itself must be listable', {
  skip: process.getuid?.() === 0 ? 'root can list every directory' : false,
}, async (t) => {
  const { mock, dir } = await mounted(t, { 'open/a.jpg': 'a', 'b.jpg': 'b' });
  const locked = join(dir, 'locked');
  mkdirSync(locked);
  chmodSync(locked, 0o000);
  try {
    const [skipped] = await mock.control.scan();
    assert.equal(skipped?.status, 'completed', skipped?.error ?? '');
    assert.deepEqual(paths(mock), ['/mnt/photo/b.jpg', '/mnt/photo/open/a.jpg']);

    chmodSync(dir, 0o000);
    const [refused] = await mock.control.scan();
    assert.equal(refused?.status, 'failed');
    assert.match(refused?.error ?? '', /EACCES/);
    assert.equal(mock.state.assets().filter((asset) => asset.isOffline).length, 0);
  } finally {
    chmodSync(dir, 0o700);
    chmodSync(locked, 0o700);
  }
});

test('the exclusion patterns keep files and directories out of the scan', async (t) => {
  const { mock } = await mounted(t, {
    '2019/@eaDir/IMG_1.jpg/SYNOPHOTO_THUMB.jpg': 'x',
    '2019/IMG_1.jpg': 'a',
    'skip/this/b.jpg': 'b',
    'keep/c.jpg': 'c',
    'keep/c_thumb.jpg': 'thumb',
    'keep/d.png': 'd',
  });
  const { key } = await connect(mock);
  const [library] = mock.state.libraries();
  assert.deepEqual(library?.exclusionPatterns, ['**/@eaDir/**']);
  await mock.control.scan();
  assert.deepEqual(paths(mock), [
    '/mnt/photo/2019/IMG_1.jpg',
    '/mnt/photo/keep/c.jpg',
    '/mnt/photo/keep/c_thumb.jpg',
    '/mnt/photo/keep/d.png',
    '/mnt/photo/skip/this/b.jpg',
  ]);

  const patterns = ['**/@eaDir/**', '**/skip/**', '**/*_thumb.jpg', '**/.opm-trash/**'];
  const update = await call(mock, 'PUT', `/api/libraries/${library?.id}`, {
    key,
    body: { exclusionPatterns: patterns },
  });
  assert.equal(update.status, 200);
  const [job] = await mock.control.scan();
  assert.equal(job?.summary?.offlined, 2);
  assert.equal(mock.state.assetByPath('/mnt/photo/skip/this/b.jpg')?.isOffline, true);
  assert.equal(mock.state.assetByPath('/mnt/photo/keep/c_thumb.jpg')?.isOffline, true);
  assert.equal(mock.state.assetByPath('/mnt/photo/keep/c.jpg')?.isOffline, false);
  assert.equal(mock.state.assetByPath('/mnt/photo/2019/IMG_1.jpg')?.isOffline, false);
});

test('import paths that no mount covers are left alone', async (t) => {
  const { mock } = await mounted(t, { 'a.jpg': 'a' });
  mock.control.addAssets([{ path: '/mnt/video/clip.mp4', library: 'Videos' }]);
  const jobs = await mock.control.scan();
  const videos = jobs.find((job) => job.summary?.unmounted.length === 1);
  assert.deepEqual(videos?.summary?.unmounted, ['/mnt/video']);
  assert.equal(videos?.summary?.offlined, 0);
  assert.equal(mock.state.assetByPath('/mnt/video/clip.mp4')?.isOffline, false);
  assert.equal(mock.state.assetByPath('/mnt/photo/a.jpg')?.isOffline, false);
});

test('a scan of an unknown library is refused', async (t) => {
  const { mock } = await mounted(t, { 'a.jpg': 'a' });
  await assert.rejects(mock.control.scan('Nowhere'), /unknown library "Nowhere"/);
  const reply = await call(mock, 'POST', '/__mock/scan', { body: { libraryId: 'Nowhere' } });
  assert.equal(reply.status, 400);
  const byName = await call(mock, 'POST', '/__mock/scan', { body: { libraryId: 'Photos' } });
  assert.equal(byName.status, 200);
  assert.equal(byName.body.jobs.length, 1);
  assert.equal(byName.body.jobs[0].status, 'completed');
});

test('scans of one library run one after the other', async (t) => {
  const dir = scratchDir(t, 'scan-serial');
  writeTree(dir, { 'a.jpg': 'a' });
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir }, scanDelayMs: 120 });
  const { key } = await connect(mock);
  const [library] = mock.state.libraries();
  for (let i = 0; i < 3; i += 1) {
    const reply = await call(mock, 'POST', `/api/libraries/${library?.id}/scan`, { key });
    assert.equal(reply.status, 204);
  }
  await eventually(() =>
    assert.deepEqual(
      mock.state.jobs().map((job) => job.status),
      ['completed', 'completed', 'completed'],
    ),
  );
  const jobs = mock.state.jobs();
  for (let i = 1; i < jobs.length; i += 1) {
    const previous = jobs[i - 1];
    const current = jobs[i];
    assert.ok(
      Date.parse(current?.startedAt ?? '') >= Date.parse(previous?.finishedAt ?? ''),
      `scan ${i + 1} started before scan ${i} ended`,
    );
  }
  assert.equal(jobs[0]?.summary?.imported, 1);
  assert.equal(jobs[1]?.summary?.unchanged, 1);
  assert.equal(jobs[2]?.summary?.unchanged, 1);
});

test('a scan sees the disk when it runs, not when it was requested', async (t) => {
  const dir = scratchDir(t, 'scan-late');
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir }, scanDelayMs: 150 });
  const { key } = await connect(mock);
  const [library] = mock.state.libraries();
  await call(mock, 'POST', `/api/libraries/${library?.id}/scan`, { key });
  writeFileSync(join(dir, 'late.jpg'), 'arrived after the request');
  await eventually(() => assert.equal(mock.state.jobs()[0]?.status, 'completed'));
  assert.deepEqual(paths(mock), ['/mnt/photo/late.jpg']);
});

test('a reset cancels the scans in flight', async (t) => {
  const dir = scratchDir(t, 'scan-reset');
  writeTree(dir, { 'a.jpg': 'a' });
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir }, scanDelayMs: 100 });
  const { key } = await connect(mock);
  const [library] = mock.state.libraries();
  await call(mock, 'POST', `/api/libraries/${library?.id}/scan`, { key });
  mock.control.reset();
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.deepEqual(mock.state.assets(), []);
  assert.deepEqual(mock.state.jobs(), []);
});
