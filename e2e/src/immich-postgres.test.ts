import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { type MockImmich, scenarioFiles, startMockImmich } from '../../tools/mock-immich/index.ts';
import { ImmichPostgres } from './immich-postgres.ts';

test('real v3.3.3 SQL path updates are visible through the mock API', {
  timeout: 300_000,
}, async (t) => {
  const database = await ImmichPostgres.start();
  t.after(() => database.stop());
  const photos = join(database.directory, 'photos');
  await mkdir(photos);
  let mock: MockImmich;
  let seeded = false;
  mock = await startMockImmich({
    scenario: scenarioFiles.fixtures,
    serverVersion: '3.3.3',
    mounts: { '/mnt/photos': photos },
    beforeAssetRead: () => database.refreshMock(mock),
    readOnlyBacking: () => seeded,
    beforeAssetDelete: (assets, force, now) => database.deleteAssets(assets, force, now),
    beforeScanCommit: (scan) => database.commitScan(scan),
  });
  t.after(() => mock.stop());
  mock.control.addAssets([
    { path: '/mnt/photos/old.jpg', isFavorite: true, isOffline: true, trashed: false },
  ]);
  await database.seedMock(mock);
  seeded = true;
  const asset = mock.state.assetByPath('/mnt/photos/old.jpg');
  assert.ok(asset);
  const id = String(asset.id);
  const album = mock.state.albums()[0];
  const tag = mock.state.tags()[0];
  assert.ok(album && tag);
  await database.sql(`INSERT INTO public.album_asset ("albumId", "assetId")
    VALUES ('${album.id}'::uuid, '${id}'::uuid);
    INSERT INTO public.tag_asset ("tagId", "assetId")
    VALUES ('${tag.id}'::uuid, '${id}'::uuid);
    UPDATE public.asset_exif SET rating = 5, description = 'backed fixture'
      WHERE "assetId" = '${id}'::uuid`);
  assert.equal(
    await database.sql(`SELECT ("isOffline" AND "isFavorite")::text
      FROM public.asset WHERE id = '${id}'`),
    'true',
  );
  await assert.rejects(() =>
    database.deleteAssets(
      [
        {
          id,
          ownerId: String(asset.ownerId),
          libraryId: String(asset.libraryId),
          originalPath: '/mnt/photos/incorrect.jpg',
        },
      ],
      true,
      Date.now(),
    ),
  );
  assert.equal(await database.sql(`SELECT count(*) FROM public.asset WHERE id = '${id}'`), '1');
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
    body: JSON.stringify({ name: 'apm-test', permissions: ['asset.read', 'asset.delete'] }),
  });
  assert.equal(createKey.status, 201);
  const key: { secret: string } = await createKey.json();
  await assert.rejects(
    () =>
      database.sql(`UPDATE public.asset SET "originalPath" = '/mnt/photos/new.jpg',
      "originalFileName" = 'new.jpg' WHERE id = '${id}'::uuid;
      INSERT INTO public.asset
        (id, "ownerId", "libraryId", "isExternal", type, "originalPath",
         "originalFileName", checksum, "checksumAlgorithm",
         "fileCreatedAt", "fileModifiedAt", "localDateTime")
      SELECT gen_random_uuid(), "ownerId", "libraryId", true, type,
        '/mnt/photos/old.jpg', 'old.jpg', checksum, "checksumAlgorithm",
        "fileCreatedAt", "fileModifiedAt", "localDateTime"
      FROM public.asset WHERE id = '${id}'::uuid`),
    (error: unknown) => {
      assert.ok(error instanceof Error && error.cause instanceof Error);
      assert.ok('stderr' in error.cause && typeof error.cause.stderr === 'string');
      assert.match(error.cause.stderr, /duplicate key value violates unique constraint/);
      return true;
    },
  );
  assert.equal(
    await database.sql(`SELECT "originalPath" FROM public.asset WHERE id = '${id}'::uuid`),
    '/mnt/photos/old.jpg',
    'the duplicate checksum must roll back the entire attempted move',
  );
  const checksum = createHash('sha1').update('path:/mnt/photos/new.jpg').digest('hex');
  await database.sql(`UPDATE public.asset SET "originalPath" = '/mnt/photos/new.jpg',
    "originalFileName" = 'new.jpg', checksum = decode('${checksum}', 'hex'),
    "fileCreatedAt" = '2024-01-02T03:04:05Z'::timestamptz,
    "fileModifiedAt" = '2024-02-02T03:04:05Z'::timestamptz,
    "localDateTime" = '2024-01-02T04:04:05Z'::timestamptz
    WHERE id = '${id}'::uuid`);
  const response = await fetch(`${mock.url}/api/assets/${id}`, {
    headers: { 'x-api-key': key.secret },
  });
  assert.equal(response.status, 200);
  const result: {
    id: string;
    originalPath: string;
    checksum: string;
    fileCreatedAt: string;
    fileModifiedAt: string;
    localDateTime: string;
    isFavorite: boolean;
    tags: Array<{ id: string }>;
    exifInfo: { rating: number; description: string };
  } = await response.json();
  assert.equal(result.id, id);
  assert.equal(result.originalPath, '/mnt/photos/new.jpg');
  assert.equal(result.checksum, Buffer.from(checksum, 'hex').toString('base64'));
  assert.equal(result.fileCreatedAt, '2024-01-02T03:04:05.000Z');
  assert.equal(result.fileModifiedAt, '2024-02-02T03:04:05.000Z');
  assert.equal(result.localDateTime, '2024-01-02T04:04:05.000Z');
  assert.equal(result.isFavorite, true);
  assert.equal(result.exifInfo.rating, 5);
  assert.equal(result.exifInfo.description, 'backed fixture');
  assert.ok(result.tags.some((item) => item.id === tag.id));
  assert.ok(mock.state.albumsOf(id).some((item) => item.id === album.id));
  assert.deepEqual(mock.state.violations(), []);
  await writeFile(join(photos, 'scanned.jpg'), 'fixture');
  const firstScan = await mock.control.scan('Photos');
  assert.equal(firstScan[0]?.status, 'completed', firstScan[0]?.error ?? undefined);
  const afterScan = await fetch(`${mock.url}/api/assets/${id}`, {
    headers: { 'x-api-key': key.secret },
  });
  assert.equal(afterScan.status, 200);
  const afterScanBody = (await afterScan.json()) as { localDateTime: string };
  assert.equal(afterScanBody.localDateTime, '2024-01-02T04:04:05.000Z');
  assert.equal(
    await database.sql(`SELECT to_char("localDateTime" AT TIME ZONE 'UTC',
      'YYYY-MM-DD"T"HH24:MI:SS') FROM public.asset WHERE id = '${id}'::uuid`),
    '2024-01-02T04:04:05',
  );
  const scanned = mock.state.assetByPath('/mnt/photos/scanned.jpg');
  assert.ok(scanned);
  const scannedId = String(scanned.id);
  assert.equal(
    await database.sql(`SELECT "originalPath" FROM public.asset WHERE id = '${scannedId}'`),
    '/mnt/photos/scanned.jpg',
  );
  await unlink(join(photos, 'scanned.jpg'));
  const secondScan = await mock.control.scan('Photos');
  assert.equal(secondScan[0]?.status, 'completed', secondScan[0]?.error ?? undefined);
  assert.equal(mock.state.assetByPath('/mnt/photos/scanned.jpg')?.isOffline, true);
  assert.equal(
    await database.sql(`SELECT ("isOffline" AND "deletedAt" IS NOT NULL)::text
      FROM public.asset WHERE id = '${scannedId}'`),
    'true',
  );
  await database.sql(`UPDATE public.asset SET "isOffline" = false, "deletedAt" = NULL
    WHERE id = '${scannedId}'::uuid`);
  const restored = await fetch(`${mock.url}/api/assets/${scannedId}`, {
    headers: { 'x-api-key': key.secret },
  });
  assert.equal(restored.status, 200);
  const restoredBody = (await restored.json()) as { isOffline: boolean; deletedAt: string | null };
  assert.equal(restoredBody.isOffline, false);
  assert.equal(restoredBody.deletedAt, null);
  const deletion = await fetch(`${mock.url}/api/assets`, {
    method: 'DELETE',
    headers: { 'x-api-key': key.secret, 'content-type': 'application/json' },
    body: JSON.stringify({ ids: [id] }),
  });
  assert.equal(deletion.status, 204);
  assert.equal(
    await database.sql(
      `SELECT ("deletedAt" IS NOT NULL)::text FROM public.asset WHERE id = '${id}'`,
    ),
    'true',
  );
  const stillThere = await fetch(`${mock.url}/api/assets/${id}`, {
    headers: { 'x-api-key': key.secret },
  });
  assert.equal(stillThere.status, 200);
  const forceDeletion = await fetch(`${mock.url}/api/assets`, {
    method: 'DELETE',
    headers: { 'x-api-key': key.secret, 'content-type': 'application/json' },
    body: JSON.stringify({ ids: [id], force: true }),
  });
  assert.equal(forceDeletion.status, 204);
  assert.equal(await database.sql(`SELECT count(*) FROM public.asset WHERE id = '${id}'`), '0');
  assert.equal(
    (await fetch(`${mock.url}/api/assets/${id}`, { headers: { 'x-api-key': key.secret } })).status,
    400,
  );
  assert.deepEqual(mock.state.violations(), []);
});
