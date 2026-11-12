import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { basename, dirname, join } from 'node:path';
import type { FileDetail } from '../../../web/src/api/generated/FileDetail';
import { emptyFilter } from '../../src/api';
import { expect, test } from '../../src/test';

test('the complete manifest agrees with index metadata, hashes and sidecar links', async ({
  sharedApp,
  manifest,
}) => {
  test.setTimeout(120_000);
  const volumes = await sharedApp.api.volumes();
  const rows = sharedApp.sql(
    'SELECT id, volume_id, dir_path, name, capture_offset_min, meta_state FROM files',
  );
  const failures: string[] = [];
  for (const [key, scenario] of Object.entries(manifest.data.scenarios)) {
    if (scenario.skipped)
      test.info().annotations.push({
        type: 'fixture skipped',
        description: `${key}: ${scenario.skip_reason}`,
      });
  }
  for (const file of manifest.data.files.filter((file) => !file.skipped)) {
    const volume = volumes.find((volume) => volume.name === file.volume);
    expect(volume, file.volume).toBeDefined();
    const dir = dirname(file.path) === '.' ? '' : dirname(file.path);
    const row = rows.find(
      (row) =>
        row.volume_id === volume?.id && row.dir_path === dir && row.name === basename(file.path),
    );
    if (!file.indexable) {
      if (row)
        failures.push(`${file.scenario} ${file.volume}/${file.path}: excluded file was indexed`);
      continue;
    }
    if (!row) {
      failures.push(`${file.scenario} ${file.volume}/${file.path}: missing index row`);
      continue;
    }
    const detail = await sharedApp.api.request<FileDetail>('GET', `/api/files/${row.id}`);
    const differences: string[] = [];
    const check = (field: string, actual: unknown, expected: unknown) => {
      if (JSON.stringify(actual) !== JSON.stringify(expected))
        differences.push(`${field}: ${JSON.stringify(actual)} != ${JSON.stringify(expected)}`);
    };
    check(
      'location',
      [detail.item.volume_id, detail.item.dir, detail.item.name],
      [volume?.id, dir, basename(file.path)],
    );
    check('size', detail.item.size, file.size);
    check('mtime', detail.item.mtime, file.mtime);
    if (file.unreadable !== 'file' || process.getuid?.() === 0) {
      const hash = createHash('sha1');
      for await (const chunk of createReadStream(
        join(sharedApp.volumesDir, file.volume, file.path),
      ))
        hash.update(chunk);
      // Sidecars are indexed but excluded from the hashing pipeline.
      if (file.kind !== 'sidecar')
        check('SHA-1 (computed from disk)', detail.hash, hash.digest('hex'));
    } else check('unreadable hash', detail.hash, null);
    if (['image', 'raw', 'video'].includes(file.kind)) {
      const extractionFails =
        file.scenario === 'exif.zero_byte' ||
        (file.unreadable === 'file' && process.getuid?.() !== 0);
      // indexer/extract.rs module contract: failures retain null capture columns; empty success uses file time.
      check('metadata state', row.meta_state, extractionFails ? 2 : 1);
      check(
        'capture source',
        detail.item.capture_src,
        extractionFails ? null : file.capture_src_expected,
      );
      if (extractionFails) {
        check('failed capture time', detail.item.capture_ts, null);
        check('failed capture offset', row.capture_offset_min, null);
      } else if (file.capture_ts_expected !== null)
        check(
          'capture time',
          detail.item.capture_ts,
          Date.parse(`${file.capture_ts_expected}Z`) / 1000,
        );
      else expect(detail.item.capture_ts, `${file.path} filesystem fallback`).not.toBeNull();
      if (!extractionFails) {
        const utcFallback =
          file.capture_src_expected === 'fs' ||
          file.scenario === 'media.video_mp4' ||
          file.scenario === 'media.video_nometa';
        check(
          'capture offset',
          row.capture_offset_min,
          utcFallback && file.capture_offset_min_expected === null
            ? 0
            : file.capture_offset_min_expected,
        );
      }
      check('camera', detail.item.camera, file.camera_expected);
      check('rating', detail.item.rating, file.rating_expected);
      check(
        'flag',
        detail.item.flag,
        file.flag_expected === 'selected' ? 1 : file.flag_expected === 'rejected' ? -1 : 0,
      );
      check('tags', [...detail.tags].sort(), [...(file.tags_expected ?? [])].sort());
    }
    check(
      'sidecars',
      detail.sidecars.map((sidecar) => `${sidecar.name}:${sidecar.kind}`).sort(),
      (file.sidecars ?? []).map((sidecar) => `${basename(sidecar.path)}:${sidecar.link}`).sort(),
    );
    if (differences.length)
      failures.push(`${file.scenario} ${file.volume}/${file.path}: ${differences.join('; ')}`);
  }
  expect(failures, 'Each file lists all differing fields').toEqual([]);
  expect(rows.length).toBe(
    manifest.data.files.filter((file) => file.indexable && !file.skipped).length,
  );
});

test('UI facet values and counts match the manifest, including XMP-only filters', async ({
  sharedApp,
  manifest,
  page,
  filterBar,
  source,
  comparison,
  bottomBar,
}) => {
  const volume = (await sharedApp.api.volumes()).find((volume) => volume.name === 'photos');
  expect(volume).toBeDefined();
  await page.goto(`${sharedApp.url}/browse?src=photos&cmp=photos`);
  await filterBar.waitForCountedFacets();
  const files = manifest.data.files.filter(
    (file) =>
      file.volume === 'photos' && file.indexable && !file.skipped && file.kind !== 'sidecar',
  );
  const counts = (pick: (file: (typeof files)[number]) => string[]) => {
    const map = new Map<string, number>();
    for (const file of files)
      for (const value of pick(file)) map.set(value, (map.get(value) ?? 0) + 1);
    return [...map]
      .map(([value, count]) => ({ value, count }))
      .sort((a, b) => a.value.localeCompare(b.value));
  };
  const facets = await sharedApp.api.facets({
    volume_id: volume?.id ?? null,
    dir: '',
    filter: emptyFilter,
    plan: null,
  });
  for (const [label, actual, expected] of [
    [
      'Cameras',
      facets.cameras,
      counts((file) => (file.camera_expected ? [file.camera_expected] : [])),
    ],
    ['Tags', facets.tags, counts((file) => file.tags_expected ?? [])],
  ] as const) {
    expect([...actual].sort((a, b) => a.value.localeCompare(b.value))).toEqual(expected);
    await filterBar.element.locator(`opm-multi-select[label="${label}"] summary`).click();
    for (const entry of expected) {
      await expect(
        filterBar.facets(label).and(
          page.getByRole('checkbox', {
            name: `${entry.value} (${entry.count})`,
            exact: true,
          }),
        ),
      ).toHaveCount(1);
    }
    await page.keyboard.press('Escape');
  }
  expect([...facets.ratings].sort((a, b) => a.value.localeCompare(b.value))).toEqual(
    counts((file) => [String(file.rating_expected ?? 0)]),
  );
  for (const entry of facets.ratings)
    await expect(
      filterBar.element.getByRole('button', {
        name: `${entry.value === '0' ? 'Unrated' : `${entry.value} ${entry.value === '1' ? 'star' : 'stars'}`} (${entry.count})`,
        exact: true,
      }),
    ).toBeVisible();
  const xmp = manifest.filesOf('sidecar.exact').find((file) => file.kind === 'raw');
  expect(xmp).toBeDefined();
  await source.openFolder('sidecars');
  await comparison.openFolder('sidecars');
  await filterBar.rating(4);
  await filterBar.flag('Selected');
  await filterBar.tag('Rome');
  await expect(source.card(basename(xmp?.path ?? '')).element).toBeVisible();
  await expect.poll(() => source.names()).toEqual([basename(xmp?.path ?? '')]);
  await expect(bottomBar.side('source').getByText('1 in folder', { exact: true })).toBeVisible();
});

for (const key of [
  'exif.full_nikon',
  'sidecar.exact',
  'media.png',
  'names.emoji',
  'junk.real_neighbour',
]) {
  test(`card name and date: ${key}`, async ({ sharedApp, manifest, source }) => {
    const scenario = manifest.scenario(key);
    test.skip(scenario.skipped, scenario.skip_reason ?? 'Fixture generation skipped this scenario');
    const file = manifest.filesOf(key).find((file) => file.indexable && file.kind !== 'sidecar');
    expect(file).toBeDefined();
    if (!file) throw new Error(`No media for ${key}`);
    await source.goto(sharedApp.url, file);
    await expect(source.card(basename(file.path)).element).toBeVisible();
    await expect(source.card(basename(file.path)).date()).not.toBeEmpty();
    if (file.capture_ts_expected) {
      const expected = new Intl.DateTimeFormat('en-US', {
        dateStyle: 'medium',
        timeZone: 'UTC',
      }).format(new Date(`${file.capture_ts_expected}Z`));
      await expect(source.card(basename(file.path)).date()).toContainText(expected);
    }
  });
}
