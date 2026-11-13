import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import type { StatisticsResponse } from '../../../web/src/api/generated/StatisticsResponse';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { writeFile } from '../../src/disk';
import { StatisticsPage } from '../../src/pages/statistics';
import { captureScreens } from '../../src/screens';
import { expect, test } from '../../src/test';

test.use({
  allowedErrors: [[/requestfailed: .*\/api\/events net::ERR_ABORTED/], { scope: 'test' }],
});

test('statistics records actual indexed changes, filters volumes and retains timezone history', async ({
  app,
  page,
  settingsPage,
}) => {
  const statistics = new StatisticsPage(page);
  const history = () => app.api.request<StatisticsResponse>('GET', '/api/statistics');
  const total = Number(app.sql('SELECT COUNT(*) AS n FROM files')[0]?.n);
  const volume = (await app.api.volumes()).find((value) => value.name === 'photos');
  if (!volume) throw new Error('Missing fixture photos volume');
  await page.goto(`${app.url}/statistics`);
  await expect(statistics.root().getByRole('heading', { name: 'Library over time' })).toBeVisible();
  await expect(statistics.daily()).toContainText('Today');
  expect((await history()).snapshots.reduce((sum, row) => sum + row.inventory.files, 0)).toBe(
    total,
  );
  const before = (await history()).activity
    .filter((row) => row.action === 'added')
    .reduce((sum, row) => sum + row.count, 0);
  const path = join(app.volumesDir, 'photos', 'statistics-new.txt');
  await writeFile(app, 'photos', 'statistics-new.txt', 'new library file');
  await app.rescan();
  await expect
    .poll(async () =>
      (await history()).activity
        .filter((row) => row.action === 'added')
        .reduce((sum, row) => sum + row.count, 0),
    )
    .toBe(before + 1);
  expect(await readFile(path, 'utf8')).toBe('new library file');
  await statistics.volume(volume.id);
  await expect(statistics.volumes().getByRole('row')).toHaveCount(2);
  await expect(statistics.volumes()).toContainText('photos');
  await expect(statistics.root().locator('[aria-busy]')).toHaveAttribute('aria-busy', 'false');
  await expectNoSeriousA11yViolations(page);
  await captureScreens(page, 'statistics-desktop');
  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator('html').evaluate((element) => element.setAttribute('data-theme', 'dark'));
  await expectNoSeriousA11yViolations(page);
  await captureScreens(page, 'statistics-mobile');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  await page.goto(`${app.url}/settings`);
  await settingsPage.fill('Reporting timezone', 'Europe/Paris');
  await settingsPage.saveGeneral();
  await expect
    .poll(async () => (await app.api.settings()).statistics_timezone)
    .toBe('Europe/Paris');
  await page.goto(`${app.url}/statistics`);
  await expect(statistics.root()).toContainText('Europe/Paris');
  expect((await history()).snapshots.some((row) => row.timezone === 'America/Los_Angeles')).toBe(
    true,
  );
  await page.reload();
  await expect(statistics.root()).toContainText('Europe/Paris');
});
