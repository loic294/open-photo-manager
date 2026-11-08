import { readFile, rm, symlink, writeFile as write } from 'node:fs/promises';
import { join } from 'node:path';
import { emptyFilter } from '../../src/api';
import { snapshotTree, writeFile } from '../../src/disk';
import { rawRequest } from '../../src/http';
import { expect, test } from '../../src/test';
import { indexedFile, movePlan } from '../../src/tool-oracles';

test('static traversal and missing assets are plain 404; app routes serve only the app @smoke', async ({
  sharedApp,
}) => {
  const host = new URL(sharedApp.url).host;
  for (const path of [
    '/..%2f..%2fetc/passwd',
    '/%00',
    '/assets/../../x',
    '/assets/missing.js',
    '/.env',
    '/%ZZ',
  ]) {
    const response = await rawRequest({
      url: sharedApp.url,
      method: 'GET',
      path,
      headers: { Host: host },
    });
    expect(response.status, path).toBe(404);
    expect(response.text).not.toContain('<script');
    expect(response.headers['content-type']).not.toContain('text/html');
  }
  for (const path of ['/move', '//']) {
    const response = await rawRequest({
      url: sharedApp.url,
      method: 'GET',
      path,
      headers: { Host: host },
    });
    expect(response.status, path).toBe(200);
    expect(response.headers['content-type']).toContain('text/html');
    expect(response.headers['cache-control']).toBe('no-cache');
    expect(response.text).toContain('<apm-app');
  }
});

test('unsafe API paths reject traversal, absolute and NUL without touching any volume', async ({
  app,
}) => {
  test.setTimeout(90_000);
  const volume = (await app.api.volumes()).find((volume) => volume.name === 'photos');
  if (!volume) throw new Error('Photos fixture volume missing');
  const before = await snapshotTree(app.volumesDir, { hash: true });
  const host = new URL(app.url).host;
  const request = async (path: string, body: object) => {
    const result = await rawRequest({
      url: app.url,
      path,
      method: 'POST',
      headers: {
        Host: host,
        'x-apm-request': '1',
        Origin: app.url,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    });
    expect(result.status, `${path}: ${JSON.stringify(body)}`).toBeGreaterThanOrEqual(400);
    expect(result.status).toBeLessThan(500);
    expect(result.json()).toMatchObject({
      error: { code: expect.any(String), message: expect.any(String) },
    });
  };
  for (const dir of ['..', 'a/../b', '/absolute', 'a\0b']) {
    const source = { scope: { volume_id: volume.id, dir, recursive: true }, filter: emptyFilter };
    for (const path of [
      '/api/browse/folders',
      '/api/browse/items',
      '/api/browse/ids',
      '/api/facets',
    ])
      await request(path, {
        volume_id: volume.id,
        dir,
        filter: emptyFilter,
        plan: null,
        sort: 'name',
        order: 'asc',
        offset: 0,
        limit: 10,
      });
    await request('/api/plans', {
      kind: 'move',
      replaces: null,
      params: {
        source,
        destination: { volume_id: volume.id, dir: 'system-security-destination' },
        move_type: 'flat',
        pattern: null,
        conflict_policy: null,
        match_mode: null,
        strict_prefix: false,
      },
    });
    await request('/api/format/preview', { pattern: '{filename}', source, limit: 1 });
    await request('/api/trash/restore', {
      target: { kind: 'batch', volume_id: volume.id, batch: dir },
      conflict_policy: null,
    });
  }
  for (const dir of ['a\\b', 'grid//', 'grid/./'])
    await request('/api/browse/folders', {
      volume_id: volume.id,
      dir,
      filter: emptyFilter,
      plan: null,
    });
  const normalized = await movePlan(
    app,
    { volume: 'photos', dir: 'grid//.' },
    { volume: 'archive', dir: 'system-normalized-destination' },
  );
  expect(normalized.stats.candidates).toBe(500);
  expect(await snapshotTree(app.volumesDir, { hash: true })).toEqual(before);
});

test('replacing an indexed photo with an outside symlink refuses every uncached media surface', async ({
  app,
  manifest,
}) => {
  test.setTimeout(90_000);
  const template = manifest.filesOf('exif.full_nikon')[0];
  await writeFile(
    app,
    'photos',
    'system-symlink/indexed.jpg',
    await readFile(app.volumePath(`${template.volume}/${template.path}`)),
  );
  await app.rescan();
  const photo = await indexedFile(app, 'photos', 'system-symlink/indexed.jpg');
  const secret = 'GENERATED-PRIVATE-E2E-OUTSIDE-BYTES-4242';
  const outside = join(app.directory, 'system-private-secret.txt');
  await write(outside, secret);
  await rm(app.volumePath('photos/system-symlink/indexed.jpg'));
  await symlink(outside, app.volumePath('photos/system-symlink/indexed.jpg'));
  for (const endpoint of ['content', 'preview', 'thumb?size=lg']) {
    const result = await rawRequest({
      url: app.url,
      method: 'GET',
      path: `/api/files/${photo.item.id}/${endpoint}`,
      headers: { Host: new URL(app.url).host },
    });
    expect(result.status, endpoint).toBeGreaterThanOrEqual(400);
    expect(result.status).toBeLessThan(500);
    expect(result.text).not.toContain(secret);
    expect(result.text).not.toContain(outside);
  }
  // lg was never cached; sm may legitimately hold a pre-swap snapshot, but must never expose outside bytes.
  const cached = await rawRequest({
    url: app.url,
    method: 'GET',
    path: `/api/files/${photo.item.id}/thumb?size=sm`,
    headers: { Host: new URL(app.url).host },
  });
  expect(cached.text).not.toContain(secret);
  await symlink(outside, app.volumePath('photos/system-symlink/new-link.jpg'));
  await app.rescan();
  const listing = await app.api.listing('photos', 'system-symlink');
  expect(listing.items).toEqual([]);
  const plan = await movePlan(
    app,
    { volume: 'photos', dir: 'system-symlink' },
    { volume: 'archive', dir: 'system-symlink-destination' },
  );
  expect(plan.stats.candidates ?? 0).toBe(0);
  expect(await readFile(outside, 'utf8')).toBe(secret);
});

test('media range, HEAD and ETags preserve original bytes and safe filename headers', async ({
  sharedApp,
  manifest,
}) => {
  test.setTimeout(60_000);
  const file = manifest.filesOf('exif.full_nikon')[0];
  const detail = await indexedFile(sharedApp, file.volume, file.path);
  const bytes = await readFile(sharedApp.volumePath(`${file.volume}/${file.path}`));
  const path = `/api/files/${detail.item.id}/content`;
  const headers = { Host: new URL(sharedApp.url).host };
  const full = await rawRequest({ url: sharedApp.url, method: 'GET', path, headers });
  expect(full.status).toBe(200);
  expect(full.bytes).toEqual(bytes);
  expect(full.headers['content-security-policy']).toBe('sandbox');
  expect(full.headers['accept-ranges']).toBe('bytes');
  for (const [range, start, end] of [
    ['bytes=2-17', 2, 17],
    ['bytes=18-', 18, bytes.length - 1],
    ['bytes=-9', bytes.length - 9, bytes.length - 1],
  ] as const) {
    const response = await rawRequest({
      url: sharedApp.url,
      method: 'GET',
      path,
      headers: { ...headers, Range: range },
    });
    expect(response.status, range).toBe(206);
    expect(response.headers['content-range']).toBe(`bytes ${start}-${end}/${bytes.length}`);
    expect(response.bytes).toEqual(bytes.subarray(start, end + 1));
  }
  const outside = await rawRequest({
    url: sharedApp.url,
    method: 'GET',
    path,
    headers: { ...headers, Range: `bytes=${bytes.length}-` },
  });
  expect(outside.status).toBe(416);
  expect(outside.headers['content-range']).toBe(`bytes */${bytes.length}`);
  const head = await rawRequest({ url: sharedApp.url, method: 'HEAD', path, headers });
  expect(head.status).toBe(200);
  expect(head.bytes).toHaveLength(0);
  expect(head.headers['content-length']).toBe(String(bytes.length));
  expect(typeof full.headers.etag).toBe('string');
  const unchanged = await rawRequest({
    url: sharedApp.url,
    method: 'GET',
    path,
    headers: { ...headers, 'If-None-Match': String(full.headers.etag) },
  });
  expect(unchanged.status).toBe(304);
  expect(unchanged.bytes).toHaveLength(0);
  for (const named of manifest.data.files.filter(
    (file) =>
      file.scenario.startsWith('names.') &&
      file.indexable &&
      !file.skipped &&
      file.kind !== 'sidecar',
  )) {
    const item = await indexedFile(sharedApp, named.volume, named.path);
    const response = await rawRequest({
      url: sharedApp.url,
      method: 'HEAD',
      path: `/api/files/${item.item.id}/content?download=1`,
      headers,
    });
    expect(response.status, named.path).toBe(200);
    expect(String(response.headers['content-disposition']), named.path).toMatch(
      /^attachment; filename="[^"\r\n]*"; filename\*=UTF-8''/,
    );
    for (const [key, value] of Object.entries(response.headers))
      expect(`${key}${value}`).not.toMatch(/[\r\n]/);
    expect(response.headers['content-security-policy']).toBe('sandbox');
  }
});

test('light and dark browser tours emit no CSP violations on every app page', async ({
  sharedApp,
  page,
  shell,
}) => {
  await page.goto(`${sharedApp.url}/browse`);
  await page.evaluate(() => {
    document.documentElement.dataset.cspViolations = '[]';
    document.addEventListener('securitypolicyviolation', (event) => {
      const messages = JSON.parse(document.documentElement.dataset.cspViolations ?? '[]');
      messages.push(`${event.violatedDirective}: ${event.blockedURI}`);
      document.documentElement.dataset.cspViolations = JSON.stringify(messages);
    });
  });
  for (const theme of ['light', 'dark'] as const) {
    await shell.theme(theme);
    for (const route of [
      'Browse',
      'Duplicate finder',
      'Asset move',
      'Folder formatting',
      'Empty folder cleanup',
      'Batch rename',
      'Trash',
      'Settings',
    ])
      await shell.navigate(route);
  }
  expect(await page.locator('html').getAttribute('data-csp-violations')).toBe('[]');
});
