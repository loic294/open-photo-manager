import { readFile } from 'node:fs/promises';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { writeFile } from '../../src/disk';
import { captureScreens } from '../../src/screens';
import { expect, test } from '../../src/test';
import { executePlan, indexedFile, movePlan, terminalTask } from '../../src/tool-oracles';

test.use({
  // Full page reload tears down only the old SSE stream and in-flight thumbnails.
  allowedErrors: [
    [
      /requestfailed: .*\/api\/events net::ERR_ABORTED/,
      /requestfailed: .*\/api\/files\/\d+\/thumb.* net::ERR_ABORTED/,
      // The restart scenario deliberately disconnects the stream; the validation scenario expects a real 422.
      /requestfailed: .*\/api\/events net::ERR_(?:CONNECTION_REFUSED|EMPTY_RESPONSE|CONNECTION_RESET)/,
      /^console.error: Failed to load resource: net::ERR_(?:CONNECTION_REFUSED|EMPTY_RESPONSE|CONNECTION_RESET)$/,
      /^API 422: .*\/api\/settings$/,
      /^console.error: Failed to load resource: the server responded with a status of 422 /,
    ],
    { scope: 'test' },
  ],
});

test('all General fields load; validation, discard and save survive browser and server restart', async ({
  app,
  page,
  shell,
  settingsPage,
}) => {
  test.setTimeout(120_000);
  const baseline = await app.api.settings();
  await page.goto(`${app.url}/settings`);
  for (const [label, value] of [
    ['Longest generated name', baseline.name_max_len],
    ['Scan workers', baseline.index.concurrency],
    ['Hash workers', baseline.index.hash_concurrency],
    ['Thumbnail workers', baseline.index.thumbnail_concurrency],
    ['Full rescan interval', baseline.index.rescan_interval_minutes],
  ] as const)
    await expect(settingsPage.field(label)).toHaveValue(String(value));
  for (const [label, values] of [
    ['Sidecar extensions', baseline.sidecar_extensions],
    ['Ignored names', baseline.junk_names],
    ['Ignored prefixes', baseline.junk_prefixes],
  ] as const) {
    for (const value of values)
      await expect(
        settingsPage
          .general()
          .locator(`opm-chip-input[label="${label}"]`)
          .getByRole('button', { name: `Remove ${value}`, exact: true }),
      ).toBeVisible();
  }
  await expect(settingsPage.field('When a file with the same name already exists')).toHaveValue(
    baseline.default_conflict_policy,
  );
  await expect(settingsPage.saveButton()).toHaveCount(0);
  await settingsPage.fill('Scan workers', '0');
  await expect(settingsPage.saveButton()).toBeDisabled();
  await expect(settingsPage.field('Scan workers')).toHaveAttribute('aria-invalid', 'true');
  await settingsPage.discard();
  await expect(settingsPage.field('Scan workers')).toHaveValue(String(baseline.index.concurrency));
  await settingsPage.fill('Full rescan interval', '31');
  await settingsPage.fill('Longest generated name', '143');
  await settingsPage.fill('Scan workers', '2');
  await settingsPage.fill('Hash workers', '2');
  await settingsPage.fill('Thumbnail workers', '2');
  await expect(settingsPage.saveButton()).toBeEnabled();
  await settingsPage.saveGeneral();
  await expect(shell.toasts()).toContainText('Settings saved');
  expect((await app.api.settings()).index.rescan_interval_minutes).toBe(31);
  expect((await app.api.settings()).index).toMatchObject({
    concurrency: 2,
    hash_concurrency: 2,
    thumbnail_concurrency: 2,
  });
  expect((await app.api.settings()).name_max_len).toBe(143);
  await page.reload();
  await expect(settingsPage.field('Full rescan interval')).toHaveValue('31');
  await app.restart();
  await page.reload();
  await expect(settingsPage.field('Full rescan interval')).toHaveValue('31');
  await app.rescan();
});

test('Settings tab switches preserve drafts and Enter in fields never submits the form', async ({
  app,
  page,
  settingsPage,
}) => {
  await page.goto(`${app.url}/settings`);
  const original = await app.api.settings();
  await settingsPage.fill('Longest generated name', '144');
  await settingsPage.field('Longest generated name').press('Enter');
  expect((await app.api.settings()).name_max_len).toBe(original.name_max_len);
  await settingsPage.addChip('Ignored prefixes', 'system-draft-prefix');
  expect((await app.api.settings()).junk_prefixes).not.toContain('system-draft-prefix');
  await settingsPage.tab('Immich');
  await settingsPage.tab('General');
  await expect(settingsPage.field('Longest generated name')).toHaveValue('144');
  await expect(
    settingsPage.general().getByRole('button', { name: 'Remove system-draft-prefix', exact: true }),
  ).toBeVisible();
  await settingsPage.discard();
  await expect(settingsPage.saveButton()).toHaveCount(0);
  await page.reload();
  await expect(settingsPage.field('Longest generated name')).toHaveValue(
    String(original.name_max_len),
  );
});

test('native beforeunload protects edits; staying retains the draft and leaving discards it', async ({
  app,
  page,
  settingsPage,
}) => {
  await page.goto(`${app.url}/settings`);
  const original = (await app.api.settings()).name_max_len;
  await settingsPage.fill('Longest generated name', String(original - 1));
  const stay = page.waitForEvent('dialog').then(async (dialog) => {
    expect(dialog.type()).toBe('beforeunload');
    await dialog.dismiss();
  });
  // A dismissed beforeunload has no load event; do not await a cancelled navigation.
  await page.evaluate(() => window.location.reload());
  await stay;
  await expect(settingsPage.field('Longest generated name')).toHaveValue(String(original - 1));
  const leave = page.waitForEvent('dialog').then(async (dialog) => {
    expect(dialog.type()).toBe('beforeunload');
    await dialog.accept();
  });
  await page.reload();
  await leave;
  await expect(settingsPage.field('Longest generated name')).toHaveValue(String(original));
  expect((await app.api.settings()).name_max_len).toBe(original);
});

test('server field validation is displayed inline without losing other edits', async ({
  app,
  page,
  settingsPage,
}) => {
  // Deliberately corrupt the submitted field, not the response: validation comes from the real server.
  test.info().annotations.push({
    type: 'expected HTTP error',
    description: 'PUT /api/settings returns real 422 for zero scan concurrency',
  });
  await page.goto(`${app.url}/settings`);
  await settingsPage.fill('Longest generated name', '142');
  await page.route('**/api/settings', async (route) => {
    if (route.request().method() !== 'PUT') return route.continue();
    const body = route.request().postDataJSON();
    await route.continue({
      postData: JSON.stringify({ ...body, index: { ...body.index, concurrency: 0 } }),
    });
  });
  const response = page.waitForResponse(
    (response) => response.url().endsWith('/api/settings') && response.status() === 422,
  );
  await settingsPage.save();
  expect((await response).status()).toBe(422);
  await expect(settingsPage.field('Scan workers')).toHaveAttribute('aria-invalid', 'true');
  await expect(settingsPage.field('Longest generated name')).toHaveValue('142');
  expect((await app.api.settings()).name_max_len).not.toBe(142);
  await settingsPage.discard();
});

test('custom sidecar configuration links and moves metadata; ignored names alter the index', async ({
  app,
  manifest,
  page,
  shell,
  settingsPage,
  source,
}) => {
  test.setTimeout(120_000);
  const template = manifest.filesOf('exif.full_nikon')[0];
  await writeFile(
    app,
    'photos',
    'system-config/configured.jpg',
    await readFile(app.volumePath(`photos/${template.path}`)),
  );
  await writeFile(
    app,
    'photos',
    'system-config/configured.jpg.opmmeta',
    'configurable metadata sidecar',
  );
  await app.rescan();
  await page.goto(`${app.url}/settings`);
  await settingsPage.addChip('Sidecar extensions', 'opmmeta');
  await settingsPage.saveGeneral();
  expect((await app.api.settings()).sidecar_extensions).toContain('opmmeta');
  await page.goto(`${app.url}/browse?src=photos/system-config&cmp=photos/system-config`);
  await shell.reindex();
  await app.waitForIdle();
  expect(
    (await indexedFile(app, 'photos', 'system-config/configured.jpg')).sidecars,
  ).toContainEqual({ name: 'configured.jpg.opmmeta', kind: 'exact' });
  const plan = await movePlan(
    app,
    { volume: 'photos', dir: 'system-config' },
    { volume: 'archive', dir: 'system-config-destination' },
  );
  const task = await executePlan(app, plan);
  expect((await terminalTask(app, task.id)).state).toBe('done');
  expect(
    (await indexedFile(app, 'archive', 'system-config-destination/configured.jpg')).sidecars,
  ).toContainEqual({ name: 'configured.jpg.opmmeta', kind: 'exact' });
  expect(
    await readFile(
      app.volumePath('archive/system-config-destination/configured.jpg.opmmeta'),
      'utf8',
    ),
  ).toBe('configurable metadata sidecar');
  await shell.navigate('Settings');
  await settingsPage.removeChip('Sidecar extensions', 'opmmeta');
  await settingsPage.addChip('Ignored names', 'configured.jpg');
  await settingsPage.saveGeneral();
  await page.goto(
    `${app.url}/browse?src=archive/system-config-destination&cmp=archive/system-config-destination`,
  );
  await shell.reindex();
  await app.waitForIdle();
  await expect(source.card('configured.jpg').element).toHaveCount(0);
  expect(
    (await app.api.listing('archive', 'system-config-destination')).items.map((item) => item.name),
  ).toEqual(['configured.jpg.opmmeta']);
  await shell.navigate('Settings');
  await settingsPage.removeChip('Ignored names', 'configured.jpg');
  await settingsPage.saveGeneral();
  await shell.navigate('Browse');
  await shell.reindex();
  await app.waitForIdle();
  await expect(source.card('configured.jpg').element).toBeVisible();
  expect(
    (await indexedFile(app, 'archive', 'system-config-destination/configured.jpg')).sidecars,
  ).toEqual([]);
});

test('Settings and Trash are keyboard-operable and accessible in light and dark', async ({
  app,
  page,
  shell,
  settingsPage,
}) => {
  await page.goto(`${app.url}/settings`);
  await settingsPage.field('Full rescan interval').focus();
  await page.keyboard.press('ControlOrMeta+A');
  await page.keyboard.type('32');
  await settingsPage.saveButton().focus();
  await page.keyboard.press('Enter');
  await expect(settingsPage.saveButton()).toHaveCount(0);
  expect((await app.api.settings()).index.rescan_interval_minutes).toBe(32);
  for (const route of ['Settings', 'Trash']) {
    await shell.navigate(route);
    for (const theme of ['light', 'dark'] as const) {
      await shell.theme(theme);
      await expectNoSeriousA11yViolations(page);
    }
    await captureScreens(page, `system-${route.toLowerCase()}`);
  }
});

for (const policy of ['skip', 'rename', 'replace'] as const) {
  test(`the saved Default (${policy}) is shared by move/format and used by a real plan`, async ({
    app,
    manifest,
    page,
    shell,
    settingsPage,
    movePage,
    formatPage,
  }) => {
    test.setTimeout(90_000);
    const files = manifest.filesOf('deep.grid_500').slice(0, 2);
    const incoming = await readFile(app.volumePath(`photos/${files[0].path}`));
    const existing = await readFile(app.volumePath(`photos/${files[1].path}`));
    await writeFile(app, 'photos', 'system-default-source/collision.jpg', incoming);
    await writeFile(app, 'photos', 'system-default-target/collision.jpg', existing);
    await app.rescan();
    await page.goto(`${app.url}/settings`);
    await settingsPage.field('When a file with the same name already exists').selectOption(policy);
    await settingsPage.saveGeneral();
    expect((await app.api.settings()).default_conflict_policy).toBe(policy);
    await shell.navigate('Asset move');
    await expect(
      movePage.policy().getByRole('option', {
        name: `Default (${policy[0].toUpperCase()}${policy.slice(1)})`,
        exact: true,
      }),
    ).toHaveCount(1);
    await shell.navigate('Folder formatting');
    await formatPage.edit();
    await expect(
      formatPage.policy().getByRole('option', {
        name: `Default (${policy[0].toUpperCase()}${policy.slice(1)})`,
        exact: true,
      }),
    ).toHaveCount(1);
    const plan = await movePlan(
      app,
      { volume: 'photos', dir: 'system-default-source' },
      { volume: 'photos', dir: 'system-default-target' },
      null,
    );
    const task = await executePlan(app, plan);
    expect((await terminalTask(app, task.id)).state).toBe('done');
    const target = app.volumePath('photos/system-default-target/collision.jpg');
    expect(await readFile(target)).toEqual(policy === 'replace' ? incoming : existing);
    if (policy === 'skip') {
      expect(await readFile(app.volumePath('photos/system-default-source/collision.jpg'))).toEqual(
        incoming,
      );
    } else {
      const listing = await app.api.listing('photos', 'system-default-target');
      const restored = await Promise.all(
        listing.items.map((item) =>
          indexedFile(app, 'photos', `system-default-target/${item.name}`),
        ),
      );
      expect(restored.some((file) => file.hash === files[0].sha1)).toBe(true);
    }
  });
}
