import { readFile, stat } from 'node:fs/promises';
import type { MoveParams } from '../../../web/src/api/generated/MoveParams';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import type { TaskDto } from '../../../web/src/api/generated/TaskDto';
import { emptyFilter } from '../../src/api';
import { writeFile } from '../../src/disk';
import { connectThroughUi, mapAndSync, waitForTask } from '../../src/immich';
import { expect, test } from '../../src/immich-readonly-test';
import { waitForIndexed } from '../../src/waits';

test.use({
  appOptions: { env: { APM_IMMICH_DATABASE_URL_FILE: '', APM_IMMICH_QUIET_SECS: '1' } },
  allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/],
});

test('a tracked move without database configuration refuses before changing files', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
  source,
  movePage,
  confirmDialog,
}) => {
  test.setTimeout(180_000);
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app, ['photos']);
  const asset = mock.state.assetByPath('/mnt/photos/sidecars/IMG_0010.JPG');
  if (!asset) throw new Error('Expected scanned Immich fixture asset');
  const volume = (await app.api.volumes()).find((entry) => entry.name === 'photos');
  if (!volume) throw new Error('Expected fixture photos volume');
  const item = (await app.api.listing('photos', 'sidecars')).items.find(
    (entry) => entry.name === 'IMG_0010.JPG',
  );
  if (!item?.in_immich) throw new Error('Fixture is not indexed as an Immich asset');
  const oldPhoto = app.volumePath('photos/sidecars/IMG_0010.JPG');
  const oldSidecar = app.volumePath('photos/sidecars/IMG_0010.JPG.xmp');
  const photoBytes = await readFile(oldPhoto);
  const sidecarBytes = await readFile(oldSidecar);
  const before = mock.state.requests().next;
  const url = new URL('/move', app.url);
  url.searchParams.set('src', 'photos/sidecars');
  url.searchParams.set('cmp', 'photos/dupes/m1');
  await page.goto(url.href);
  await source.useFolder();
  await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
  await movePage.chooseMoveType('flat');
  await expect(source.card('IMG_0010.JPG').element).toHaveAttribute('aria-label', /In Immich/);
  await source.card('IMG_0010.JPG').select();
  await expect(movePage.actionButton()).toBeEnabled();
  const selectionResponse = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/selection$/.test(new URL(response.url()).pathname),
  );
  await movePage.actionButton().click();
  expect(((await (await selectionResponse).json()) as SelectionDto).item_count).toBe(1);
  const executing = page.waitForResponse(
    (response) =>
      response.request().method() === 'POST' &&
      /^\/api\/plans\/\d+\/execute$/.test(new URL(response.url()).pathname),
  );
  await confirmDialog.confirm('Move 1 file');
  const execution = await executing;
  const task = (await execution.json()) as TaskDto;
  const planId = Number(new URL(execution.url()).pathname.split('/')[3]);
  await waitForTask(app, task.id);
  const result = await app.api.request<PlanDto>('GET', `/api/plans/${planId}`);
  expect(result.result).toMatchObject({ done: 0, skipped: 1, failed: 0 });
  await page.getByRole('button', { name: /Last run: 0 failed, 1 skipped/ }).click();
  const dialog = page.locator('apm-plan-result');
  await expect(
    dialog.getByText('Immich database update refused:', { exact: false }).first(),
  ).toBeVisible();
  await expect(dialog).toContainText('no file moved');
  await expect(dialog).toContainText('retry');
  const warning = result.result?.failures.find((entry) => entry.code === 'immich_move_refused');
  expect(warning?.message).toContain('No file was moved.');
  expect(warning?.message.toLowerCase()).toContain('retry');
  expect(await readFile(oldPhoto)).toEqual(photoBytes);
  expect(await readFile(oldSidecar)).toEqual(sidecarBytes);
  await expect(stat(app.volumePath('photos/dupes/m1/IMG_0010.JPG'))).rejects.toMatchObject({
    code: 'ENOENT',
  });
  await expect(stat(app.volumePath('photos/dupes/m1/IMG_0010.JPG.xmp'))).rejects.toMatchObject({
    code: 'ENOENT',
  });
  expect(mock.state.assetByPath('/mnt/photos/sidecars/IMG_0010.JPG')?.id).toBe(asset.id);
  expect(
    mock.state
      .requests(before)
      .entries.filter((entry) => entry.method === 'DELETE' && entry.path === '/api/assets'),
  ).toEqual([]);
});

test('a missing database blocks tracked files but allows untracked files in the same move', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  test.setTimeout(180_000);
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app, ['photos']);
  const volume = (await app.api.volumes()).find((entry) => entry.name === 'photos');
  if (!volume) throw new Error('Expected fixture photos volume');
  const oldPhoto = app.volumePath('photos/sidecars/IMG_0010.JPG');
  const oldSidecar = app.volumePath('photos/sidecars/IMG_0010.JPG.xmp');
  const photoBytes = await readFile(oldPhoto);
  const sidecarBytes = await readFile(oldSidecar);
  await writeFile(app, 'photos', 'sidecars/e2e-untracked.jpg', photoBytes);
  const indexTask = await app.api.rescan({
    targets: [{ volume_id: volume.id, dir: 'sidecars', recursive: false }],
    force: false,
    confirm_removals: false,
  });
  await waitForTask(app, indexTask.id);
  await waitForIndexed(app, 'photos/sidecars/e2e-untracked.jpg');
  const items = (await app.api.listing('photos', 'sidecars')).items;
  const tracked = items.find((entry) => entry.name === 'IMG_0010.JPG');
  const untracked = items.find((entry) => entry.name === 'e2e-untracked.jpg');
  if (!tracked?.in_immich || !untracked || untracked.in_immich) {
    throw new Error('Expected one tracked and one untracked fixture');
  }
  const selection = await app.api.request<SelectionDto>('POST', '/api/selections', {
    ids: [tracked.id, untracked.id],
    label: 'mixed missing database safety check',
  });
  const params: MoveParams = {
    move_type: 'flat',
    source: {
      scope: { volume_id: volume.id, dir: 'sidecars', recursive: false },
      filter: { ...emptyFilter, selection_id: selection.id },
    },
    destination: { volume_id: volume.id, dir: 'dupes/m1' },
    pattern: null,
    conflict_policy: null,
    match_mode: null,
    strict_prefix: false,
  };
  const cursor = mock.state.requests().next;
  const plan = await app.api.request<PlanDto>('POST', '/api/plans', {
    kind: 'move',
    params,
    replaces: null,
  });
  const task = await app.api.request<TaskDto>('POST', `/api/plans/${plan.id}/execute`, {
    options: {},
    only: null,
  });
  await waitForTask(app, task.id);
  const result = await app.api.request<PlanDto>('GET', `/api/plans/${plan.id}`);
  expect(result.result).toMatchObject({ done: 1, skipped: 1, failed: 0 });
  const warning = result.result?.failures.find((entry) => entry.code === 'immich_move_refused');
  expect(warning?.message).toContain('No file was moved.');
  expect(warning?.message.toLowerCase()).toContain('retry');
  expect(await readFile(oldPhoto)).toEqual(photoBytes);
  expect(await readFile(oldSidecar)).toEqual(sidecarBytes);
  expect(await readFile(app.volumePath('photos/dupes/m1/e2e-untracked.jpg'))).toEqual(photoBytes);
  await expect(stat(app.volumePath('photos/sidecars/e2e-untracked.jpg'))).rejects.toMatchObject({
    code: 'ENOENT',
  });
  await expect(stat(app.volumePath('photos/dupes/m1/IMG_0010.JPG'))).rejects.toMatchObject({
    code: 'ENOENT',
  });
  await expect(stat(app.volumePath('photos/dupes/m1/IMG_0010.JPG.xmp'))).rejects.toMatchObject({
    code: 'ENOENT',
  });
  expect(
    mock.state
      .requests(cursor)
      .entries.filter((entry) => entry.method === 'DELETE' && entry.path === '/api/assets'),
  ).toEqual([]);
});
