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

test('asset move carries the linked XMP sidecar with its photo', async ({
  app,
  page,
  manifest,
  source,
  movePage,
  confirmDialog,
}) => {
  const file = manifest.file('photos', 'sidecars/IMG_0001.CR3');
  const sidecar = manifest.file('photos', 'sidecars/IMG_0001.CR3.xmp');
  test.skip(
    file.skipped || sidecar.skipped,
    file.skip_reason ?? sidecar.skip_reason ?? 'fixture scenario was skipped',
  );
  const url = new URL('/move', app.url);
  url.searchParams.set('src', 'photos/sidecars');
  url.searchParams.set('cmp', 'archive');
  await page.goto(url.href);
  await source.useFolder();
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
  const before = await snapshotTree(app.volumesDir, { hash: true });

  const executeResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/execute$/));
  await confirmDialog.confirm('Move 1 file');
  const executed = await executeResponse;
  const task = (await executed.json()) as { id: number };
  await waitForTask(app, task.id);

  const after = await snapshotTree(app.volumesDir, { hash: true });
  const moved = diffTrees(before, after).moved;
  expect(moved).toContainEqual({
    from: `photos/${file.path}`,
    to: `archive/${fileName}`,
  });
  const sidecarName = sidecar.path.split('/').at(-1) ?? sidecar.path;
  expect(moved).toContainEqual({
    from: `photos/${sidecar.path}`,
    to: `archive/${sidecarName}`,
  });
  expect(after.find((entry) => entry.path === `archive/${fileName}`)?.hash).toBe(file.sha1);
  expect(after.find((entry) => entry.path === `archive/${sidecarName}`)?.hash).toBe(sidecar.sha1);
  const planId = Number(new URL(executed.url()).pathname.split('/')[3]);
  const result = (await app.api.request<PlanDto>('GET', `/api/plans/${planId}`)).result;
  expect(result).toMatchObject({ done: 1, skipped: 0, failed: 0, bytes: file.size });
  expect(plan.stats.candidates).toBeGreaterThan(0);
});
