import { fixture } from '../src/scenarios';
import { captureScreens } from '../src/screens';
import { expect, test } from '../src/test';

test.use({
  allowedErrors: [
    [
      /^requestfailed: .*\/api\/files\/\d+\/(?:preview|thumb\?.*) net::ERR_ABORTED$/,
      /^requestfailed: .*\/api\/events net::ERR_ABORTED$/,
    ],
    { scope: 'test' },
  ],
});

for (const width of [1440, 390]) {
  test(`compact duplicate actions leave both footer segments intact at ${width}px`, async ({
    page,
    sharedApp,
    duplicatesPage,
    bottomBar,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`${sharedApp.url}/duplicates?src=photos/dupes/m1&cmp=archive/m1`);
    await expect(duplicatesPage.actionButton()).toBeEnabled();
    const action = await duplicatesPage.actionButton().boundingBox();
    const footer = await bottomBar.element.boundingBox();
    expect(action).not.toBeNull();
    expect(footer).not.toBeNull();
    expect(action?.height).toBeLessThanOrEqual(40);
    expect((action?.y ?? 0) + (action?.height ?? 0)).toBeLessThan(footer?.y ?? 0);
    await expect(bottomBar.element.locator('opm-split-button')).toHaveCount(0);
    for (const label of ['Source pane', 'Comparison pane']) {
      await expect(bottomBar.element.getByRole('group', { name: label })).toBeVisible();
    }
    await captureScreens(page, `compact-duplicates-${width}`);
  });
}

test('tool header reveals help and filters without repeating its name', async ({
  page,
  sharedApp,
  filterBar,
}) => {
  await page.goto(`${sharedApp.url}/browse?src=photos`);
  const toggle = page.getByRole('button', { name: 'Toggle filters', exact: true });
  await expect(toggle).toHaveAttribute('aria-expanded', 'false');
  await expect(page.locator('#tool-filters')).toBeHidden();
  await expect(page.locator('opm-tool-view > header h2')).toHaveCount(0);
  const info = page.getByRole('button', { name: 'About this tool', exact: true });
  await info.click();
  const help = page.getByRole('dialog', { name: 'Browse', exact: true });
  await expect(help).toBeVisible();
  await expect(help).toContainText('How to use it');
  await expect(help.locator('li')).toHaveCount(3);
  await page.keyboard.press('Escape');
  await expect(help).not.toBeVisible();
  await expect(info).toBeFocused();
  await filterBar.open();
  await filterBar.rating(3);
  await toggle.click();
  await expect(page.locator('#tool-filters')).toBeHidden();
  await toggle.click();
  await expect(filterBar.element.getByRole('button', { name: /^3 stars/ })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
});

test('formatting editor lives in a modal and keeps its pattern in the toolbar', async ({
  page,
  sharedApp,
  formatPage,
}) => {
  await page.goto(`${sharedApp.url}/format?src=photos/dupes/m1`);
  await expect(page.locator('opm-tool-view > header code')).toHaveText('{YYYY}/{mm}');
  await expect(formatPage.pattern()).toBeHidden();
  await formatPage.edit();
  await expect(formatPage.pattern()).toBeVisible();
  await expect(formatPage.pattern()).toBeEnabled();
  await expect(formatPage.pattern()).toBeEditable();
  await formatPage.pattern().fill('{YYYY}/{filename}');
  await expect(formatPage.pattern()).toHaveAttribute('aria-invalid', 'false');
  await formatPage.livePreview().locator('summary').click();
  await expect(formatPage.previewRows()).not.toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 900 });
  await expect(formatPage.pattern()).toBeVisible();
  await captureScreens(page, 'format-modal-mobile');
  await formatPage.done();
  await expect(page.locator('opm-tool-view > header code')).toHaveText('{YYYY}/{filename}');
  await expect(formatPage.pattern()).toBeHidden();
  const recursive = page.getByRole('switch', { name: 'Include subfolders', exact: true });
  const edit = page.getByRole('button', { name: 'Edit formatting', exact: true });
  const recursiveBox = await recursive.boundingBox();
  const editBox = await edit.boundingBox();
  expect(recursiveBox?.x).toBeGreaterThan(editBox?.x ?? 0);
});

test('breadcrumbs collapse the middle, expose parent links and reveal the full path', async ({
  page,
  sharedApp,
  manifest,
  source,
}) => {
  const file = [...manifest.data.files]
    .filter((entry) => entry.indexable && !entry.skipped && entry.kind === 'image')
    .sort((a, b) => b.path.lastIndexOf('/') - a.path.lastIndexOf('/'))[0];
  if (!file) throw new Error('No indexed image fixture');
  const dir = file.path.slice(0, file.path.lastIndexOf('/'));
  const path = `${file.volume}/${dir}`;
  const url = new URL('/browse', sharedApp.url);
  url.searchParams.set('src', path);
  await page.setViewportSize({ width: 800, height: 900 });
  await page.goto(url.href);
  const crumbs = source.breadcrumbs();
  const middle = crumbs.getByText('...', { exact: true });
  await expect(middle).toBeVisible();
  await expect(middle).toHaveAttribute('data-tip', path);
  const current = crumbs.locator('a[aria-current="page"]');
  await expect(current).toBeVisible();
  await expect(current).toHaveText(dir.slice(dir.lastIndexOf('/') + 1));
  expect(await current.evaluate((element) => element.clientWidth >= element.scrollWidth)).toBe(
    true,
  );
  await expect(current.locator('..')).toHaveAttribute('data-tip', path);
  await middle.click();
  await expect(crumbs.locator('.dropdown-content a').first()).toBeVisible();
  await page.keyboard.press('Escape');
  await captureScreens(page, 'collapsed-path');
});

test('indexing and volumes use complete rows and viewer margins match the theme', async ({
  page,
  sharedApp,
  source,
  viewer,
  manifest,
}) => {
  await page.goto(`${sharedApp.url}/settings`);
  const form = page.getByRole('form', { name: 'General settings' });
  const sections = ['Files and sidecars', 'Indexing', 'Volumes'].map((name) =>
    page.getByRole('group', { name, exact: true }),
  );
  await expect(form).toBeVisible();
  const widths = await Promise.all(
    sections.map(async (section) => (await section.boundingBox())?.width),
  );
  expect(widths[1]).toBe(widths[0]);
  expect(widths[2]).toBe(widths[0]);
  const photo = fixture(manifest, 'exif.full_nikon');
  await source.goto(sharedApp.url, photo);
  await source.card(photo.name).click();
  await viewer.expectOpen(photo.name);
  for (const theme of ['light', 'dark']) {
    await page
      .locator('html')
      .evaluate((element, value) => element.setAttribute('data-theme', value), theme);
    const colors = await page.locator('opm-viewer-stage').evaluate((stage) => {
      const box = stage.closest('.modal-box');
      return {
        stage: getComputedStyle(stage).backgroundColor,
        box: box ? getComputedStyle(box).backgroundColor : null,
        primary: getComputedStyle(document.documentElement)
          .getPropertyValue('--color-primary')
          .trim(),
      };
    });
    expect(colors.stage).toBe(colors.box);
    expect(colors.primary).toBe('#ffc700');
    await expect(viewer.image()).toHaveClass(/object-contain/);
  }
  await captureScreens(page, 'theme-viewer');
});
