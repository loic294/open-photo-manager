import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { BackingAssetPath, MockImmich } from '../index.ts';
import { call, connect, scratchDir, startMock, writeTree } from './helpers.ts';

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
      isOffline: Boolean(asset.isOffline),
      isFavorite: Boolean(asset.isFavorite),
      rating: asset.rating === null ? null : Number(asset.rating),
      description: String(asset.description),
      visibility: 'timeline',
      deletedAt: asset.deletedAt === null ? null : String(asset.deletedAt),
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
    isOffline: true,
    deletedAt: '2026-11-08T11:00:00.000Z',
  };
  for (const invalid of [
    [changed],
    [changed, changed],
    [changed, { ...second, ownerId: first.id }],
    [changed, { ...second, originalPath: changed.originalPath, originalFileName: 'new.jpg' }],
    [changed, { ...second, updatedAt: 'not-a-date' }],
    [changed, { ...second, deletedAt: 'not-a-date' }],
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
  assert.equal(asset?.isOffline, true);
  assert.equal(asset?.deletedAt, changed.deletedAt);
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

test('association views refresh the backing asset state before calculating counts', async (t) => {
  let mock: MockImmich;
  let rows: BackingAssetPath[] = [];
  mock = await startMock(t, {
    beforeAssetRead: async () => mock.control.reconcileAssetPaths(rows),
  });
  const { key } = await connect(mock);
  const added = mock.control.addAssets([{ path: '/mnt/photo/old.jpg' }]);
  const id = added.items[0]?.id;
  const album = mock.state.albums()[0];
  assert.ok(id && album);
  const attached = await call(mock, 'PUT', `/api/albums/${album.id}/assets`, {
    key,
    body: { ids: [id] },
  });
  assert.equal(attached.status, 200);
  rows = mock.state.assets().map((asset) => ({
    id: String(asset.id),
    ownerId: String(asset.ownerId),
    libraryId: String(asset.libraryId),
    originalPath: String(asset.originalPath),
    originalFileName: String(asset.originalFileName),
    checksum: String(asset.checksum),
    updatedAt: String(asset.updatedAt),
    isOffline: true,
    isFavorite: Boolean(asset.isFavorite),
    rating: asset.rating === null ? null : Number(asset.rating),
    description: String(asset.description),
    visibility: 'timeline',
    deletedAt: '2026-11-08T11:00:00.000Z',
  }));
  const response = await call(mock, 'GET', `/api/albums?assetId=${id}`, { key });
  assert.equal(response.status, 200);
  assert.equal(response.body[0].assetCount, 0);
  assert.equal(mock.state.assetByPath('/mnt/photo/old.jpg')?.isOffline, true);
});

test('backing associations and favorite state are validated before publishing a snapshot', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.addAssets([{ path: '/mnt/photo/old.jpg' }]);
  const asset = mock.state.assets()[0];
  assert.ok(asset);
  const id = String(asset.id);
  const rows: BackingAssetPath[] = [
    {
      id,
      ownerId: String(asset.ownerId),
      libraryId: String(asset.libraryId),
      originalPath: '/mnt/photo/new.jpg',
      originalFileName: 'new.jpg',
      checksum: String(asset.checksum),
      updatedAt: String(asset.updatedAt),
      isOffline: false,
      isFavorite: true,
      rating: 5,
      description: 'fixture description',
      visibility: 'archive',
      deletedAt: null,
    },
  ];
  const albums = mock.state.albums().map((album) => ({
    id: album.id,
    name: album.name,
    assetIds: [id],
  }));
  const tags = mock.state.tags().map((tag) => ({
    id: tag.id,
    name: tag.value,
    assetIds: [id],
  }));
  assert.ok(albums[0] && tags[0]);
  const badTags = [{ ...tags[0], assetIds: ['unknown'] }, ...tags.slice(1)];
  assert.throws(() => mock.control.reconcileAssetPaths(rows, { albums, tags: badTags }));
  assert.equal(mock.state.assetByPath('/mnt/photo/old.jpg')?.isFavorite, false);
  assert.deepEqual(mock.state.albumsOf(id), []);
  mock.control.reconcileAssetPaths(rows, { albums, tags });
  assert.equal(mock.state.assetByPath('/mnt/photo/new.jpg')?.isFavorite, true);
  assert.equal(mock.state.assetByPath('/mnt/photo/new.jpg')?.rating, 5);
  assert.equal(mock.state.assetByPath('/mnt/photo/new.jpg')?.description, 'fixture description');
  assert.equal(mock.state.assetByPath('/mnt/photo/new.jpg')?.visibility, 'archive');
  assert.equal(mock.state.albumsOf(id).length, albums.length);
  const response = await call(mock, 'GET', `/api/assets/${id}`, { key });
  assert.equal(response.body.isFavorite, true);
  assert.equal(response.body.tags.length, tags.length);
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
    isOffline: Boolean(asset.isOffline),
    isFavorite: Boolean(asset.isFavorite),
    rating: asset.rating === null ? null : Number(asset.rating),
    description: String(asset.description),
    visibility: 'timeline',
    deletedAt: asset.deletedAt === null ? null : String(asset.deletedAt),
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

test('unsupported writes are refused rather than diverging from the backing database', async (t) => {
  let active = false;
  const mock = await startMock(t, { readOnlyBacking: () => active });
  const { key } = await connect(mock);
  const added = mock.control.addAssets([{ path: '/mnt/photo/old.jpg' }]);
  active = true;
  const before = mock.state.assets();
  for (const [method, path, body] of [
    ['DELETE', '/api/assets', { ids: [added.items[0]?.id], force: true }],
    ['PUT', '/api/assets', { ids: [added.items[0]?.id], isFavorite: true }],
    ['PUT', '/api/assets/', { ids: [added.items[0]?.id], isFavorite: true }],
    ['POST', `/api/libraries/${before[0]?.libraryId}/scan`, {}],
    ['POST', `/api/libraries/${before[0]?.libraryId}/scan/`, {}],
  ] as const) {
    const response = await call(mock, method, path, { key, body });
    assert.equal(response.status, 503);
  }
  await assert.rejects(() => mock.control.scan(), /does not support mock mutations/);
  assert.throws(() => mock.control.addAssets([{ path: '/mnt/photo/new.jpg' }]), /backing store/);
  assert.deepEqual(mock.state.assets(), before);
  assert.deepEqual(mock.state.violations(), []);
});

test('backed deletion is serialized with reads and failed writes leave the mock unchanged', async (t) => {
  let active = false;
  let fail = true;
  let release: (() => void) | undefined;
  let started: (() => void) | undefined;
  const entered = new Promise<void>((resolve) => {
    started = resolve;
  });
  const blocked = new Promise<void>((resolve) => {
    release = resolve;
  });
  const mock = await startMock(t, {
    readOnlyBacking: () => active,
    beforeAssetRead: async () => {},
    beforeAssetDelete: async () => {
      if (fail) throw new Error('test backing unavailable');
      started?.();
      await blocked;
    },
  });
  const { key } = await connect(mock);
  const added = mock.control.addAssets([{ path: '/mnt/photo/old.jpg', isOffline: true }]);
  const id = added.items[0]?.id;
  assert.ok(id);
  active = true;
  assert.equal(
    (await call(mock, 'DELETE', '/api/assets', { key, body: { ids: [id], force: true } })).status,
    500,
  );
  assert.equal(mock.state.assetByPath('/mnt/photo/old.jpg')?.id, id);
  fail = false;
  const deleting = call(mock, 'DELETE', '/api/assets', { key, body: { ids: [id], force: true } });
  await entered;
  const reading = call(mock, 'GET', `/api/assets/${id}`, { key });
  release?.();
  assert.equal((await deleting).status, 204);
  assert.equal((await reading).status, 400);
  assert.deepEqual(mock.state.violations(), []);
});

test('a backed scan publishes its changes only after the backing commit succeeds', async (t) => {
  const photos = scratchDir(t, 'backed-scan');
  writeTree(photos, { 'new.jpg': 'new' });
  let active = false;
  let fail = true;
  let proposed: string[] = [];
  const mock = await startMock(t, {
    mounts: { '/mnt/photo': photos },
    readOnlyBacking: () => active,
    beforeAssetRead: async () => {},
    beforeScanCommit: async ({ after }) => {
      proposed = after.map((asset) => asset.originalPath);
      if (fail) throw new Error('test backing unavailable');
    },
  });
  mock.control.addAssets([{ path: '/mnt/photo/missing.jpg', trashed: false }]);
  active = true;
  const failed = await mock.control.scan('Photos');
  assert.equal(failed[0]?.status, 'failed');
  assert.match(failed[0]?.error ?? '', /test backing unavailable/);
  assert.deepEqual(proposed.sort(), ['/mnt/photo/missing.jpg', '/mnt/photo/new.jpg']);
  assert.equal(mock.state.assetByPath('/mnt/photo/missing.jpg')?.isOffline, false);
  assert.equal(mock.state.assetByPath('/mnt/photo/new.jpg'), undefined);
  fail = false;
  const succeeded = await mock.control.scan('Photos');
  assert.equal(succeeded[0]?.status, 'completed');
  assert.equal(mock.state.assetByPath('/mnt/photo/missing.jpg')?.isOffline, true);
  assert.ok(mock.state.assetByPath('/mnt/photo/new.jpg'));
});

test('a backed scan serializes asset reads until its commit is published', async (t) => {
  const photos = scratchDir(t, 'held-scan');
  writeTree(photos, { 'new.jpg': 'new' });
  let active = false;
  let release: (() => void) | undefined;
  let entered: (() => void) | undefined;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const started = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const mock = await startMock(t, {
    mounts: { '/mnt/photo': photos },
    readOnlyBacking: () => active,
    beforeAssetRead: async () => {},
    beforeScanCommit: async () => {
      entered?.();
      await held;
    },
  });
  const { key } = await connect(mock);
  const asset = mock.control.addAssets([{ path: '/mnt/photo/missing.jpg' }]).items[0];
  assert.ok(asset);
  active = true;
  const scanning = mock.control.scan('Photos');
  await started;
  const reading = call(mock, 'GET', `/api/assets/${asset.id}`, { key });
  release?.();
  assert.equal((await scanning)[0]?.status, 'completed');
  const response = await reading;
  assert.equal(response.status, 200);
  assert.equal(response.body.isOffline, true);
  assert.notEqual(response.body.deletedAt, null);
});
