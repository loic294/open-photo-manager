import { formatFileSize } from '../../../web/src/util/format';
import { writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';

declare global {
  interface Window {
    __e2eLongTasks: number[];
  }
}

function isResponse(path: RegExp) {
  return (response: import('@playwright/test').Response) =>
    response.request().method() === 'POST' && path.test(new URL(response.url()).pathname);
}

test('duplicate and move tools stay responsive with 3,000 indexed files', async ({
  app,
  page,
  source,
  bottomBar,
  movePage,
  confirmDialog,
}) => {
  test.setTimeout(300_000);
  const bytes = 'A private 3,000-file performance fixture';
  await Promise.all(
    Array.from({ length: 3_000 }, (_, index) =>
      writeFile(app, 'photos', `e2e-performance/ITEM_${String(index).padStart(4, '0')}.txt`, bytes),
    ),
  );
  await app.rescan();
  expect((await app.api.listing('photos', 'e2e-performance')).total).toBe(3_000);

  await page.addInitScript(() => {
    window.__e2eLongTasks = [];
    new PerformanceObserver((list) => {
      window.__e2eLongTasks.push(...list.getEntries().map((entry) => entry.duration));
    }).observe({ type: 'longtask', buffered: true });
  });

  const duplicatesPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  const duplicatesUrl = new URL('/duplicates', app.url);
  duplicatesUrl.searchParams.set('src', 'photos/e2e-performance');
  duplicatesUrl.searchParams.set('cmp', 'archive');
  await page.goto(duplicatesUrl.href);
  await duplicatesPlan;
  await source.waitReady();
  await page.evaluate(() => {
    window.__e2eLongTasks = [];
  });
  const duplicateSelectStarted = performance.now();
  await bottomBar.selectAll('source');
  await expect
    .poll(() =>
      page.getByRole('group', { name: 'Source pane' }).getByRole('status').allTextContents(),
    )
    .toContain('3,000 selected');
  expect(performance.now() - duplicateSelectStarted).toBeLessThan(1_000);
  expect(await page.evaluate(() => Math.max(0, ...window.__e2eLongTasks))).toBeLessThan(1_000);
  await source.clear();

  const moveUrl = new URL('/move', app.url);
  moveUrl.searchParams.set('src', 'photos/e2e-performance');
  moveUrl.searchParams.set('cmp', 'archive');
  await page.goto(moveUrl.href);
  await expect(movePage.planNotices()).toContainText('Choose the source folder');
  const sourcePlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await source.useFolder();
  await sourcePlan;
  await expect(movePage.planNotices()).toContainText('Choose the destination folder');
  const destinationPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await movePage.destinationBase().getByRole('button', { name: 'Use this folder' }).click();
  await destinationPlan;
  const customPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await movePage.chooseMoveType('custom');
  await customPlan;
  await movePage.openPattern();

  const requestCounts = { plans: 0, previews: 0 };
  page.on('request', (request) => {
    if (request.method() !== 'POST') return;
    const path = new URL(request.url()).pathname;
    if (path === '/api/plans') requestCounts.plans++;
    if (path === '/api/format/preview') requestCounts.previews++;
  });
  await page.evaluate(() => {
    window.__e2eLongTasks = [];
  });
  const finalPreview = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  const finalPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await movePage.pattern().fill('first/{name}');
  await movePage.pattern().fill('second/{name}');
  await movePage.pattern().fill('formatted/{filename}');
  await finalPreview;
  await finalPlan;
  await expect(movePage.actionButton()).toBeEnabled();
  expect(requestCounts.previews).toBe(1);
  expect(requestCounts.plans).toBeLessThanOrEqual(1);
  expect(await page.evaluate(() => Math.max(0, ...window.__e2eLongTasks))).toBeLessThan(1_000);

  await page.evaluate(() => {
    window.__e2eLongTasks = [];
  });
  const moveSelectStarted = performance.now();
  await bottomBar.selectAll('source');
  await expect
    .poll(() =>
      page.getByRole('group', { name: 'Source pane' }).getByRole('status').allTextContents(),
    )
    .toContain('3,000 selected');
  expect(performance.now() - moveSelectStarted).toBeLessThan(1_000);
  expect(await page.evaluate(() => Math.max(0, ...window.__e2eLongTasks))).toBeLessThan(1_000);

  const selectionResponse = page.waitForResponse(isResponse(/^\/api\/plans\/\d+\/selection$/));
  await movePage.actionButton().click();
  const selection = (await (await selectionResponse).json()) as {
    item_count: number;
    total_size: number;
  };
  expect(selection.item_count).toBe(3_000);
  expect(selection.total_size).toBe(3_000 * bytes.length);
  await expect(confirmDialog.dialog()).toContainText('Move 3,000 files');
  await expect(confirmDialog.dialog()).toContainText(formatFileSize(selection.total_size));
});
