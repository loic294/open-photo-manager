import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { expect, test } from '../../src/test';
import { expectedEmptyDirs } from '../../src/tool-oracles';

function isPlan(response: import('@playwright/test').Response) {
  return (
    response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans'
  );
}

test('@smoke cleanup preview marks exactly the empty subtree', async ({
  app,
  page,
  manifest,
  comparison,
  cleanupPage,
}) => {
  const expected = expectedEmptyDirs(manifest, { volume: 'photos', dir: 'empty' });
  const planResponse = page.waitForResponse(isPlan);
  const url = new URL('/cleanup', app.url);
  url.searchParams.set('src', 'photos/empty');
  url.searchParams.set('cmp', 'photos/empty');
  await page.goto(url.href);
  const plan = (await (await planResponse).json()) as PlanDto;
  expect(plan.stats.empty_folders).toBe(expected.length);

  for (const name of ['nested', 'chain', 'cascade']) {
    await expect.poll(() => comparison.folderState(name)).toMatchObject({ red: true });
  }
  await expect.poll(() => comparison.folderState('mixed')).toMatchObject({ red: false });
  await comparison.openFolder('nested');
  await expect.poll(() => comparison.folderState('a')).toMatchObject({ red: true });
  await comparison.openFolder('a');
  await expect.poll(() => comparison.folderState('b')).toMatchObject({ red: true });
  await comparison.openFolder('b');
  await expect.poll(() => comparison.folderState('c')).toMatchObject({ red: true });

  const directPlanResponse = page.waitForResponse(isPlan);
  await cleanupPage.includeSubfolders().setChecked(false);
  const directPlan = (await (await directPlanResponse).json()) as PlanDto;
  expect(directPlan.stats.empty_folders).toBe(
    expectedEmptyDirs(manifest, { volume: 'photos', dir: 'empty', recursive: false }).length,
  );
  expect(directPlan.stats.empty_folders).toBeLessThan(plan.stats.empty_folders ?? 0);
  await expect(cleanupPage.actionButton()).toContainText(
    `Delete empty folders (${directPlan.stats.empty_folders})`,
  );
});
