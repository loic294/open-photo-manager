import assert from 'node:assert/strict';
import { existsSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { scenarioFiles } from '../index.ts';
import { call, connect, scratchDir, startMock, USER, writeTree } from './helpers.ts';

const MISSING_ID = '00000000-0000-4000-8000-000000000042';

async function withAssets(t: Parameters<typeof startMock>[0], options = {}) {
  const mock = await startMock(t, options);
  const { items } = mock.control.addAssets([
    { path: '/mnt/photo/a.jpg', fileCreatedAt: '2024-01-01T00:00:00.000Z' },
    { path: '/mnt/photo/b.jpg', fileCreatedAt: '2024-01-02T00:00:00.000Z' },
    { path: '/mnt/photo/c.jpg', fileCreatedAt: '2024-01-03T00:00:00.000Z' },
  ]);
  const { key } = await connect(mock);
  const ids = items.map((item) => item.id);
  return { mock, key, ids };
}

test('an asset is fetched with its details, and a missing one is the "not found" 400', async (t) => {
  const { mock, key, ids } = await withAssets(t);
  const reply = await call(mock, 'GET', `/api/assets/${ids[0]}`, { key });
  assert.equal(reply.status, 200);
  assert.equal(reply.body.id, ids[0]);
  assert.equal(reply.body.originalPath, '/mnt/photo/a.jpg');
  assert.equal(reply.body.type, 'IMAGE');
  assert.equal(reply.body.isOffline, false);
  assert.equal(reply.body.isTrashed, false);
  assert.deepEqual(reply.body.tags, []);
  assert.equal(typeof reply.body.exifInfo, 'object');

  const unknown = await call(mock, 'GET', `/api/assets/${MISSING_ID}`, { key });
  assert.equal(unknown.status, 400);
  assert.equal(unknown.body.message, 'Not found or no asset.read access');
  const malformed = await call(mock, 'GET', '/api/assets/nope', { key });
  assert.equal(malformed.status, 400);
  assert.equal(malformed.body.message, 'id must be a UUID');
});

test('the status of an unknown asset can be 404, as other Immich versions answer', async (t) => {
  const scenario = JSON.parse(readFileSync(scenarioFiles.default, 'utf8'));
  scenario.options = { unknownAssetStatus: 404 };
  const mock = await startMock(t, { scenario });
  const { key } = await connect(mock);
  const reply = await call(mock, 'GET', `/api/assets/${MISSING_ID}`, { key });
  assert.equal(reply.status, 404);
  assert.equal(reply.body.statusCode, 404);
});

test('locked assets and the assets of other accounts look missing', async (t) => {
  const { mock, ids } = await withAssets(t);
  mock.control.addAssets([{ path: '/mnt/photo/locked.jpg', visibility: 'locked' }]);
  const locked = mock.state.assetByPath('/mnt/photo/locked.jpg');
  const admin = await connect(mock);
  const lockedReply = await call(mock, 'GET', `/api/assets/${locked?.id}`, { key: admin.key });
  assert.equal(lockedReply.status, 400);
  const regular = await connect(mock, { account: USER });
  const foreign = await call(mock, 'GET', `/api/assets/${ids[0]}`, { key: regular.key });
  assert.equal(foreign.status, 400);
});

test('a bulk update changes the fields given, moves updatedAt and is all or nothing', async (t) => {
  const { mock, key, ids } = await withAssets(t);
  const before = await call(mock, 'GET', `/api/assets/${ids[0]}`, { key });
  const reply = await call(mock, 'PUT', '/api/assets', {
    key,
    body: {
      ids: [ids[0], ids[1]],
      isFavorite: true,
      rating: 4,
      visibility: 'archive',
      description: 'to print',
    },
  });
  assert.equal(reply.status, 204);
  for (const id of [ids[0], ids[1]]) {
    const asset = await call(mock, 'GET', `/api/assets/${id}`, { key });
    assert.equal(asset.body.isFavorite, true);
    assert.equal(asset.body.visibility, 'archive');
    assert.equal(asset.body.isArchived, true);
    assert.equal(asset.body.exifInfo.rating, 4);
    assert.equal(asset.body.exifInfo.description, 'to print');
    assert.ok(asset.body.updatedAt > before.body.updatedAt);
  }
  const untouched = await call(mock, 'GET', `/api/assets/${ids[2]}`, { key });
  assert.equal(untouched.body.isFavorite, false);

  const cleared = await call(mock, 'PUT', '/api/assets', {
    key,
    body: { ids: [ids[0]], rating: null },
  });
  assert.equal(cleared.status, 204);
  assert.equal(mock.state.assetByPath('/mnt/photo/a.jpg')?.rating, null);

  const partial = await call(mock, 'PUT', '/api/assets', {
    key,
    body: { ids: [ids[2], MISSING_ID], isFavorite: true },
  });
  assert.equal(partial.status, 400);
  assert.equal(partial.body.message, 'Not found or no asset.update access');
  assert.equal(mock.state.assetByPath('/mnt/photo/c.jpg')?.visibility, 'timeline');
  assert.equal((await call(mock, 'GET', `/api/assets/${ids[2]}`, { key })).body.isFavorite, false);
});

test('a bulk update refuses bad fields', async (t) => {
  const { mock, key, ids } = await withAssets(t);
  const bad = async (body: unknown) => {
    const reply = await call(mock, 'PUT', '/api/assets', { key, body });
    assert.equal(reply.status, 400, JSON.stringify(reply.body));
    return JSON.stringify(reply.body.message);
  };
  assert.match(await bad({ ids: [ids[0]], rating: 6 }), /rating/);
  assert.match(await bad({ ids: [ids[0]], visibility: 'secret' }), /visibility/);
  assert.match(await bad({ ids: ['nope'] }), /ids/);
  assert.match(await bad({ ids: [ids[0]], colour: 'red' }), /property colour should not exist/);
  assert.match(await bad({}), /ids/);
});

test('a soft delete sends the asset to the trash and keeps the file', async (t) => {
  const dir = scratchDir(t, 'assets-soft');
  writeTree(dir, { 'a.jpg': 'a', 'b.jpg': 'b' });
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir } });
  await mock.control.scan('Photos');
  const { key } = await connect(mock);
  const a = mock.state.assetByPath('/mnt/photo/a.jpg');

  const reply = await call(mock, 'DELETE', '/api/assets', {
    key,
    body: { ids: [a?.id], force: false },
  });
  assert.equal(reply.status, 204);
  const after = mock.state.assetByPath('/mnt/photo/a.jpg');
  assert.notEqual(after?.deletedAt, null);
  assert.equal(after?.isOffline, false);
  assert.ok(existsSync(join(dir, 'a.jpg')));
  assert.deepEqual(mock.state.violations(), []);
  const view = await call(mock, 'GET', `/api/assets/${a?.id}`, { key });
  assert.equal(view.body.isTrashed, true);
  assert.equal(view.body.deletedAt, undefined);

  const noForce = await call(mock, 'DELETE', '/api/assets', { key, body: { ids: [after?.id] } });
  assert.equal(noForce.status, 204);
  assert.ok(existsSync(join(dir, 'a.jpg')));
  assert.deepEqual(mock.state.violations(), []);
});

test('a delete is all or nothing when one id is out of reach', async (t) => {
  const { mock, key, ids } = await withAssets(t);
  const reply = await call(mock, 'DELETE', '/api/assets', {
    key,
    body: { ids: [ids[0], MISSING_ID], force: true },
  });
  assert.equal(reply.status, 400);
  assert.equal(reply.body.message, 'Not found or no asset.delete access');
  assert.equal(mock.state.assets().length, 3);
  assert.deepEqual(mock.state.violations(), []);
});

test('a forced delete of an online asset is a violation and removes the real file', async (t) => {
  const dir = scratchDir(t, 'assets-force');
  writeTree(dir, { 'a.jpg': 'a', 'b.jpg': 'b' });
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir } });
  await mock.control.scan();
  const { key } = await connect(mock);
  const a = mock.state.assetByPath('/mnt/photo/a.jpg');
  const b = mock.state.assetByPath('/mnt/photo/b.jpg');

  const reply = await call(mock, 'DELETE', '/api/assets', {
    key,
    body: { ids: [a?.id], force: true },
  });
  assert.equal(reply.status, 204);
  assert.equal(existsSync(join(dir, 'a.jpg')), false);
  assert.ok(existsSync(join(dir, 'b.jpg')));
  const violations = mock.state.violations();
  assert.equal(violations.length, 1);
  assert.equal(violations[0]?.kind, 'online-asset-force-deleted');
  assert.equal(violations[0]?.assetId, a?.id);
  assert.equal(violations[0]?.path, '/mnt/photo/a.jpg');
  assert.equal(violations[0]?.libraryId, a?.libraryId);
  assert.equal(violations[0]?.fileRemoved, true);
  assert.equal(mock.state.assetByPath('/mnt/photo/a.jpg'), undefined);
  assert.equal(mock.state.assetByPath('/mnt/photo/b.jpg')?.id, b?.id);

  const snapshot = await call(mock, 'GET', '/__mock/state');
  assert.equal(snapshot.body.violations.length, 1);
});

test('a forced delete of an offline asset is what the application may do: no violation', async (t) => {
  const dir = scratchDir(t, 'assets-offline');
  writeTree(dir, { 'a.jpg': 'a', 'b.jpg': 'b' });
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir } });
  await mock.control.scan();
  const { key } = await connect(mock);
  const a = mock.state.assetByPath('/mnt/photo/a.jpg');
  rmSync(join(dir, 'a.jpg'));
  await mock.control.scan();
  assert.equal(mock.state.assetByPath('/mnt/photo/a.jpg')?.isOffline, true);

  const reply = await call(mock, 'DELETE', '/api/assets', {
    key,
    body: { ids: [a?.id], force: true },
  });
  assert.equal(reply.status, 204);
  assert.deepEqual(mock.state.violations(), []);
  assert.equal(mock.state.assetByPath('/mnt/photo/a.jpg'), undefined);
  assert.ok(existsSync(join(dir, 'b.jpg')));
});

test('a forced delete of an online asset with no mount is still a violation', async (t) => {
  const { mock, key, ids } = await withAssets(t);
  const reply = await call(mock, 'DELETE', '/api/assets', {
    key,
    body: { ids: [ids[1]], force: true },
  });
  assert.equal(reply.status, 204);
  const [violation] = mock.state.violations();
  assert.equal(violation?.fileRemoved, false);
  assert.equal(violation?.path, '/mnt/photo/b.jpg');
});

test('a forced delete also removes the asset from its albums and tags', async (t) => {
  const { mock, key, ids } = await withAssets(t);
  const [album] = mock.state.albums();
  const [tag] = mock.state.tags();
  await call(mock, 'PUT', `/api/albums/${album?.id}/assets`, { key, body: { ids: [ids[0]] } });
  await call(mock, 'PUT', '/api/tags/assets', {
    key,
    body: { assetIds: [ids[0]], tagIds: [tag?.id] },
  });
  assert.equal(mock.state.albumsOf(ids[0] ?? '').length, 1);
  await call(mock, 'DELETE', '/api/assets', { key, body: { ids: [ids[0]], force: true } });
  assert.equal(mock.state.albumsOf(ids[0] ?? '').length, 0);
  assert.deepEqual(mock.state.tags()[0]?.assetIds, []);
});

test('albums are listed by asset id and assets are added with one result each', async (t) => {
  const { mock, key, ids } = await withAssets(t);
  const albums = mock.state.albums();
  const holidays = albums.find((album) => album.name === 'Holidays');
  const family = albums.find((album) => album.name === 'Family');

  const all = await call(mock, 'GET', '/api/albums', { key });
  assert.equal(all.status, 200);
  assert.deepEqual(all.body.map((album: { albumName: string }) => album.albumName).sort(), [
    'Family',
    'Holidays',
    'To print',
  ]);
  assert.equal(all.body[0].assetCount, 0);

  const added = await call(mock, 'PUT', `/api/albums/${holidays?.id}/assets`, {
    key,
    body: { ids: [ids[0], ids[1], MISSING_ID] },
  });
  assert.equal(added.status, 200);
  assert.deepEqual(added.body, [
    { id: ids[0], success: true },
    { id: ids[1], success: true },
    { id: MISSING_ID, success: false, error: 'no_permission' },
  ]);
  const again = await call(mock, 'PUT', `/api/albums/${holidays?.id}/assets`, {
    key,
    body: { ids: [ids[0], ids[2]] },
  });
  assert.deepEqual(again.body, [
    { id: ids[0], success: false, error: 'duplicate' },
    { id: ids[2], success: true },
  ]);

  const mine = await call(mock, 'GET', `/api/albums?assetId=${ids[0]}`, { key });
  assert.deepEqual(
    mine.body.map((album: { id: string }) => album.id),
    [holidays?.id],
  );
  const none = await call(mock, 'GET', `/api/albums?assetId=${MISSING_ID}`, { key });
  assert.deepEqual(none.body, []);
  await call(mock, 'PUT', `/api/albums/${family?.id}/assets`, { key, body: { ids: [ids[0]] } });
  const both = await call(mock, 'GET', `/api/albums?assetId=${ids[0]}`, { key });
  assert.equal(both.body.length, 2);

  const counted = await call(mock, 'GET', '/api/albums', { key });
  assert.equal(
    counted.body.find((album: { id: string }) => album.id === holidays?.id).assetCount,
    3,
  );
  assert.deepEqual(
    mock.state
      .albumsOf(ids[0] ?? '')
      .map((album) => album.name)
      .sort(),
    ['Family', 'Holidays'],
  );

  const unknown = await call(mock, 'PUT', `/api/albums/${MISSING_ID}/assets`, {
    key,
    body: { ids: [ids[0]] },
  });
  assert.equal(unknown.status, 400);
  const malformed = await call(mock, 'GET', '/api/albums?assetId=nope', { key });
  assert.equal(malformed.status, 400);
});

test('tags are listed and attached, and the count is tags times assets', async (t) => {
  const { mock, key, ids } = await withAssets(t);
  const listed = await call(mock, 'GET', '/api/tags', { key });
  assert.equal(listed.status, 200);
  const birthdays = listed.body.find((tag: { value: string }) => tag.value === 'Family/Birthdays');
  assert.equal(birthdays.name, 'Birthdays');
  const travel = listed.body.find((tag: { value: string }) => tag.value === 'Travel');
  assert.equal(travel.name, 'Travel');

  const tagged = await call(mock, 'PUT', '/api/tags/assets', {
    key,
    body: { assetIds: ids, tagIds: [travel.id, birthdays.id] },
  });
  assert.equal(tagged.status, 200);
  assert.deepEqual(tagged.body, { count: 6 });
  const unknownTag = await call(mock, 'PUT', '/api/tags/assets', {
    key,
    body: { assetIds: ids, tagIds: [travel.id, MISSING_ID] },
  });
  assert.deepEqual(unknownTag.body, { count: 3 });
  assert.equal(mock.state.tags().find((tag) => tag.value === 'Travel')?.assetIds.length, 3);

  const asset = await call(mock, 'GET', `/api/assets/${ids[0]}`, { key });
  assert.deepEqual(asset.body.tags.map((tag: { value: string }) => tag.value).sort(), [
    'Family/Birthdays',
    'Travel',
  ]);
  const bad = await call(mock, 'PUT', '/api/tags/assets', {
    key,
    body: { assetIds: ['x'], tagIds: [] },
  });
  assert.equal(bad.status, 400);
});
