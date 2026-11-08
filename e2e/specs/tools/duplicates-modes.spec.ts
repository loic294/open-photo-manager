import type { PlanDto } from '../../../web/src/api/generated/PlanDto';
import { expect, test } from '../../src/test';
import { expectedMatches, type MatchMode } from '../../src/tool-oracles';

// Reloading the page closes the server-sent events connection.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

const modes: { label: string; mode: MatchMode; number: number }[] = [
  { label: 'Hash exact match', mode: 'hash', number: 1 },
  { label: 'Filename and time exact match', mode: 'name_and_time', number: 2 },
  { label: 'Filename exact match, different time', mode: 'name_different_time', number: 3 },
  { label: 'Filename prefix match', mode: 'name_prefix', number: 4 },
];

function parent(path: string) {
  const slash = path.lastIndexOf('/');
  return slash < 0 ? '' : path.slice(0, slash);
}

function paneLocation(volume: string, dir: string) {
  return `${volume}${dir ? `/${dir}` : ''}`;
}

async function matchedNames(pane: import('../../src/pages/pane').PanePage) {
  return pane.element.getByRole('option').evaluateAll((rows) =>
    rows
      .filter((row) => row.getAttribute('aria-label')?.includes(', matched'))
      .map((row) => row.getAttribute('aria-label')?.split(',')[0]?.trim() ?? '')
      .filter(Boolean)
      .sort(),
  );
}

for (const { label, mode, number } of modes) {
  test(`@smoke duplicate preview: ${label}`, async ({
    app,
    page,
    manifest,
    source,
    comparison,
    duplicatesPage,
  }) => {
    const group = manifest
      .duplicateGroups(number)
      .find((candidate) => candidate.source && candidate.members.some((member) => member.match));
    expect(group, `fixture group for ${label}`).toBeDefined();
    const sourceFile = manifest.file(group?.source?.volume ?? '', group?.source?.path ?? '');
    const counterpart = group?.members.find((member) => member.match);
    expect(counterpart, `matched fixture for ${label}`).toBeDefined();
    const comparisonFile = manifest.file(counterpart?.volume ?? '', counterpart?.path ?? '');
    const sourceDir = parent(sourceFile.path);
    const comparisonDir = parent(comparisonFile.path);
    const planResponse = page.waitForResponse(
      (response) =>
        response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/plans',
    );
    const url = new URL('/duplicates', app.url);
    url.searchParams.set('src', paneLocation(sourceFile.volume, sourceDir));
    url.searchParams.set('cmp', paneLocation(comparisonFile.volume, comparisonDir));
    await page.goto(url.href);
    let plan = (await (await planResponse).json()) as PlanDto;
    if (mode !== 'hash') {
      const selectedPlanResponse = page.waitForResponse(
        (response) =>
          response.request().method() === 'POST' &&
          new URL(response.url()).pathname === '/api/plans',
      );
      await duplicatesPage.chooseMode(label);
      plan = (await (await selectedPlanResponse).json()) as PlanDto;
    }
    await source.waitReady();
    await comparison.waitReady();

    const sourceFiles = manifest.data.files.filter(
      (file) =>
        file.indexable && file.volume === sourceFile.volume && parent(file.path) === sourceDir,
    );
    const expected = expectedMatches(manifest, mode, sourceFiles).filter(
      (file) => file.volume === comparisonFile.volume && parent(file.path) === comparisonDir,
    );
    const expectedSources = manifest
      .duplicateGroups(number)
      .filter(
        (candidate) =>
          candidate.source &&
          sourceFiles.some(
            (file) =>
              file.volume === candidate.source?.volume && file.path === candidate.source?.path,
          ) &&
          candidate.members.some(
            (member) =>
              member.volume === comparisonFile.volume &&
              parent(member.path) === comparisonDir &&
              ((member.match && member.modes.includes(number)) ||
                (mode === 'name_prefix' && member.match_plain_prefix === true)),
          ),
      )
      .map((candidate) => candidate.source?.path.split('/').at(-1) ?? '')
      .filter(Boolean)
      .sort();
    await expect.poll(() => matchedNames(source)).toEqual(expectedSources);
    await expect
      .poll(() => matchedNames(comparison))
      .toEqual(expected.map((file) => file.path.split('/').at(-1)).sort());
    await source.card(sourceFile.path.split('/').at(-1) ?? '').expectBorder('success');
    for (const file of expected)
      await comparison.card(file.path.split('/').at(-1) ?? '').expectBorder('success');
    await expect(duplicatesPage.planSummary()).toContainText(
      `${plan.stats.comparison_matched} comparison and ${plan.stats.source_matched} source files`,
    );
    await expect(
      page.getByRole('group', { name: 'Source pane' }).getByRole('status').first(),
    ).toContainText('in folder');
    await expect(
      page.getByRole('group', { name: 'Comparison pane' }).getByRole('status').first(),
    ).toContainText('in folder');

    if (mode === 'name_prefix') {
      await duplicatesPage.strictPrefix().setChecked(true);
      const strictExpected = expectedMatches(manifest, mode, [sourceFile], {
        strictPrefix: true,
      }).filter(
        (file) => file.volume === comparisonFile.volume && parent(file.path) === comparisonDir,
      );
      await expect
        .poll(() => matchedNames(comparison))
        .toEqual(strictExpected.map((file) => file.path.split('/').at(-1)).sort());
    }

    await page.reload();
    await expect(duplicatesPage.mode()).toHaveValue(mode);
    if (mode === 'name_prefix') await expect(duplicatesPage.strictPrefix()).toBeChecked();
    await expect(page).toHaveURL(
      new RegExp(`src=${encodeURIComponent(paneLocation(sourceFile.volume, sourceDir))}`),
    );
  });
}
