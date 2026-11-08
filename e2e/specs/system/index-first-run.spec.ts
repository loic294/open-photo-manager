import { basename } from 'node:path';
import { expect, test } from '../../src/test';

test.use({
  appOptions: { cold: true },
  // Navigating/reloading cancels the old page's SSE and viewer thumbnail preload.
  allowedErrors: [
    [
      /requestfailed: .*\/api\/events net::ERR_ABORTED/,
      /requestfailed: .*\/api\/files\/\d+\/(?:thumb|preview).* net::ERR_ABORTED/,
    ],
    { scope: 'test' },
  ],
});

test('cold startup responds while indexing and leaves the browser usable @slow', async ({
  app,
  page,
  shell,
  source,
  filterBar,
  viewer,
  manifest,
}) => {
  test.slow();
  await page.addInitScript(() => {
    const history: string[] = [];
    const observer = new MutationObserver(() => {
      const label = document.querySelector('apm-task-indicator')?.textContent ?? '';
      if (label.includes('Indexing:') && history.at(-1) !== label) history.push(label);
      if (document.documentElement)
        document.documentElement.dataset.indexHistory = JSON.stringify(history);
    });
    observer.observe(document, { subtree: true, childList: true, characterData: true });
  });
  const expectedCount = manifest.data.files.filter(
    (file) => file.indexable && !file.skipped,
  ).length;
  await page.goto(`${app.url}/browse`);
  await expect(shell.reindexButton()).toBeEnabled();
  await expect(shell.taskTrigger()).toBeVisible({ timeout: 10_000 });
  await shell.hoverTasks();
  await expect(shell.taskPanel()).toBeVisible();
  await page.keyboard.press('Escape');
  await source.openFolder('photos');
  await filterBar.rating(3);
  await expect(filterBar.element.getByRole('button', { name: /^3 stars/ })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  await filterBar.clear();
  await expect
    .poll(() => Number(app.sql('SELECT count(*) AS n FROM files')[0]?.n), { timeout: 30_000 })
    .toBeGreaterThan(0);
  const firstCount = Number(app.sql('SELECT count(*) AS n FROM files')[0]?.n);
  expect(firstCount).toBeLessThanOrEqual(expectedCount);
  const target = manifest.filesOf('exif.full_nikon')[0];
  await expect(source.folder('2021')).toBeVisible();
  await source.openFolder('2021');
  await source.openFolder('03');
  await source.card(basename(target.path)).click();
  await viewer.expectOpen(basename(target.path));
  await viewer.close();
  await app.waitForIdle();
  const history = JSON.parse(
    (await page.locator('html').getAttribute('data-index-history')) ?? '[]',
  ) as string[];
  expect(history.join('\n')).toContain('Indexing: scanning folders');
  expect(history.join('\n')).toContain('Indexing: creating thumbnails');
  expect(Number(app.sql('SELECT count(*) AS n FROM files')[0]?.n)).toBe(expectedCount);
  await expect(shell.taskTrigger()).toHaveCount(0, { timeout: 10_000 });
});
