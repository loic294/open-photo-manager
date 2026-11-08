import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { captureScreens } from '../../src/screens';
import { expect, test } from '../../src/test';

// Closing the viewer cancels its preview and thumbnail preload requests.
test.use({
  allowedErrors: [/^requestfailed: .*\/api\/files\/\d+\/(?:preview|thumb\?.*) net::ERR_ABORTED$/],
});

function isPlan(response: import('@playwright/test').Response) {
  return (
    response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans'
  );
}

async function captureToolScreens(page: import('@playwright/test').Page, name: string) {
  const original = page.viewportSize();
  for (const viewport of [
    { width: 1440, height: 900 },
    { width: 1024, height: 700 },
  ]) {
    await page.setViewportSize(viewport);
    await captureScreens(page, `${name}-${viewport.width}x${viewport.height}`);
  }
  if (original) await page.setViewportSize(original);
}

for (const [tool, slug, path] of [
  ['Duplicate finder', 'duplicates', '/duplicates?src=photos/dupes/m1&cmp=archive/m1'],
  ['Folder formatting', 'format', '/format?src=photos/2022/08'],
  ['Empty folder cleanup', 'cleanup', '/cleanup?src=photos/empty&cmp=photos/empty'],
] as const) {
  test(`${tool} loaded pane has no serious accessibility violations`, async ({
    sharedApp,
    page,
    source,
    duplicatesPage,
    confirmDialog,
    viewer,
  }) => {
    const planResponse = page.waitForResponse(isPlan);
    await page.goto(new URL(path, sharedApp.url).href);
    await planResponse;
    await expect(page.locator('apm-pane [data-pane-body][aria-busy="true"]')).toHaveCount(0);
    await expectNoSeriousA11yViolations(page);
    if (slug === 'duplicates') {
      const selectionResponse = page.waitForResponse(
        (response) =>
          response.request().method() === 'POST' &&
          /^\/api\/plans\/\d+\/selection$/.test(new URL(response.url()).pathname),
      );
      await duplicatesPage.actionButton().click();
      await selectionResponse;
      await expect(confirmDialog.dialog()).toBeVisible();
      await expectNoSeriousA11yViolations(page);
      await confirmDialog.cancel();
      await source.card('IMG_0300.jpg').click();
      await viewer.expectOpen('IMG_0300.jpg');
      await expectNoSeriousA11yViolations(page);
      await page.keyboard.press('Escape');
      await expect(viewer.dialog()).not.toBeVisible();
    }
    await captureToolScreens(page, slug);
  });
}

test('Duplicate finder action menu is operable with the keyboard', async ({
  sharedApp,
  page,
  duplicatesPage,
}) => {
  const planResponse = page.waitForResponse(isPlan);
  await page.goto(new URL('/duplicates?src=photos/dupes/m1&cmp=archive/m1', sharedApp.url).href);
  await planResponse;
  const chooseAction = duplicatesPage.actionMenu().locator('summary[aria-label="Choose action"]');
  await chooseAction.focus();
  await page.keyboard.press('Enter');
  const moveAction = duplicatesPage.actionMenu().getByRole('menuitemradio', {
    name: 'Move files',
    exact: true,
  });
  await expect(moveAction).toBeVisible();
  await moveAction.focus();
  await page.keyboard.press('Enter');
  await expect(duplicatesPage.actionButton()).toContainText('Move files');
});

test('Asset move ghosts have no serious accessibility violations', async ({
  sharedApp,
  page,
  source,
  movePage,
}) => {
  const url = new URL('/move', sharedApp.url);
  url.searchParams.set('src', 'photos/dupes/m1');
  url.searchParams.set('cmp', 'archive');
  await page.goto(url.href);
  await source.useFolder();
  const destinationPlan = page.waitForResponse(isPlan);
  await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
  await destinationPlan;
  const flatPlan = page.waitForResponse(isPlan);
  await movePage.chooseMoveType('flat');
  await flatPlan;
  await expect(movePage.destinationCard('IMG_0300.jpg').element).toBeVisible();
  await expect(page.locator('apm-pane [data-pane-body][aria-busy="true"]')).toHaveCount(0);
  await expectNoSeriousA11yViolations(page);
  await captureToolScreens(page, 'move');
});
