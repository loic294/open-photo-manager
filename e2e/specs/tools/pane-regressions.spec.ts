import { RenamePage } from '../../src/pages/rename-page';
import { captureScreens } from '../../src/screens';
import { expect, test } from '../../src/test';

test('format pins a browsed destination despite file-time warnings', async ({
  app,
  page,
  comparison,
  formatPage,
}) => {
  await page.goto(`${app.url}/format?src=photos/sidecars`);
  await expect(formatPage.actionButton()).toBeEnabled();
  await comparison.element.getByRole('button', { name: 'Up one folder' }).click();
  await comparison.openFolder('empty');
  await comparison.useFolder();
  await expect(comparison.base()).toContainText('photos/empty');
  await expect(comparison.base()).not.toContainText('Same as the source folder');
  await expect(formatPage.actionButton()).toBeEnabled();
});

test('rename previews new names in the comparison pane', async ({
  app,
  page,
  source,
  comparison,
  bottomBar,
}) => {
  await page.goto(`${app.url}/rename?src=photos/sidecars`);
  await expect(source.card('IMG_0001.CR3').element).toBeVisible();
  await new RenamePage(page).pattern('Renamed_{name}');
  await expect(comparison.card('Renamed_IMG_0001.CR3').element).toBeVisible();
  await expect(comparison.card('Renamed_IMG_0001.CR3').element).toHaveAttribute(
    'aria-disabled',
    'true',
  );
  await expect(comparison.card('IMG_0001.CR3').element).toHaveCount(0);
  await expect(
    bottomBar.side('comparison').getByRole('button', { name: 'Select all' }),
  ).toBeDisabled();
});

test('footer divisions align with pane divisions with actions present', async ({
  app,
  page,
  comparison,
  bottomBar,
  formatPage,
}) => {
  await page.goto(`${app.url}/format?src=photos/sidecars`);
  await expect(formatPage.actionButton()).toBeEnabled();
  await expect(comparison.element).toBeVisible();
  const pane = await comparison.element.boundingBox();
  const footer = await bottomBar.side('comparison').boundingBox();
  expect(pane).not.toBeNull();
  expect(footer).not.toBeNull();
  expect(Math.abs((pane?.x ?? 0) - (footer?.x ?? 0))).toBeLessThanOrEqual(1);
  await captureScreens(page, 'pane-footer-desktop');
});

test('sync navigation links both folder locations, scales and scrolling', async ({
  app,
  page,
  source,
  comparison,
  bottomBar,
}) => {
  await page.goto(`${app.url}/browse?src=photos/sidecars&cmp=archive`);
  const sync = page.getByRole('switch', { name: 'Sync navigation', exact: true });
  await sync.setChecked(true);
  await expect(comparison.card('IMG_0001.CR3').element).toBeVisible();
  await bottomBar.size('source', 224);
  await expect.poll(() => bottomBar.sizeValue('comparison')).toBe('224');
  await source.scrollTo(160);
  await expect.poll(() => comparison.scrollTop()).toBe(160);
  await comparison.scrollTo(240);
  await expect.poll(() => source.scrollTop()).toBe(240);
  await comparison.element.getByRole('button', { name: 'Up one folder' }).click();
  await expect(source.breadcrumbs()).not.toContainText('sidecars');
  await expect(comparison.breadcrumbs()).not.toContainText('sidecars');
  await source.openFolder('sidecars');
  await expect(comparison.card('IMG_0001.CR3').element).toBeVisible();
  await page.goBack();
  await expect(source.breadcrumbs()).not.toContainText('sidecars');
  await expect(comparison.breadcrumbs()).not.toContainText('sidecars');
  await page.goForward();
  await expect(source.card('IMG_0001.CR3').element).toBeVisible();
  await expect(comparison.card('IMG_0001.CR3').element).toBeVisible();
  const name = source.card('IMG_0001.CR3').element.locator('[data-name]');
  await expect(name).toHaveAttribute('data-tip', 'IMG_0001.CR3');
  await name.hover();
  expect(await name.evaluate((element) => getComputedStyle(element, '::before').visibility)).toBe(
    'visible',
  );
  const date = source.card('IMG_0001.CR3').date();
  await expect(date).toHaveAttribute('data-tip', /· \d+(?:\.\d+)? [KMGT]?B ·/);
  await date.hover();
  expect(await date.evaluate((element) => getComputedStyle(element, '::before').visibility)).toBe(
    'visible',
  );
  await sync.setChecked(false);
  await comparison.element.getByRole('button', { name: 'Up one folder' }).click();
  await expect(source.breadcrumbs()).toContainText('sidecars');
});

test('duplicate actions are inline and the narrow footer does not overflow', async ({
  app,
  page,
  bottomBar,
}) => {
  await page.goto(`${app.url}/duplicates?src=photos/dupes/m1&cmp=archive/m1`);
  await expect(bottomBar.splitButton()).toBeVisible();
  await expect(bottomBar.actionButton()).toBeEnabled();
  await expect(page.locator('[data-floating-actions]')).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(bottomBar.element).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  for (const side of ['source', 'comparison'] as const) {
    const counts = bottomBar.side(side).getByRole('status').first();
    await expect(counts).toContainText('in folder');
    expect(await counts.evaluate((element) => element.clientWidth)).toBeGreaterThan(80);
  }
  await captureScreens(page, 'pane-footer-mobile');
});
