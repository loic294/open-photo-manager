import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { type MockImmich, scenarioFiles, startMockImmich } from '../../tools/mock-immich/index.ts';
import { ImmichPostgres } from './immich-postgres.ts';

test('real v3.3.3 SQL path updates are visible through the mock API', {
  timeout: 300_000,
}, async (t) => {
  const database = await ImmichPostgres.start();
  t.after(() => database.stop());
  let mock: MockImmich;
  let seeded = false;
  mock = await startMockImmich({
    scenario: scenarioFiles.fixtures,
    serverVersion: '3.3.3',
    beforeAssetRead: () => database.refreshMock(mock),
    readOnlyBacking: () => seeded,
  });
  t.after(() => mock.stop());
  mock.control.addAssets([{ path: '/mnt/photos/old.jpg', isFavorite: true }]);
  await database.seedMock(mock);
  seeded = true;
  const asset = mock.state.assetByPath('/mnt/photos/old.jpg');
  assert.ok(asset);
  const id = String(asset.id);
  const login = await fetch(`${mock.url}/api/auth/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ email: 'admin@example.test', password: 'admin-password' }),
  });
  assert.equal(login.status, 201);
  const session: { accessToken: string } = await login.json();
  const createKey = await fetch(`${mock.url}/api/api-keys`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${session.accessToken}` },
    body: JSON.stringify({ name: 'apm-test', permissions: ['asset.read'] }),
  });
  assert.equal(createKey.status, 201);
  const key: { secret: string } = await createKey.json();
  const checksum = createHash('sha1').update('path:/mnt/photos/new.jpg').digest('hex');
  await database.sql(`UPDATE public.asset SET "originalPath" = '/mnt/photos/new.jpg',
    "originalFileName" = 'new.jpg', checksum = decode('${checksum}', 'hex')
    WHERE id = '${id}'::uuid`);
  const response = await fetch(`${mock.url}/api/assets/${id}`, {
    headers: { 'x-api-key': key.secret },
  });
  assert.equal(response.status, 200);
  const result: { id: string; originalPath: string; checksum: string } = await response.json();
  assert.equal(result.id, id);
  assert.equal(result.originalPath, '/mnt/photos/new.jpg');
  assert.equal(result.checksum, Buffer.from(checksum, 'hex').toString('base64'));
  assert.deepEqual(mock.state.violations(), []);
});
