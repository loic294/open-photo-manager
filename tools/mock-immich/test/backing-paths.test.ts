import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { BackingAssetPath, MockImmich } from '../index.ts';
import { call, connect, startMock } from './helpers.ts';

test('a complete backing snapshot updates API paths without changing asset identities', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.addAssets([
    { path: '/mnt/photo/old.jpg', isFavorite: true },
    { path: '/mnt/photo/other.jpg' },
  ]);
  const rows = mock.state.assets().map(
    (asset): BackingAssetPath => ({
      id: String(asset.id),
      ownerId: String(asset.ownerId),
      libraryId: String(asset.libraryId),
      originalPath: String(asset.originalPath),
      originalFileName: String(asset.originalFileName),
      checksum: String(asset.checksum),
      updatedAt: String(asset.updatedAt),
    }),
  );
  const first = rows[0];
  const second = rows[1];
  assert.ok(first && second);
  const album = mock.state.albums()[0];
  const tag = mock.state.tags()[0];
  assert.ok(album && tag);
  assert.equal(
    (await call(mock, 'PUT', `/api/albums/${album.id}/assets`, { key, body: { ids: [first.id] } }))
      .status,
    200,
  );
  assert.equal(
    (
      await call(mock, 'PUT', '/api/tags/assets', {
        key,
        body: { assetIds: [first.id], tagIds: [tag.id] },
      })
    ).status,
    200,
  );
  const changed = {
    ...first,
    originalPath: '/mnt/photo/new.jpg',
    originalFileName: 'new.jpg',
    checksum: 'new-database-checksum',
    updatedAt: '2026-11-08T12:00:00.000Z',
  };
  for (const invalid of [
    [changed],
    [changed, changed],
    [changed, { ...second, ownerId: first.id }],
    [changed, { ...second, originalPath: changed.originalPath, originalFileName: 'new.jpg' }],
    [changed, { ...second, updatedAt: 'not-a-date' }],
  ]) {
    assert.throws(() => mock.control.reconcileAssetPaths(invalid));
    assert.equal(mock.state.assetByPath('/mnt/photo/old.jpg')?.id, first.id);
    assert.equal(mock.state.assetByPath('/mnt/photo/new.jpg'), undefined);
  }
  mock.control.reconcileAssetPaths([changed, second]);
  assert.equal(mock.state.assetByPath('/mnt/photo/old.jpg'), undefined);
  const asset = mock.state.assetByPath('/mnt/photo/new.jpg');
  assert.equal(asset?.id, first.id);
  assert.equal(asset?.isFavorite, true);
  assert.equal(asset?.checksum, changed.checksum);
  const response = await call(mock, 'GET', `/api/assets/${first.id}`, { key });
  assert.equal(response.status, 200);
  assert.equal(response.body.originalPath, changed.originalPath);
  assert.equal(response.body.originalFileName, changed.originalFileName);
  assert.equal(response.body.checksum, changed.checksum);
  assert.equal(response.body.updatedAt, changed.updatedAt);
  assert.deepEqual(mock.state.albumsOf(first.id), [{ id: album.id, name: album.name }]);
  assert.deepEqual(mock.state.tags()[0]?.assetIds, [first.id]);
  assert.deepEqual(mock.state.violations(), []);
});

test('backing refresh runs before asset reads and failures cannot return stale results', async (t) => {
  let mock: MockImmich;
  let rows: BackingAssetPath[] = [];
  let fail = false;
  mock = await startMock(t, {
    beforeAssetRead: async () => {
      if (fail) throw new Error('test backing unavailable');
      mock.control.reconcileAssetPaths(rows);
    },
  });
  const { key } = await connect(mock);
  mock.control.addAssets([{ path: '/mnt/photo/old.jpg' }]);
  rows = mock.state.assets().map((asset) => ({
    id: String(asset.id),
    ownerId: String(asset.ownerId),
    libraryId: String(asset.libraryId),
    originalPath: '/mnt/photo/new.jpg',
    originalFileName: 'new.jpg',
    checksum: String(asset.checksum),
    updatedAt: String(asset.updatedAt),
  }));
  const id = rows[0]?.id;
  assert.ok(id);
  const result = await call(mock, 'GET', `/api/assets/${id}`, { key });
  assert.equal(result.status, 200);
  assert.equal(result.body.originalPath, '/mnt/photo/new.jpg');
  fail = true;
  const rejected = await call(mock, 'GET', `/api/assets/${id}`, { key });
  assert.equal(rejected.status, 500);
});
