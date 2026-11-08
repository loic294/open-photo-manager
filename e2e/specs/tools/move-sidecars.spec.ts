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

const sidecarCases = [
  {
    label: 'exact XMP',
    ownerPath: 'sidecars/IMG_0001.CR3',
    sidecarPath: 'sidecars/IMG_0001.CR3.xmp',
    destinationSidecarName: null,
    link: 'exact',
    shared: false,
  },
  {
    label: 'shared stem XMP',
    ownerPath: 'sidecars/IMG_0002.CR3',
    sidecarPath: 'sidecars/IMG_0002.xmp',
    destinationSidecarName: null,
    link: 'stem',
    shared: true,
  },
  {
    label: 'case-insensitive XMP',
    ownerPath: 'sidecars/IMG_0003.JPG',
    sidecarPath: 'sidecars/img_0003.xmp',
    destinationSidecarName: 'IMG_0003.xmp',
    link: 'stem',
    shared: false,
  },
  {
    label: 'AAE edit metadata',
    ownerPath: 'sidecars/IMG_0006.jpg',
    sidecarPath: 'sidecars/IMG_0006.jpg.aae',
    destinationSidecarName: null,
    link: 'exact',
    shared: false,
  },
  {
    label: 'Takeout JSON metadata',
    ownerPath: 'sidecars/takeout_0007.jpg',
    sidecarPath: 'sidecars/takeout_0007.jpg.json',
    destinationSidecarName: null,
    link: 'exact',
    shared: false,
  },
] as const;

for (const sidecarCase of sidecarCases) {
  test(`asset move carries ${sidecarCase.label} with its photo`, async ({
    app,
    page,
    manifest,
    source,
    movePage,
    confirmDialog,
  }) => {
    const file = manifest.file('photos', sidecarCase.ownerPath);
    const sidecar = manifest.file('photos', sidecarCase.sidecarPath);
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
    const sidecarName = sidecar.path.split('/').at(-1) ?? sidecar.path;
    const destinationSidecarName = sidecarCase.destinationSidecarName ?? sidecarName;
    expect(moved).toContainEqual({
      from: `photos/${file.path}`,
      to: `archive/${fileName}`,
    });
    expect(after.find((entry) => entry.path === `archive/${fileName}`)?.hash).toBe(file.sha1);
    const archivedSidecar = after.find(
      (entry) => entry.path === `archive/${destinationSidecarName}`,
    );
    expect(
      archivedSidecar?.hash,
      JSON.stringify(
        after.filter((entry) => entry.path.toLowerCase().includes(sidecarName.toLowerCase())),
        null,
        2,
      ),
    ).toBe(sidecar.sha1);
    if (sidecarCase.shared) {
      expect(moved).not.toContainEqual({
        from: `photos/${sidecar.path}`,
        to: `archive/${sidecarName}`,
      });
      expect(after.find((entry) => entry.path === `photos/${sidecar.path}`)?.hash).toBe(
        sidecar.sha1,
      );
    } else {
      expect(moved).toContainEqual({
        from: `photos/${sidecar.path}`,
        to: `archive/${destinationSidecarName}`,
      });
    }

    const links = app.sql(
      `SELECT owner.name AS owner_name, sidecar.name AS sidecar_name, link.kind
       FROM file_sidecars link
       JOIN files owner ON owner.id = link.file_id
       JOIN files sidecar ON sidecar.id = link.sidecar_id
       JOIN volumes volume ON volume.id = owner.volume_id
       WHERE volume.name = 'archive' AND owner.dir_path = '' AND owner.name = '${fileName}'`,
    );
    expect(links).toEqual([
      expect.objectContaining({
        owner_name: fileName,
        sidecar_name: destinationSidecarName,
        kind: sidecarCase.link,
      }),
    ]);
    if (sidecarCase.shared) {
      expect(
        app.sql(
          `SELECT owner.name AS owner_name, sidecar.name AS sidecar_name, link.kind
           FROM file_sidecars link
           JOIN files owner ON owner.id = link.file_id
           JOIN files sidecar ON sidecar.id = link.sidecar_id
           JOIN volumes volume ON volume.id = owner.volume_id
           WHERE volume.name = 'photos' AND owner.dir_path = 'sidecars'
             AND owner.name = 'IMG_0002.JPG'`,
        ),
      ).toEqual([
        expect.objectContaining({
          owner_name: 'IMG_0002.JPG',
          sidecar_name: 'IMG_0002.xmp',
          kind: 'stem',
        }),
      ]);
    }

    const planId = Number(new URL(executed.url()).pathname.split('/')[3]);
    const result = (await app.api.request<PlanDto>('GET', `/api/plans/${planId}`)).result;
    expect(result).toMatchObject({ done: 1, skipped: 0, failed: 0, bytes: file.size });
    expect(plan.stats.candidates).toBeGreaterThan(0);
  });
}

test('asset move retains a shared sidecar when its final destination is already present', async ({
  app,
  page,
  source,
  movePage,
  confirmDialog,
}) => {
  const first = 'IMG_0002.CR3';
  const second = 'IMG_0002.JPG';
  const sidecar = 'IMG_0002.xmp';
  const url = new URL('/move', app.url);
  url.searchParams.set('src', 'photos/sidecars');
  url.searchParams.set('cmp', 'archive');
  await page.goto(url.href);
  await source.useFolder();
  await expect(movePage.planNotices()).toContainText('Choose the destination folder');
  const planResponse = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
  await planResponse;
  await movePage.chooseMoveType('keep_source');

  await source.card(first).select();
  await source.card(second).select();
  const selectionResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/selection$/));
  await movePage.actionButton().click();
  const selection = (await (await selectionResponse).json()) as SelectionDto;
  expect(selection.item_count).toBe(2);
  await expect(confirmDialog.dialog()).toContainText('Move 2 files');
  const before = await snapshotTree(app.volumesDir, { hash: true });

  const executeResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/execute$/));
  await confirmDialog.confirm('Move 2 files');
  const executed = await executeResponse;
  const task = (await executed.json()) as { id: number };
  await waitForTask(app, task.id);

  const after = await snapshotTree(app.volumesDir, { hash: true });
  const moved = diffTrees(before, after).moved;
  for (const name of [first, second]) {
    expect(moved).toContainEqual({ from: `photos/sidecars/${name}`, to: `archive/${name}` });
  }
  const sourceSidecar = after.find((entry) => entry.path === `photos/sidecars/${sidecar}`);
  const destinationSidecar = after.find((entry) => entry.path === `archive/${sidecar}`);
  expect(sourceSidecar?.hash).toBeDefined();
  expect(destinationSidecar?.hash).toBe(sourceSidecar?.hash);

  const planId = Number(new URL(executed.url()).pathname.split('/')[3]);
  const result = (await app.api.request<PlanDto>('GET', `/api/plans/${planId}`)).result;
  if (!result) throw new Error('Executed move plan did not return a result');
  expect(result).toMatchObject({ done: 2, skipped: 0, failed: 0 });
  expect(result.note).toContain('shared_sidecar_retained');
  expect(result.note).toContain(sidecar);
});
