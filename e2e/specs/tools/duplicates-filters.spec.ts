import { readFile } from 'node:fs/promises';
import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';

function isPlan(response: import('@playwright/test').Response) {
  return (
    response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans'
  );
}

function matchedCount(pane: import('../../src/pages/pane').PanePage) {
  return pane.element
    .getByRole('option')
    .evaluateAll(
      (rows) => rows.filter((row) => row.getAttribute('aria-label')?.includes(', matched')).length,
    );
}

test('duplicate filters apply to both panes and clearing restores the matches', async ({
  app,
  page,
  manifest,
  filterBar,
  source,
  comparison,
}) => {
  const fixtures = [
    manifest.file('photos', '2021/03/DSC_0001.jpg'),
    manifest.file('photos', '2022/08/IMG_1109.jpg'),
    manifest.file('photos', '2022/08/IMG_1110.jpg'),
  ];
  test.skip(
    fixtures.some((file) => file.skipped),
    fixtures.find((file) => file.skipped)?.skip_reason ?? 'fixture scenario was skipped',
  );
  for (const [index, file] of fixtures.entries()) {
    test.skip(file.skipped, file.skip_reason ?? 'fixture scenario was skipped');
    const bytes = await readFile(app.volumePath(`${file.volume}/${file.path}`));
    const name = `filter-${index}.jpg`;
    await writeFile(app, 'photos', `e2e-filter/source/${name}`, bytes);
    await writeFile(app, 'archive', `e2e-filter/comparison/${name}`, bytes);
  }
  await app.rescan();

  const cameraFile = fixtures[0];
  const ratedFile = fixtures[1];
  if (!cameraFile || !ratedFile) throw new Error('Expected the duplicate filter fixtures');
  const url = new URL('/duplicates', app.url);
  url.searchParams.set('src', 'photos/e2e-filter/source');
  url.searchParams.set('cmp', 'archive/e2e-filter/comparison');
  const initialResponse = page.waitForResponse(isPlan);
  await page.goto(url.href);
  const initial = (await (await initialResponse).json()) as PlanDto;
  expect(initial.stats.source_matched).toBe(fixtures.length);
  expect(initial.stats.comparison_matched).toBe(fixtures.length);
  await source.waitReady();
  await comparison.waitReady();

  const checks = [
    {
      expected: fixtures.filter((file) => file.camera_expected === cameraFile.camera_expected)
        .length,
      apply: () => filterBar.camera(cameraFile.camera_expected ?? ''),
    },
    {
      expected: fixtures.filter((file) => file.rating_expected === ratedFile.rating_expected)
        .length,
      apply: () => filterBar.rating(ratedFile.rating_expected ?? 0),
    },
    {
      expected: fixtures.filter((file) => file.flag_expected === ratedFile.flag_expected).length,
      apply: () => filterBar.flag(ratedFile.flag_expected === 'rejected' ? 'Rejected' : 'Selected'),
    },
    {
      expected: fixtures.filter((file) =>
        (file.tags_expected ?? []).includes(ratedFile.tags_expected?.[0] ?? ''),
      ).length,
      apply: () => filterBar.tag(ratedFile.tags_expected?.[0] ?? ''),
    },
  ];
  for (const check of checks) {
    const filteredResponse = page.waitForResponse(isPlan);
    await check.apply();
    const filtered = (await (await filteredResponse).json()) as PlanDto;
    expect(filtered.stats.source_matched).toBe(check.expected);
    expect(filtered.stats.comparison_matched).toBe(check.expected);
    await expect.poll(() => matchedCount(source)).toBe(check.expected);
    await expect.poll(() => matchedCount(comparison)).toBe(check.expected);

    const clearedResponse = page.waitForResponse(isPlan);
    await filterBar.clear();
    const cleared = (await (await clearedResponse).json()) as PlanDto;
    expect(cleared.stats.source_matched).toBe(fixtures.length);
    expect(cleared.stats.comparison_matched).toBe(fixtures.length);
  }
});
