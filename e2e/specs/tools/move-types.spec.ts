import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import type { SelectionDto } from '../../../web/src/api/generated/SelectionDto';
import { formatFileSize } from '../../../web/src/util/format';
import { ApiFailure } from '../../src/api';
import { diffTrees, snapshotTree } from '../../src/disk';
import { expect, test } from '../../src/test';
import { expectedDestination } from '../../src/tool-oracles';

const moveTypes = [
  { value: 'keep_source', label: 'Keep source file structure' },
  { value: 'keep_destination', label: 'Keep destination file structure' },
  { value: 'flat', label: 'Flat file transfer' },
  { value: 'custom', label: 'Use custom file structure' },
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

for (const { value, label } of moveTypes) {
  const smoke = value === 'keep_source' ? '@smoke ' : '';
  test(`${smoke}asset move executes ${label} as previewed`, async ({
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
    url.searchParams.set('src', 'photos/dupes/m1');
    url.searchParams.set('cmp', 'archive');
    await page.goto(url.href);
    await source.useFolder();
    await expect(movePage.planNotices()).toContainText('Choose the destination folder');
    await movePage.previewAfter(() =>
      movePage
        .destinationBase()
        .getByRole('button', { name: 'Use this folder', exact: true })
        .click(),
    );
    if (value !== 'keep_source') {
      await movePage.previewAfter(() => movePage.chooseMoveType(value));
    } else {
      await movePage.chooseMoveType(value);
    }
    if (value === 'custom') {
      await movePage.openPattern();
      const previewResponse = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
      await movePage.previewAfter(() => movePage.pattern().fill('{YYYY}/{filename}'));
      expect((await previewResponse).status()).toBe(200);
      await expect(movePage.actionButton()).toBeEnabled();
    }
    const fileName = file.path.split('/').at(-1) ?? file.path;
    const destination =
      value === 'custom'
        ? expectedDestination(file, '{YYYY}/{filename}')
        : value === 'keep_destination'
          ? `m1/${fileName}`
          : fileName;
    await source.card(fileName).select();
    await expect(source.card(fileName).isSelected()).resolves.toBe(true);

    const destinationFolders = destination.split('/').slice(0, -1);
    if (value === 'custom') {
      const firstFolder = destinationFolders[0];
      if (!firstFolder) throw new Error('Expected a custom destination folder');
      await expect
        .poll(() => movePage.destinationFolder(firstFolder).getAttribute('class'))
        .toContain('border-dashed');
    } else {
      for (const folder of destinationFolders) await movePage.openDestinationFolder(folder);
      const ghost = movePage.destinationCard(fileName);
      await expect(ghost.element).toBeVisible();
      await expect(ghost.isGhost()).resolves.toBe(true);
    }
    const before = await snapshotTree(app.volumesDir, { hash: true });

    const selectionResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/selection$/));
    await movePage.actionButton().click();
    const selection = (await (await selectionResponse).json()) as SelectionDto;
    expect(selection.item_count).toBe(1);
    expect(selection.total_size).toBe(file.size);
    await expect(confirmDialog.dialog()).toContainText('1 file');
    await expect(confirmDialog.dialog()).toContainText(formatFileSize(file.size ?? 0));
    const executeResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/execute$/));
    await confirmDialog.confirm('Move 1 file');
    const executed = await executeResponse;
    const task = (await executed.json()) as { id: number };
    await waitForTask(app, task.id);

    const after = await snapshotTree(app.volumesDir, { hash: true });
    const changes = diffTrees(before, after);
    expect(changes.moved).toContainEqual({
      from: `photos/${file.path}`,
      to: `archive/${destination}`,
    });
    expect(
      changes.removed.filter((entry) => !changes.moved.some((move) => move.from === entry.path)),
    ).toEqual([]);
    expect(changes.changed).toEqual([]);
    const planId = Number(new URL(executed.url()).pathname.split('/')[3]);
    const result = (await app.api.request<PlanDto>('GET', `/api/plans/${planId}`)).result;
    expect(result).toMatchObject({ done: 1, skipped: 0, failed: 0, bytes: file.size });
    if (value === 'custom') {
      const directory = destinationFolders.join('/');
      await expect
        .poll(async () => {
          try {
            return (await app.api.listing('archive', directory)).items.some(
              (item) => item.name === fileName,
            );
          } catch (error) {
            if (error instanceof ApiFailure && error.status === 404) return false;
            throw error;
          }
        })
        .toBe(true);
      for (const folder of destinationFolders) await movePage.openDestinationFolder(folder);
      await expect(movePage.destinationCard(fileName).element).toBeVisible();
      await expect(movePage.destinationCard(fileName).isGhost()).resolves.toBe(false);
    }
    await expect
      .poll(() => app.api.listing('photos', 'dupes/m1'))
      .toMatchObject({
        items: expect.not.arrayContaining([expect.objectContaining({ name: fileName })]),
      });
    await expect
      .poll(() => app.api.listing('archive', destinationFolders.join('/')))
      .toMatchObject({
        items: expect.arrayContaining([expect.objectContaining({ name: fileName })]),
      });
  });
}
