import { readFile, stat } from 'node:fs/promises';
import type { ImmichStatusDto } from '../../../web/src/api/generated/ImmichStatusDto';
import type { MoveParams } from '../../../web/src/api/generated/MoveParams';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import type { TaskDto } from '../../../web/src/api/generated/TaskDto';
import { emptyFilter } from '../../src/api';
import { connectThroughUi, mapAndSync, waitForTask } from '../../src/immich';
import { expect, test } from '../../src/immich-test';

test('moving out of the mapped library deletes only the offline Immich asset', async ({
  backedImmich: { app, database, mock, expectRemovedFiles },
  page,
  shell,
  settingsPage,
}) => {
  test.setTimeout(240_000);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app, ['photos']);
  const asset = mock.state.assetByPath('/mnt/photos/sidecars/IMG_0010.JPG');
  if (!asset) throw new Error('Expected scanned Immich fixture asset');
  const assetId = String(asset.id);
  const volumes = await app.api.volumes();
  const source = volumes.find((volume) => volume.name === 'photos');
  const destination = volumes.find((volume) => volume.name === 'archive');
  if (!source || !destination) throw new Error('Expected fixture volumes');
  const item = (await app.api.listing('photos', 'sidecars')).items.find(
    (file) => file.name === 'IMG_0010.JPG',
  );
  if (!item || !item.in_immich) throw new Error('Fixture is not indexed as an Immich asset');
  const selection = await app.api.request<SelectionDto>('POST', '/api/selections', {
    ids: [item.id],
    label: 'move outside library',
  });
  const params: MoveParams = {
    move_type: 'flat',
    source: {
      scope: { volume_id: source.id, dir: 'sidecars', recursive: false },
      filter: { ...emptyFilter, selection_id: selection.id },
    },
    destination: { volume_id: destination.id, dir: '' },
    pattern: null,
    conflict_policy: null,
    match_mode: null,
    strict_prefix: false,
  };
  const before = await readFile(app.volumePath('photos/sidecars/IMG_0010.JPG'));
  const beforeSidecar = await readFile(app.volumePath('photos/sidecars/IMG_0010.JPG.xmp'));
  const plan = await app.api.request<PlanDto>('POST', '/api/plans', {
    kind: 'move',
    params,
    replaces: null,
  });
  expectRemovedFiles(['photos/sidecars/IMG_0010.JPG', 'photos/sidecars/IMG_0010.JPG.xmp']);
  const task = await app.api.request<TaskDto>('POST', `/api/plans/${plan.id}/execute`, {
    options: {},
    only: null,
  });
  await waitForTask(app, task.id);
  const result = await app.api.request<PlanDto>('GET', `/api/plans/${plan.id}`);
  expect(result.result).toMatchObject({ done: 1, skipped: 0, failed: 0 });
  expect(await readFile(app.volumePath('archive/IMG_0010.JPG'))).toEqual(before);
  expect(await readFile(app.volumePath('archive/IMG_0010.JPG.xmp'))).toEqual(beforeSidecar);
  await expect(stat(app.volumePath('photos/sidecars/IMG_0010.JPG'))).rejects.toMatchObject({
    code: 'ENOENT',
  });
  await expect
    .poll(
      async () => {
        const status = await app.api.request<ImmichStatusDto>('GET', '/api/immich/status');
        if (status.needs_attention > 0) throw new Error('Immich outbox needs attention');
        return database.sql(`SELECT count(*) FROM public.asset WHERE id = '${assetId}'::uuid`);
      },
      { timeout: 180_000 },
    )
    .toBe('0');
  expect(mock.state.assetByPath('/mnt/photos/sidecars/IMG_0010.JPG')).toBeUndefined();
  expect(mock.state.assetByPath('/mnt/archive/IMG_0010.JPG')).toBeUndefined();
  const calls = mock.state.requests().entries;
  expect(calls.some((entry) => entry.method === 'POST' && entry.path.endsWith('/scan'))).toBe(true);
  const deletes = calls.filter(
    (entry) => entry.method === 'DELETE' && entry.path === '/api/assets',
  );
  expect(deletes).toHaveLength(1);
  expect(deletes[0]?.bodySummary).toMatchObject({ ids: [assetId], force: true });
  expect(mock.state.violations()).toEqual([]);
});
