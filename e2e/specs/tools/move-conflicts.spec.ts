import { readFile } from 'node:fs/promises';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { formatFileSize } from '../../../web/src/util/format';
import { snapshotTree, trashEntries, writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';

const conflictCases = [
  { name: 'Smart skips an identical destination', policy: 'smart', same: true, outcome: 'skip' },
  {
    name: 'Smart renames different destination content',
    policy: 'smart',
    same: false,
    outcome: 'rename',
  },
  {
    name: 'Skip preserves a conflicting destination',
    policy: 'skip',
    same: false,
    outcome: 'skip',
  },
  {
    name: 'Rename keeps both files even when identical',
    policy: 'rename',
    same: true,
    outcome: 'rename',
  },
  {
    name: 'Replace trashes different destination content',
    policy: 'replace',
    same: false,
    outcome: 'replace',
  },
] as const;

function isResponse(path: RegExp) {
  return (response: import('@playwright/test').Response) =>
    response.request().method() === 'POST' && path.test(new URL(response.url()).pathname);
}

async function waitForTask(app: import('../../src/app-instance').AppInstance, id: number) {
  await expect
    .poll(async () => (await app.api.tasks()).find((task) => task.id === id)?.state)
    .toBe('done');
}

for (const conflictCase of conflictCases) {
  test(`move conflict policy: ${conflictCase.name}`, async ({
    app,
    page,
    manifest,
    source,
    movePage,
    confirmDialog,
  }) => {
    const sourceFile = manifest.file('photos', 'dupes/m1/IMG_0300.jpg');
    const otherFile = manifest.file('photos', '2022/08/IMG_1110.jpg');
    test.skip(sourceFile.skipped, sourceFile.skip_reason ?? 'fixture scenario was skipped');
    const collision = conflictCase.same ? sourceFile : otherFile;
    const collisionBytes = await readFile(app.volumePath(`${collision.volume}/${collision.path}`));
    await writeFile(app, 'archive', 'IMG_0300.jpg', collisionBytes);
    await app.rescan();

    const url = new URL('/move', app.url);
    url.searchParams.set('src', 'photos/dupes/m1');
    url.searchParams.set('cmp', 'archive');
    await page.goto(url.href);
    await source.useFolder();
    await expect(movePage.planNotices()).toContainText('Choose the destination folder');
    let updatedPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
    await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
    await updatedPlan;

    updatedPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
    await movePage.chooseMoveType('flat');
    await updatedPlan;
    updatedPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
    await movePage.choosePolicy(conflictCase.policy);
    const plan = (await (await updatedPlan).json()) as PlanDto;

    const before = await snapshotTree(app.volumesDir, { hash: true });
    if (conflictCase.outcome === 'skip') {
      expect(plan.stats[conflictCase.same ? 'identical_at_destination' : 'skipped_by_policy']).toBe(
        1,
      );
      const fileName = sourceFile.path.split('/').at(-1) ?? sourceFile.path;
      await source.card(fileName).select();
      const selectionResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/selection$/));
      await movePage.actionButton().click();
      const selection = (await (await selectionResponse).json()) as SelectionDto;
      expect(selection.item_count).toBe(0);
      await expect(confirmDialog.dialog()).not.toBeVisible();
      expect(await snapshotTree(app.volumesDir, { hash: true })).toEqual(before);
      return;
    }

    const fileName = sourceFile.path.split('/').at(-1) ?? sourceFile.path;
    await source.card(fileName).select();
    const selectionResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/selection$/));
    await movePage.actionButton().click();
    const selection = (await (await selectionResponse).json()) as SelectionDto;
    expect(selection.item_count).toBe(1);
    expect(selection.total_size).toBe(sourceFile.size);
    await expect(confirmDialog.dialog()).toContainText('Move 1 file');
    await expect(confirmDialog.dialog()).toContainText(formatFileSize(sourceFile.size ?? 0));

    const executeResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/execute$/));
    await confirmDialog.confirm('Move 1 file');
    const executedResponse = await executeResponse;
    const task = (await executedResponse.json()) as { id: number };
    await waitForTask(app, task.id);

    const planId = Number(new URL(executedResponse.url()).pathname.split('/')[3]);
    const executed = await app.api.request<PlanDto>('GET', `/api/plans/${planId}`);
    const after = await snapshotTree(app.volumesDir, { hash: true });
    const result = executed.result;
    expect(result).not.toBeNull();
    expect(result).toMatchObject({ done: 1, skipped: 0, failed: 0, bytes: sourceFile.size });
    const destinationName = conflictCase.outcome === 'rename' ? 'IMG_0300_1.jpg' : fileName;
    const destination = after.find((entry) => entry.path === `archive/${destinationName}`);
    expect(destination?.hash).toBe(sourceFile.sha1);
    expect(after.some((entry) => entry.path === `photos/${sourceFile.path}`)).toBe(false);
    if (conflictCase.outcome === 'replace') {
      const trashed = after.filter(
        (entry) => entry.type === 'file' && entry.path.includes('/.opm-trash/') && entry.hash,
      );
      expect(trashed.some((entry) => entry.hash === otherFile.sha1)).toBe(true);
      const archive = (await app.api.volumes()).find((volume) => volume.name === 'archive');
      if (!archive) throw new Error('Missing archive volume');
      const batches = await app.api.request<{
        batches: { volume: string; reasons: string[]; items: number }[];
      }>('GET', `/api/trash/batches?volume_id=${archive.id}&offset=0&limit=100`);
      expect(batches.batches).toEqual(
        expect.arrayContaining([
          expect.objectContaining({ volume: 'archive', reasons: ['replaced'], items: 1 }),
        ]),
      );
    } else {
      expect(await trashEntries(app)).toHaveLength(0);
      expect(after.find((entry) => entry.path === 'archive/IMG_0300.jpg')?.hash).toBe(
        conflictCase.same ? sourceFile.sha1 : otherFile.sha1,
      );
    }

    expect(plan.stats.candidates).toBeGreaterThan(0);
  });
}
