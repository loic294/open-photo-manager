import { createHash } from 'node:crypto';
import { readFile, stat, writeFile } from 'node:fs/promises';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { RenameParams } from '../../../web/src/api/generated/RenameParams';
import type { TaskDto } from '../../../web/src/api/generated/TaskDto';
import { emptyFilter } from '../../src/api';
import { connectThroughUi, mapAndSync, waitForTask } from '../../src/immich';
import { expect, test } from '../../src/immich-test';

test('same-folder rename preserves the Immich identity, album, sidecar and cursors', async ({
  backedImmich: { app, database, mock, expectRemovedFiles },
  page,
  shell,
  settingsPage,
}) => {
  test.setTimeout(180_000);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app);

  const asset = mock.state.assetByPath('/mnt/photos/sidecars/IMG_0010.JPG');
  if (!asset) throw new Error('Expected scanned Immich fixture asset');
  const id = String(asset.id);
  const oldPhoto = app.volumePath('photos/sidecars/IMG_0010.JPG');
  const oldSidecar = app.volumePath('photos/sidecars/IMG_0010.JPG.xmp');
  const newPhoto = app.volumePath('photos/sidecars/Renamed_IMG_0010.JPG');
  const newSidecar = app.volumePath('photos/sidecars/Renamed_IMG_0010.JPG.xmp');
  const photoBytes = await readFile(oldPhoto);
  const sidecarBytes = await readFile(oldSidecar);
  const album = await database.sql('INSERT INTO public.album DEFAULT VALUES RETURNING id::text');
  await database.sql(`INSERT INTO public.album_asset ("albumId", "assetId")
    VALUES ('${album}'::uuid, '${id}'::uuid);
    INSERT INTO public.asset_file ("assetId", type, path)
    VALUES ('${id}'::uuid, 'sidecar', '/mnt/photos/sidecars/IMG_0010.JPG.xmp')`);
  const before = await database.sql(`SELECT json_build_object('asset', a."updateId",
    'sidecar', f."updateId") FROM public.asset a JOIN public.asset_file f ON f."assetId" = a.id
    WHERE a.id = '${id}'::uuid AND f.type = 'sidecar'`);
  const volumes = await app.api.volumes();
  const volume = volumes.find((entry) => entry.name === 'photos');
  if (!volume) throw new Error('Missing fixture photos volume');
  const params: RenameParams = {
    source: {
      scope: { volume_id: volume.id, dir: 'sidecars', recursive: false },
      filter: emptyFilter,
    },
    rules: {
      matcher: { mode: 'glob', pattern: 'IMG_0010.JPG', ignore_case: false },
      clean: false,
      rewrite: null,
      pattern: 'Renamed_{name}',
      count_digits: 1,
    },
    conflict_policy: null,
  };
  const build = () =>
    app.api.request<PlanDto>('POST', '/api/plans', {
      kind: 'rename',
      params,
      replaces: null,
    });
  const execute = async (plan: PlanDto) => {
    expect(plan.stats.to_rename).toBe(1);
    const task = await app.api.request<TaskDto>('POST', `/api/plans/${plan.id}/execute`, {
      options: {},
      only: null,
    });
    await waitForTask(app, task.id);
    return app.api.request<PlanDto>('GET', `/api/plans/${plan.id}`);
  };
  const unavailablePlan = await build();
  await database.suspendDatabase();
  try {
    const refused = await execute(unavailablePlan);
    expect(refused.result).toMatchObject({ done: 0, skipped: 1, failed: 0 });
    const warning = refused.result?.failures.find(
      (failure) => failure.code === 'immich_move_refused',
    );
    expect(warning?.message).toContain('No file was moved.');
    expect(warning?.message.toLowerCase()).toContain('retry');
    expect(await readFile(oldPhoto)).toEqual(photoBytes);
    expect(await readFile(oldSidecar)).toEqual(sidecarBytes);
    await expect(stat(newPhoto)).rejects.toMatchObject({ code: 'ENOENT' });
    await expect(stat(newSidecar)).rejects.toMatchObject({ code: 'ENOENT' });
  } finally {
    await database.resumeDatabase();
  }
  expect(
    await database.sql(`SELECT "originalPath" FROM public.asset WHERE id = '${id}'::uuid`),
  ).toBe('/mnt/photos/sidecars/IMG_0010.JPG');
  // The old photo path is recreated below to verify Immich can import it again.
  expectRemovedFiles(['photos/sidecars/IMG_0010.JPG.xmp']);
  const finished = await execute(await build());
  expect(finished.result).toMatchObject({ done: 1, skipped: 0, failed: 0 });
  expect(await readFile(newPhoto)).toEqual(photoBytes);
  expect(await readFile(newSidecar)).toEqual(sidecarBytes);
  await expect(stat(oldPhoto)).rejects.toMatchObject({ code: 'ENOENT' });
  await expect(stat(oldSidecar)).rejects.toMatchObject({ code: 'ENOENT' });
  const path = '/mnt/photos/sidecars/Renamed_IMG_0010.JPG';
  const checksum = createHash('sha1').update(`path:${path}`).digest('hex');
  await expect
    .poll(() => database.sql(`SELECT "originalPath" FROM public.asset WHERE id = '${id}'::uuid`), {
      timeout: 120_000,
    })
    .toBe(path);
  const sql = await database.sql(`SELECT json_build_object(
      'path', a."originalPath", 'name', a."originalFileName",
      'checksum', encode(a.checksum, 'hex'), 'asset', a."updateId",
      'sidecar_path', f.path, 'sidecar', f."updateId",
      'associated', EXISTS(SELECT 1 FROM public.album_asset aa
        WHERE aa."assetId" = a.id AND aa."albumId" = '${album}'::uuid))
    FROM public.asset a JOIN public.asset_file f ON f."assetId" = a.id
    WHERE a.id = '${id}'::uuid AND f.type = 'sidecar'`);
  const cursors = JSON.parse(before) as { asset: string; sidecar: string };
  const actual = JSON.parse(sql) as {
    path: string;
    name: string;
    checksum: string;
    asset: string;
    sidecar: string;
    sidecar_path: string;
    associated: boolean;
  };
  expect(actual).toMatchObject({
    path,
    name: 'Renamed_IMG_0010.JPG',
    checksum,
    sidecar_path: `${path}.xmp`,
    associated: true,
  });
  expect(actual.asset).not.toBe(cursors.asset);
  expect(actual.sidecar).not.toBe(cursors.sidecar);
  const session = await fetch(`${mock.url}/api/auth/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ email: 'admin@example.test', password: 'admin-password' }),
  });
  expect(session.status).toBe(201);
  const auth = (await session.json()) as { accessToken: string };
  const keys = await fetch(`${mock.url}/api/api-keys`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${auth.accessToken}` },
    body: JSON.stringify({ name: 'rename-assertion', permissions: ['asset.read'] }),
  });
  expect(keys.status).toBe(201);
  const key = (await keys.json()) as { secret: string };
  const response = await fetch(`${mock.url}/api/assets/${id}`, {
    headers: { 'x-api-key': key.secret },
  });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({
    id,
    originalPath: path,
    checksum: Buffer.from(checksum, 'hex').toString('base64'),
  });

  await writeFile(oldPhoto, Buffer.from('a different fixture at the original path'));
  const [scan] = await mock.control.scan('Photos');
  expect(scan?.status, scan?.error ?? undefined).toBe('completed');
  const reimported = mock.state.assetByPath('/mnt/photos/sidecars/IMG_0010.JPG');
  expect(reimported).toBeDefined();
  expect(reimported?.id).not.toBe(id);
  const importedId = String(reimported?.id);
  const importedChecksum = createHash('sha1')
    .update('path:/mnt/photos/sidecars/IMG_0010.JPG')
    .digest('hex');
  const rows = await database.sql(`SELECT json_build_object('id', id::text,
    'path', "originalPath", 'checksum', encode(checksum, 'hex'))
    FROM public.asset WHERE id IN ('${id}'::uuid, '${importedId}'::uuid) ORDER BY "originalPath"`);
  const assets = rows.split('\n').map((row) => JSON.parse(row) as object);
  expect(assets).toEqual([
    { id: importedId, path: '/mnt/photos/sidecars/IMG_0010.JPG', checksum: importedChecksum },
    { id, path, checksum },
  ]);
});
