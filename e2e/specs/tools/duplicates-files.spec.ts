import { existsSync } from 'node:fs';
import { join } from 'node:path';
import type { DuplicateFileGroups } from '../../../web/src/api/generated/DuplicateFileGroups';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { captureScreens } from '../../src/screens';
import { expect, test } from '../../src/test';

test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('duplicate file view compares copies, highlights metadata, prioritizes rules and trashes only removals', async ({
  app,
  page,
  manifest,
  source,
  comparison,
  duplicatesPage,
  confirmDialog,
}) => {
  const fixture = manifest
    .duplicateGroups(1)
    .find((group) => group.source && group.members.some((member) => member.match));
  if (!fixture?.source) throw new Error('Missing hash duplicate fixture');
  const file = manifest.file(fixture.source.volume, fixture.source.path);
  const dir = file.path.includes('/') ? file.path.slice(0, file.path.lastIndexOf('/')) : '';
  const url = new URL('/duplicates', app.url);
  url.searchParams.set('src', `${file.volume}/${dir}`);
  await page.goto(url.href);
  await source.waitReady();
  await comparison.waitReady();
  await duplicatesPage.showFiles();
  const response = await duplicatesPage.findFiles();
  expect(response.ok()).toBe(true);
  const data = (await response.json()) as DuplicateFileGroups;
  expect(data.groups.length).toBeGreaterThan(0);
  const first = data.groups[0];
  if (!first) throw new Error('Missing first group');
  const group = duplicatesPage
    .files()
    .getByRole('article', { name: 'Duplicate group 1', exact: true });
  await expect(group.locator('opm-file-card')).toHaveCount(first.files.length);
  expect(
    first.files.some(
      (copy) => copy.item.dir !== dir || copy.item.volume_id !== first.files[0]?.item.volume_id,
    ),
  ).toBe(true);

  for (const [field, key] of [
    ['name', 'name'],
    ['size', 'size'],
    ['date', 'capture_ts'],
  ] as const) {
    const differs = new Set(first.files.map((copy) => copy.item[key])).size > 1;
    for (const element of await group.locator(`[data-copy-${field}]`).all()) {
      if (differs) await expect(element).toHaveClass(/font-bold/);
      else await expect(element).not.toHaveClass(/font-bold/);
    }
  }
  for (const [index, copy] of first.files.entries()) {
    const rating = group.locator('[data-copy-rating]').nth(index);
    if (copy.item.rating === 0) await expect(rating).toHaveText('Not rated');
    else
      await expect(rating.getByRole('img')).toHaveAttribute(
        'aria-label',
        `${copy.item.rating} out of 5 stars`,
      );
    await expect(group.locator('[data-copy-flag]').nth(index)).toHaveText(
      copy.item.flag > 0 ? 'Selected' : copy.item.flag < 0 ? 'Rejected' : 'None',
    );
  }
  await group.locator('opm-file-card').last().click();
  await expect(group.locator('opm-file-card').last().getByRole('button')).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  await expect(group.locator('opm-file-card').last().getByRole('button')).toHaveClass(
    /border-success/,
  );
  await duplicatesPage
    .files()
    .getByRole('slider', { name: 'Duplicate thumbnail size' })
    .fill('360');
  await expect(duplicatesPage.files().getByRole('slider')).toHaveValue('360');

  await group.locator('opm-file-card').last().click({ button: 'right' });
  await page
    .locator('[data-duplicate-menu]')
    .getByRole('button', { name: 'Keep files from this folder' })
    .click();
  const folder = page.getByRole('dialog', { name: 'Keep files from this folder', exact: true });
  await expect(folder.getByRole('combobox').locator('option').first()).toHaveText(/.+/);
  await folder.getByRole('radio', { name: 'This folder and all subfolders', exact: true }).check();
  await folder.getByRole('button', { name: 'Apply to all groups' }).click();
  await duplicatesPage
    .files()
    .getByRole('button', { name: 'Automatic Selection', exact: true })
    .click();
  const automatic = duplicatesPage.automaticDialog();
  await expect(automatic.locator('li').first()).toContainText('Is in folder or subfolder');
  await automatic
    .locator('li')
    .nth(1)
    .dragTo(automatic.locator('li').first(), {
      sourcePosition: { x: 12, y: 12 },
      targetPosition: { x: 12, y: 12 },
    });
  await expect(automatic.locator('li').first()).toContainText('Capture date');
  await automatic.getByRole('button', { name: 'Move File size up', exact: true }).click();
  await automatic.getByRole('checkbox', { name: 'File size', exact: true }).check();
  await automatic
    .getByRole('combobox', { name: 'File size order', exact: true })
    .selectOption('desc');
  await expectNoSeriousA11yViolations(page);
  await automatic.getByRole('button', { name: 'Apply to all groups' }).click();

  await expectNoSeriousA11yViolations(page);
  await captureScreens(page, 'duplicate-files-desktop');
  await page.setViewportSize({ width: 390, height: 844 });
  await expectNoSeriousA11yViolations(page);
  await captureScreens(page, 'duplicate-files-mobile');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  await page.setViewportSize({ width: 1440, height: 1000 });

  const retained = await duplicatesPage
    .files()
    .locator('opm-file-card')
    .evaluateAll((cards) =>
      cards
        .filter((card) => card.querySelector('[aria-pressed="true"]'))
        .map((card) => (card as HTMLElement & { item: { id: number } }).item.id),
    );
  expect(retained.length).toBeGreaterThan(0);
  const execute = page.waitForResponse((response) =>
    new URL(response.url()).pathname.endsWith('/execute'),
  );
  await duplicatesPage
    .files()
    .getByRole('button', { name: /^Trash \d+ duplicates$/ })
    .click();
  await expect(confirmDialog.dialog()).toContainText('including copies outside your source folder');
  await confirmDialog
    .dialog()
    .getByRole('button', { name: /^Trash \d+ files$/ })
    .click();
  const execution = await execute;
  expect(execution.ok()).toBe(true);
  const planId = Number(new URL(execution.url()).pathname.split('/')[3]);
  await app.waitForIdle();
  const plan = await app.api.request<PlanDto>('GET', `/api/plans/${planId}`);
  expect(plan.result?.failed).toBe(0);
  expect(plan.result?.skipped).toBe(0);
  expect(plan.result?.done).toBe(
    data.groups.flatMap((entry) => entry.files).length - retained.length,
  );
  const volumes = await app.api.volumes();
  for (const copy of data.groups.flatMap((entry) => entry.files)) {
    const volume = volumes.find((volume) => volume.id === copy.item.volume_id);
    if (!volume) throw new Error('Missing copy volume');
    expect(existsSync(join(app.volumesDir, volume.name, copy.item.dir, copy.item.name))).toBe(
      retained.includes(copy.item.id),
    );
  }
});

test('duplicate file view excludes an outside volume and can include it again', async ({
  app,
  page,
  manifest,
  source,
  comparison,
  duplicatesPage,
}) => {
  const fixture = manifest
    .duplicateGroups(1)
    .find((group) => group.source && group.members.some((member) => member.match));
  if (!fixture?.source) throw new Error('Missing hash duplicate fixture');
  const file = manifest.file(fixture.source.volume, fixture.source.path);
  const dir = file.path.includes('/') ? file.path.slice(0, file.path.lastIndexOf('/')) : '';
  const url = new URL('/duplicates', app.url);
  url.searchParams.set('src', `${file.volume}/${dir}`);
  await page.goto(url.href);
  await source.waitReady();
  await comparison.waitReady();
  await duplicatesPage.showFiles();
  const initial = await duplicatesPage.findFiles();
  expect(initial.ok()).toBe(true);
  const before = (await initial.json()) as DuplicateFileGroups;
  const volumes = await app.api.volumes();
  const outside = volumes.find(
    (volume) =>
      volume.name !== file.volume &&
      before.groups.some((group) => group.files.some((copy) => copy.item.volume_id === volume.id)),
  );
  if (!outside) throw new Error('Missing matching copies on another volume');
  await duplicatesPage.files().getByRole('button', { name: 'Exclude folder', exact: true }).click();
  await duplicatesPage.pickFolder('Exclude folder and subfolders', [outside.name]);
  const excluded = await duplicatesPage.findFiles();
  expect(excluded.ok()).toBe(true);
  const after = (await excluded.json()) as DuplicateFileGroups;
  expect(after.groups.flatMap((group) => group.files).length).toBeLessThan(
    before.groups.flatMap((group) => group.files).length,
  );
  expect(
    after.groups.some((group) => group.files.some((copy) => copy.item.volume_id === outside.id)),
  ).toBe(false);
  await duplicatesPage
    .files()
    .getByRole('button', { name: `Stop excluding ${outside.name}/`, exact: true })
    .click();
  const restored = await duplicatesPage.findFiles();
  expect(restored.ok()).toBe(true);
  expect((await restored.json()) as DuplicateFileGroups).toEqual(before);
});
