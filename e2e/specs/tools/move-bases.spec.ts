import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { formatFileSize } from '../../../web/src/util/format';
import { diffTrees, snapshotTree } from '../../src/disk';
import { expect, test } from '../../src/test';

function isResponse(path: RegExp) {
  return (response: import('@playwright/test').Response) =>
    response.request().method() === 'POST' && path.test(new URL(response.url()).pathname);
}

async function waitForTask(app: import('../../src/app-instance').AppInstance, id: number) {
  await expect
    .poll(async () => (await app.api.tasks()).find((task) => task.id === id)?.state)
    .toBe('done');
}

test('asset move preserves paths relative to the selected source base', async ({
  app,
  page,
  manifest,
  source,
  movePage,
  confirmDialog,
}) => {
  const file = manifest.file('photos', 'dupes/m1/IMG_0300.jpg');
  test.skip(file.skipped, file.skip_reason ?? 'fixture scenario was skipped');
  const url = new URL('/move', app.url);
  url.searchParams.set('src', 'photos/dupes');
  url.searchParams.set('cmp', 'archive');
  await page.goto(url.href);
  await source.useFolder();
  await source.openFolder('m1');
  await expect(movePage.planNotices()).toContainText('Choose the destination folder');
  const planResponse = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
  const plan = (await (await planResponse).json()) as PlanDto;
  await movePage.chooseMoveType('keep_source');

  const fileName = file.path.split('/').at(-1) ?? file.path;
  await source.card(fileName).select();
  const selectionResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/selection$/));
  await movePage.actionButton().click();
  const selection = (await (await selectionResponse).json()) as SelectionDto;
  expect(selection.item_count).toBe(1);
  expect(selection.total_size).toBe(file.size);
  await expect(confirmDialog.dialog()).toContainText('Move 1 file');
  await expect(confirmDialog.dialog()).toContainText(formatFileSize(file.size ?? 0));
  await expect(movePage.destinationFolder('m1')).toBeVisible();
  const before = await snapshotTree(app.volumesDir, { hash: true });

  const executeResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/execute$/));
  await confirmDialog.confirm('Move 1 file');
  const executed = await executeResponse;
  const task = (await executed.json()) as { id: number };
  await waitForTask(app, task.id);

  const after = await snapshotTree(app.volumesDir, { hash: true });
  const changes = diffTrees(before, after);
  expect(changes.moved).toContainEqual({
    from: `photos/${file.path}`,
    to: `archive/m1/${fileName}`,
  });
  expect(
    changes.removed.filter((entry) => !changes.moved.some((move) => move.from === entry.path)),
  ).toEqual([]);
  expect(
    changes.added.filter(
      (entry) => entry.type === 'file' && !changes.moved.some((move) => move.to === entry.path),
    ),
  ).toEqual([]);
  const planId = Number(new URL(executed.url()).pathname.split('/')[3]);
  const result = (await app.api.request<PlanDto>('GET', `/api/plans/${planId}`)).result;
  expect(result).toMatchObject({ done: 1, skipped: 0, failed: 0, bytes: file.size });
  expect(plan.stats.candidates).toBeGreaterThan(0);
});
