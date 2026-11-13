import { cp, readFile, rename, rm, stat } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { emptyFilter } from '../../src/api';
import { snapshotTree, touch, writeFile } from '../../src/disk';
import { captureScreens } from '../../src/screens';
import { expect, test } from '../../src/test';
import { indexedFile, taskAction, terminalTask } from '../../src/tool-oracles';
import { expectStaysFalse } from '../../src/waits';

test.use({
  allowedErrors: [
    [
      // The offline/reconnect case deliberately stops the backend; full navigations cancel old streams/images.
      /requestfailed: .*\/api\/events net::ERR_(?:ABORTED|CONNECTION_REFUSED|EMPTY_RESPONSE|CONNECTION_RESET)/,
      /requestfailed: .*\/api\/files\/\d+\/(?:thumb|preview).* net::ERR_ABORTED/,
      /^console.error: Failed to load resource: net::ERR_(?:CONNECTION_REFUSED|EMPTY_RESPONSE|CONNECTION_RESET)$/,
    ],
    { scope: 'test' },
  ],
});

test('scan split menu remembers its action and explains options with DaisyUI help', async ({
  app,
  page,
  shell,
}) => {
  await page.goto(`${app.url}/browse`);
  const split = shell.menu().locator('opm-split-button');
  await expect(shell.reindexButton()).toHaveText('Scan current folders for changes');
  await split.getByRole('button', { name: 'Choose scan action', exact: true }).click();
  const menu = split.getByRole('menu', { name: 'Choose scan action', exact: true });
  await expect(menu.getByRole('menuitemradio')).toHaveCount(4);
  const before = await app.api.tasks();
  const info = menu.getByRole('menuitem', { name: 'About Re-index all assets', exact: true });
  await info.focus();
  const tooltip = page.getByRole('tooltip');
  await expect(tooltip).toContainText('metadata, checksums and thumbnails');
  await expect(tooltip).toContainText('Asks for confirmation');
  await expect(tooltip).toBeVisible();
  const helpBounds = await tooltip.boundingBox();
  const desktopMenuBounds = await menu.boundingBox();
  expect((helpBounds?.y ?? 0) + (helpBounds?.height ?? 0)).toBeLessThanOrEqual(
    desktopMenuBounds?.y ?? 0,
  );
  expect(await tooltip.evaluate((element) => element.parentElement?.matches(':popover-open'))).toBe(
    true,
  );
  await info.click();
  await expect(menu).toBeVisible();
  await expectNoSeriousA11yViolations(page);
  await captureScreens(page, 'scan-menu-desktop');
  await menu.getByRole('menuitemradio', { name: 'Re-index current folders', exact: true }).click();
  await expect(shell.reindexButton()).toHaveText('Re-index current folders');
  expect(await app.api.tasks()).toEqual(before);
  await page.reload();
  await expect(shell.reindexButton()).toHaveText('Re-index current folders');
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('button', { name: 'Menu', exact: true }).click();
  await split.getByRole('button', { name: 'Choose scan action', exact: true }).click();
  await info.focus();
  await expect(tooltip).toBeVisible();
  const menuBounds = await menu.boundingBox();
  expect(menuBounds?.x).toBeGreaterThanOrEqual(0);
  expect((menuBounds?.x ?? 0) + (menuBounds?.width ?? 0)).toBeLessThanOrEqual(390);
  expect(await page.locator('.drawer-side').evaluate((element) => element.scrollLeft)).toBe(0);
  await expectNoSeriousA11yViolations(page);
  await captureScreens(page, 'scan-menu-mobile');
  await menu
    .getByRole('menuitemradio', { name: 'Scan current folders for changes', exact: true })
    .click();
  await expect(shell.reindexButton()).toBeVisible();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await shell.reindexButton().click();
  await expect(shell.toasts()).toContainText('Open a folder first');
});

test('current re-index scopes both panes; all-assets confirmation and coalescing', async ({
  app,
  page,
  shell,
  confirmDialog,
  source,
  manifest,
}) => {
  test.setTimeout(120_000);
  await page.goto(`${app.url}/browse`);
  const before = await app.api.tasks();
  await shell.reindex();
  await expect(shell.toasts()).toContainText('Open a folder first');
  expect(await app.api.tasks()).toEqual(before);
  await page.goto(`${app.url}/browse?src=photos/stale&cmp=archive/inbox`);
  await expect(
    source.card(basename(manifest.filesOf('stale.rename')[0].path)).element,
  ).toBeVisible();
  await writeFile(app, 'photos', '2021/03/outside-scope.txt', 'must remain unindexed');
  const response = page.waitForResponse(
    (response) =>
      response.url().endsWith('/api/index/rescan') && response.request().method() === 'POST',
  );
  await shell.reindex();
  const task = await (await response).json();
  const request = JSON.parse(
    String(app.sql(`SELECT params_json FROM tasks WHERE id = ${task.id}`)[0]?.params_json),
  );
  const volumes = await app.api.volumes();
  // IndexJobs canonicalizes scopes; order is not part of the request contract.
  expect(request.targets).toHaveLength(2);
  expect(request.targets).toEqual(
    expect.arrayContaining([
      {
        volume_id: volumes.find((volume) => volume.name === 'photos')?.id,
        dir: 'stale',
        recursive: true,
      },
      {
        volume_id: volumes.find((volume) => volume.name === 'archive')?.id,
        dir: 'inbox',
        recursive: true,
      },
    ]),
  );
  expect(request.force).toBe(true);
  expect((await terminalTask(app, task.id)).title).toBe('Re-indexing 2 folders');
  expect(
    (await app.api.listing('photos', '2021/03')).items.some(
      (item) => item.name === 'outside-scope.txt',
    ),
  ).toBe(false);
  const allBefore = (await app.api.tasks()).map((task) => task.id);
  await shell.reindex(true);
  await expect(confirmDialog.dialog()).toContainText('keep using the app');
  await confirmDialog.cancel();
  expect((await app.api.tasks()).map((task) => task.id)).toEqual(allBefore);
  await shell.reindex(true);
  const allResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith('/api/index/rescan') && response.request().method() === 'POST',
  );
  await confirmDialog
    .dialog()
    .getByRole('button', { name: 'Re-index all assets', exact: true })
    .dblclick();
  await expect(confirmDialog.dialog()).not.toBeVisible();
  const all = await (await allResponse).json();
  expect((await terminalTask(app, all.id)).title).toBe('Re-indexing all assets');
  expect(
    (await app.api.tasks()).filter((task) => !allBefore.includes(task.id) && task.kind === 'index'),
  ).toHaveLength(1);
  expect(
    (await app.api.listing('photos', '2021/03')).items.some(
      (item) => item.name === 'outside-scope.txt',
    ),
  ).toBe(true);
});

test('all seven incremental fixture mutations agree across UI, API and disk', async ({
  app,
  manifest,
  page,
  shell,
  source,
  filterBar,
}) => {
  test.setTimeout(120_000);
  await page.goto(`${app.url}/browse?src=photos/stale&cmp=photos/stale`);
  const unchanged = await indexedFile(app, 'photos', manifest.filesOf('stale.new_sidecar')[0].path);
  const renamed = await indexedFile(app, 'photos', manifest.filesOf('stale.rename')[0].path);
  const changed = await indexedFile(app, 'photos', manifest.filesOf('stale.bytes_changed')[0].path);
  const mutations = await app.applyFixtureMutations();
  expect(mutations.map((row) => row.op).sort()).toEqual(
    [
      'rename',
      'change_bytes',
      'touch',
      'delete_dir',
      'add_sidecar',
      'edit_sidecar',
      'add_file',
    ].sort(),
  );
  expect((await taskAction(app, page, '/api/index/rescan', () => shell.reindex())).state).toBe(
    'done',
  );
  for (const row of mutations) {
    const path = row.new_path === '-' ? row.path : row.new_path;
    await test.step(`${row.op}: ${path}`, async () => {
      if (row.op === 'delete_dir') {
        await expect(source.folder(basename(row.path))).toHaveCount(0);
        expect(
          app.sql(`SELECT count(*) AS n FROM files WHERE dir_path = '${row.path}'`)[0]?.n,
        ).toBe(0);
      } else if (row.op === 'add_sidecar' || row.op === 'edit_sidecar') {
        const ownerPath =
          row.op === 'add_sidecar'
            ? manifest.filesOf('stale.new_sidecar').find((file) => file.kind !== 'sidecar')?.path
            : manifest.filesOf('stale.touch_xmp').find((file) => file.kind !== 'sidecar')?.path;
        if (!ownerPath) throw new Error('Sidecar mutation has no owner in the manifest');
        const detail = await indexedFile(app, row.volume, ownerPath);
        expect(detail.item.rating).toBe(row.op === 'add_sidecar' ? 4 : 3);
        expect(detail.sidecars.map((sidecar) => sidecar.name)).toContain(basename(row.path));
        await filterBar.rating(detail.item.rating);
        await expect(source.card(basename(ownerPath)).element).toBeVisible();
        await filterBar.clear();
      } else {
        await expect(source.card(basename(path)).element).toBeVisible();
        const detail = await indexedFile(app, row.volume, path);
        expect(detail.hash).toBe(row.sha1);
        expect(detail.item.mtime).toBe(Number(row.mtime));
        if (row.op === 'rename') {
          expect(detail.hash).toBe(renamed.hash);
          expect(detail.item.rating).toBe(renamed.item.rating);
          await expect(source.card(basename(row.path)).element).toHaveCount(0);
        }
        if (row.op === 'change_bytes') expect(detail.hash).not.toBe(changed.hash);
      }
    });
  }
  expect(
    (await indexedFile(app, 'photos', `${unchanged.item.dir}/${unchanged.item.name}`)).item.id,
  ).toBe(unchanged.item.id);
});

test('forced re-index rereads same-size same-mtime sidecar edits', async ({
  app,
  manifest,
  page,
  shell,
  source,
  comparison,
  filterBar,
}) => {
  const owner = manifest.filesOf('stale.touch_xmp').find((file) => file.kind !== 'sidecar');
  const sidecar = manifest.filesOf('stale.touch_xmp').find((file) => file.kind === 'sidecar');
  if (!owner || !sidecar) throw new Error('Missing sidecar mutation fixtures');
  await page.goto(`${app.url}/browse?src=photos/stale&cmp=photos/stale`);
  await source.waitReady();
  await comparison.waitReady();
  const original = await readFile(app.volumePath(`photos/${sidecar.path}`), 'utf8');
  const edited = original
    .replace(/Rating="2"/, 'Rating="5"')
    .replace(/<xmp:Rating>2</, '<xmp:Rating>5<');
  expect(edited).not.toBe(original);
  expect(Buffer.byteLength(edited)).toBe(Buffer.byteLength(original));
  const time = (await stat(app.volumePath(`photos/${sidecar.path}`))).mtime;
  await writeFile(app, 'photos', sidecar.path, edited);
  await touch(app, 'photos', sidecar.path, time);
  await app.rescan();
  expect((await indexedFile(app, 'photos', owner.path)).item.rating).toBe(2);
  expect((await taskAction(app, page, '/api/index/rescan', () => shell.reindex())).state).toBe(
    'done',
  );
  await source.waitReady();
  await comparison.waitReady();
  await filterBar.rating(5);
  await expect(source.card(basename(owner.path)).element).toBeVisible();
  expect((await indexedFile(app, 'photos', owner.path)).item.rating).toBe(5);
});

test('mass disappearance is held, restored, then explicitly removed without disk writes @slow', async ({
  app,
  manifest,
  page,
  shell,
  confirmDialog,
  allowedErrors,
  source,
  bottomBar,
}) => {
  test.slow();
  const grid = manifest.filesOf('deep.grid_500');
  const dir = dirname(grid[0].path);
  const volume = (await app.api.volumes()).find((volume) => volume.name === grid[0].volume);
  if (!volume) throw new Error('Grid volume missing');
  const originalIds = app.sql(
    `SELECT id FROM files WHERE volume_id = ${volume.id} AND dir_path = '${dir}' ORDER BY id`,
  );
  const backup = join(app.directory, 'grid-backup');
  await cp(app.volumePath(`${volume.name}/${dir}`), backup, {
    recursive: true,
    preserveTimestamps: true,
  });
  await page.goto(`${app.url}/browse?src=${volume.name}/${dir}&cmp=${volume.name}/${dir}`);
  await rm(app.volumePath(`${volume.name}/${dir}`), { recursive: true });
  expect((await taskAction(app, page, '/api/index/rescan', () => shell.reindex())).state).toBe(
    'done',
  );
  await expect(shell.notices()).toContainText(`${grid.length} files could not be found on disk`);
  for (const theme of ['light', 'dark'] as const) {
    await shell.theme(theme);
    await expectNoSeriousA11yViolations(page, { include: ['opm-index-notices'] });
  }
  await captureScreens(page, 'system-held-removals');
  expect((await app.api.indexStatus()).held_removals).toEqual([
    { volume_id: volume.id, volume_name: volume.name, files: grid.length },
  ]);
  await expect(source.card(basename(grid[0].path)).element).toBeVisible();
  expect(await bottomBar.counts('source')).toContain(String(grid.length));
  expect(
    app.sql(
      `SELECT id FROM files WHERE volume_id = ${volume.id} AND dir_path = '${dir}' ORDER BY id`,
    ),
  ).toEqual(originalIds);
  await cp(backup, app.volumePath(`${volume.name}/${dir}`), {
    recursive: true,
    preserveTimestamps: true,
  });
  await shell.notices().getByRole('button', { name: 'Rescan', exact: true }).click();
  await expect(shell.notices()).not.toContainText('could not be found on disk', {
    timeout: 30_000,
  });
  await rm(app.volumePath(`${volume.name}/${dir}`), { recursive: true });
  expect((await taskAction(app, page, '/api/index/rescan', () => shell.reindex())).state).toBe(
    'done',
  );
  const before = await snapshotTree(app.volumesDir, { hash: true });
  await shell.notices().getByRole('button', { name: 'Remove from the index', exact: true }).click();
  await expect(confirmDialog.dialog()).toContainText(String(grid.length));
  await expect(confirmDialog.dialog()).toContainText('no file on disk is touched');
  await confirmDialog.cancel();
  expect((await app.api.indexStatus()).held_removals[0].files).toBe(grid.length);
  await shell.notices().getByRole('button', { name: 'Remove from the index', exact: true }).click();
  // Deleting the displayed indexed folder causes its documented 404/ancestor walk-up.
  const allowanceStart = allowedErrors.length;
  allowedErrors.push(
    `API 404: ${app.url}/api/browse/folders`,
    /^console.error: Failed to load resource: the server responded with a status of 404 \(Not Found\)$/,
    `requestfailed: ${app.url}/api/browse/items net::ERR_ABORTED`,
  );
  await confirmDialog.confirm('Remove from the index');
  await expect(shell.notices()).not.toContainText('could not be found on disk', {
    timeout: 30_000,
  });
  expect(
    app.sql(
      `SELECT count(*) AS n FROM files WHERE volume_id = ${volume.id} AND dir_path = '${dir}'`,
    )[0]?.n,
  ).toBe(0);
  expect(await snapshotTree(app.volumesDir, { hash: true })).toEqual(before);
  await expect(page.getByRole('region', { name: 'Source', exact: true })).not.toContainText(
    'Could not load',
  );
  allowedErrors.splice(allowanceStart);
});

test('offline volume keeps identities and a reconnect restores live re-index controls', async ({
  app,
  page,
  shell,
  source,
  allowedErrors,
  manifest,
  comparison,
}) => {
  test.setTimeout(120_000);
  const volumes = await app.api.volumes();
  const archive = volumes.find((volume) => volume.name === 'archive');
  if (!archive) throw new Error('Archive missing');
  const ids = app.sql(`SELECT id FROM files WHERE volume_id = ${archive.id} ORDER BY id`);
  const offline = join(app.directory, 'archive-offline');
  await rename(app.volumePath('archive'), offline);
  try {
    await app.rescan();
    await page.goto(`${app.url}/browse`);
    // Offline state is exposed by volume diagnostics, not folder-list text.
    expect((await app.api.volumes()).find((volume) => volume.name === 'archive')?.online).toBe(
      false,
    );
    await shell.navigate('Settings');
    await expect(page.locator('opm-settings-general')).toContainText('offline');
    await page.goto(`${app.url}/browse?src=archive&cmp=photos`);
    await expect(source.element.getByRole('alert')).toContainText(
      'Volume archive is offline: showing what was last indexed',
    );
    const snapshot = (await app.api.listing('archive', '')).items;
    for (const item of snapshot) await expect(source.card(item.name).element).toBeVisible();
    const folders = await app.api.folders({
      volume_id: archive.id,
      dir: '',
      filter: emptyFilter,
      plan: null,
    });
    expect(snapshot.length + folders.folders.length).toBeGreaterThan(0);
    for (const folder of folders.folders) await expect(source.folder(folder.name)).toBeVisible();
    expect(app.sql(`SELECT id FROM files WHERE volume_id = ${archive.id} ORDER BY id`)).toEqual(
      ids,
    );
    expect((await app.api.volumes()).find((volume) => volume.name === 'photos')?.online).toBe(true);
    await expect(
      app.api.rescan({
        targets: [{ volume_id: archive.id, dir: '', recursive: true }],
        force: true,
        confirm_removals: false,
      }),
    ).rejects.toThrow('volume_offline');
  } finally {
    await rename(offline, app.volumePath('archive'));
  }
  await app.rescan();
  expect((await app.api.volumes()).find((volume) => volume.name === 'archive')?.online).toBe(true);
  expect(app.sql(`SELECT id FROM files WHERE volume_id = ${archive.id} ORDER BY id`)).toEqual(ids);
  // Exercise disconnected controls only after both panes' initial requests have completed.
  const initialResponses = ['folders', 'items']
    .map((kind) => `/api/browse/${kind}`)
    .concat('/api/facets')
    .map((path) =>
      page.waitForResponse(
        (response) => new URL(response.url()).pathname === path && response.ok(),
      ),
    );
  await page.goto(`${app.url}/browse?src=photos/stale&cmp=photos/stale`);
  await Promise.all((await Promise.all(initialResponses)).map((response) => response.finished()));
  await expect(source.element.locator('[aria-busy="true"]')).toHaveCount(0);
  await expect(
    source.card(basename(manifest.filesOf('stale.rename')[0].path)).element,
  ).toBeVisible();
  await expect(
    comparison.card(basename(manifest.filesOf('stale.rename')[0].path)).element,
  ).toBeVisible();
  const allowanceStart = allowedErrors.length;
  for (const path of ['browse/folders', 'browse/items', 'facets'])
    for (const error of ['CONNECTION_RESET', 'EMPTY_RESPONSE', 'CONNECTION_REFUSED'])
      allowedErrors.push(`requestfailed: ${app.url}/api/${path} net::ERR_${error}`);
  await app.suspend();
  await expect(shell.reindexButton()).toBeDisabled({ timeout: 30_000 });
  await expect(shell.reindexButton(true)).toBeDisabled();
  // Connection absence has no positive terminal event; controls must remain disabled until resume.
  await expectStaysFalse(() => shell.reindexButton().isEnabled(), { forMs: 500 });
  await app.resume();
  await expect(shell.reindexButton()).toBeEnabled({ timeout: 45_000 });
  await expect(shell.reindexButton(true)).toBeEnabled();
  allowedErrors.splice(allowanceStart);
});
