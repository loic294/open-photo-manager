import { readFile } from 'node:fs/promises';
import type { FormatTokensResponse } from '../../../web/src/api/generated/FormatTokensResponse';
import { writeFile } from '../../src/disk';
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
  const smoke = scenario === 'exif.full_nikon' ? '@smoke ' : '';
  test(`${smoke}format pattern uses the expected date for ${scenario}`, async ({
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
    await formatPage.edit();

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

test('format pattern keeps the embedded date when sidecar metadata overrides camera and rating', async ({
  app,
  page,
  manifest,
  formatPage,
}) => {
  const file = manifest.file('photos', 'sidecars/IMG_0010.JPG');
  const sidecar = manifest.file('photos', 'sidecars/IMG_0010.JPG.xmp');
  test.skip(
    file.skipped || sidecar.skipped,
    file.skip_reason ?? sidecar.skip_reason ?? 'fixture skipped',
  );

  const folder = 'e2e-sidecar-override';
  const ownerName = 'IMG_0010.JPG';
  await writeFile(
    app,
    'photos',
    `${folder}/${ownerName}`,
    await readFile(app.volumePath(`${file.volume}/${file.path}`)),
  );
  await writeFile(
    app,
    'photos',
    `${folder}/${ownerName}.xmp`,
    await readFile(app.volumePath(`${sidecar.volume}/${sidecar.path}`)),
  );
  const volume = (await app.api.volumes()).find((entry) => entry.name === 'photos');
  if (!volume) throw new Error('Photos volume is missing');
  await app.rescan([{ volume_id: volume.id, dir: folder, recursive: false }]);

  const listing = await app.api.listing('photos', folder);
  const indexedOwner = listing.items.find((item) => item.name === ownerName);
  expect(indexedOwner).toMatchObject({
    capture_src: 'exif',
    capture_ts: Date.parse('2022-07-07T07:07:07Z') / 1000,
    camera: 'Sony ILCE-7M4',
    rating: 5,
  });

  const pattern = '{YYYY}/{mm}/{filename}';
  const url = new URL('/format', app.url);
  url.searchParams.set('src', `photos/${folder}`);
  const previewResponse = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  const planResponse = page.waitForResponse(isResponse(/^\/api\/plans$/));
  await page.goto(url.href);
  await previewResponse;
  await planResponse;
  await formatPage.edit();
  const expected = '2022/07/IMG_0010.JPG';
  const updatedPreview = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  await formatPage.pattern().fill(pattern);
  await updatedPreview;
  await formatPage.livePreview().locator('summary').click();
  await expect(formatPage.livePreview()).toContainText(expected);
  await expect(formatPage.previewRows()).toContainText('exif');
});

test('format pattern renders month names, time and the original uppercase extension', async ({
  app,
  page,
  formatPage,
}) => {
  const pattern = '{MMM}/{MMMM}/{HH}-{min}-{sec}/{folder}/{name}.{ext}';
  const planResponse = page.waitForResponse(isResponse(/^\/api\/plans$/));
  const previewResponse = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  const url = new URL('/format', app.url);
  url.searchParams.set('src', 'photos/sidecars');
  await page.goto(url.href);
  await planResponse;
  await previewResponse;

  await formatPage.edit();
  const updatedPreview = page.waitForResponse(isResponse(/^\/api\/format\/preview$/));
  await formatPage.pattern().fill(pattern);
  await updatedPreview;
  const expected = 'May/May/09-30-00/sidecars/IMG_0001.CR3';
  await formatPage.livePreview().locator('summary').click();
  await expect(formatPage.livePreview()).toContainText(expected);
});
