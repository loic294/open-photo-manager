import { expect, test } from '../src/test';

// A full reload/history navigation intentionally closes the old EventSource.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('volume root lists both mounted shares @smoke', async ({
  page,
  sharedApp,
  source,
  comparison,
}) => {
  await page.goto(sharedApp.url);
  await expect.poll(() => source.folderNames()).toEqual(['archive', 'photos']);
  expect(await comparison.folderNames()).toEqual(['archive', 'photos']);
  await expect(source.folder('photos')).toContainText('items');
});

test('folder navigation is independent and survives history and reload', async ({
  page,
  sharedApp,
  source,
  comparison,
}) => {
  await page.goto(sharedApp.url);
  await source.openFolder('photos');
  await comparison.openFolder('archive');
  expect(new URL(page.url()).searchParams.get('src')).toBe('photos');
  expect(new URL(page.url()).searchParams.get('cmp')).toBe('archive');
  await source.openFolder('media');
  await page.reload();
  await expect(source.breadcrumbs()).toContainText('media');
  await expect(comparison.breadcrumbs()).toContainText('archive');
  await page.goBack();
  await expect(source.breadcrumbs()).not.toContainText('media');
  await page.goForward();
  await expect(source.breadcrumbs()).toContainText('media');
});
