import type { FormatTokensResponse } from '../../../web/src/api/generated/FormatTokensResponse';
import { expect, test } from '../../src/test';
import { expectedDestination } from '../../src/tool-oracles';

const dateFixtures = [
  { scenario: 'exif.full_nikon', date: '2021/03' },
  { scenario: 'sidecar.exact', date: '2022/05' },
  { scenario: 'exif.no_date', date: '2019/07' },
  { scenario: 'exif.dec31_offset', date: '2022/12' },
] as const;

function isResponse(path: RegExp) {
  return (response: import('@playwright/test').Response) =>
    response.request().method() === 'POST' && path.test(new URL(response.url()).pathname);
}

for (const { scenario, date } of dateFixtures) {
  test(`@smoke format pattern uses the expected date for ${scenario}`, async ({
    app,
    page,
    manifest,
    formatPage,
  }) => {
    const file = manifest.data.files.find(
      (candidate) => candidate.scenario === scenario && candidate.kind !== 'sidecar',
    );
    test.skip(!file || file.skipped, manifest.scenario(scenario).skip_reason ?? 'fixture skipped');
    if (!file) return;
    const pattern = '{YYYY}/{mm}/{filename}';
    const dir = file.path.includes('/') ? file.path.slice(0, file.path.lastIndexOf('/')) : '';
    const url = new URL('/format', app.url);
    url.searchParams.set('src', `${file.volume}${dir ? `/${dir}` : ''}`);
    const initialPreview = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
    const initialPlan = page.waitForResponse(isResponse(/^\/api\/plans$/));
    await page.goto(url.href);
    await initialPreview;
    await initialPlan;

    const previewResponse = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
    await formatPage.pattern().fill(pattern);
    expect((await previewResponse).ok()).toBe(true);
    await formatPage.livePreview().locator('summary').click();
    const expected = expectedDestination(file, pattern);
    expect(expected.startsWith(`${date}/`)).toBe(true);
    await expect(formatPage.livePreview()).toContainText(expected);

    const tokens = await app.api.request<FormatTokensResponse>('GET', '/api/format/tokens');
    const offered = new Set(tokens.tokens.map((token) => token.name));
    expect([...offered]).toEqual(
      expect.arrayContaining(['YYYY', 'mm', 'filename', 'camera', 'folder']),
    );
    await formatPage.variables().click();
    await expect(page.getByRole('button', { name: '{YYYY}', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '{filename}', exact: true })).toBeVisible();
  });
}
