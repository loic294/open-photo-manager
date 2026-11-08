import assert from 'node:assert/strict';
import { test } from 'node:test';
import { call, connect, searchAll, startMock, typedBody } from './helpers.ts';

test('the state shows the whole model, with paging and a path filter', async (t) => {
  const mock = await startMock(t);
  mock.control.generate({ library: 'Photos', prefix: '/mnt/photo/2024/IMG_', count: 30 });
  mock.control.generate({
    library: 'Videos',
    prefix: '/mnt/video/clip_',
    count: 5,
    extension: 'mp4',
  });

  const all = await call(mock, 'GET', '/__mock/state');
  assert.equal(all.status, 200);
  assert.deepEqual(Object.keys(all.body).sort(), [
    'albums',
    'assets',
    'description',
    'failures',
    'gates',
    'held',
    'jobs',
    'keys',
    'libraries',
    'mounts',
    'serverVersion',
    'settings',
    'tags',
    'users',
    'violations',
  ]);
  assert.equal(all.body.assets.total, 35);
  assert.equal(all.body.assets.limit, 100);
  assert.equal(all.body.assets.items.length, 35);
  assert.equal(all.body.serverVersion, 'v3.4.0');
  assert.deepEqual(all.body.violations, []);

  const page = await call(mock, 'GET', '/__mock/state?limit=10&offset=25');
  assert.equal(page.body.assets.total, 35);
  assert.equal(page.body.assets.items.length, 10);
  assert.equal(page.body.assets.offset, 25);

  const videos = await call(mock, 'GET', '/__mock/state?path=/mnt/video/&limit=0');
  assert.equal(videos.body.assets.total, 5);
  assert.deepEqual(videos.body.assets.items, []);

  for (const query of ['limit=-1', 'limit=abc', 'limit=5001', 'offset=1.5']) {
    const bad = await call(mock, 'GET', `/__mock/state?${query}`);
    assert.equal(bad.status, 400, query);
  }
  assert.deepEqual(mock.state.snapshot({ path: '/mnt/video/', limit: 1 }).assets, {
    total: 5,
    offset: 0,
    limit: 1,
    items: [mock.state.assetByPath('/mnt/video/clip_000000.mp4')],
  });
});

test('the request log is read from a cursor, and never shows the control calls', async (t) => {
  const mock = await startMock(t);
  await call(mock, 'GET', '/api/server/ping');
  await call(mock, 'GET', '/api/nothing-here');
  await call(mock, 'POST', '/api/auth/login', {
    body: { email: 'admin@example.test', password: 'admin-password' },
  });

  const first = await call(mock, 'GET', '/__mock/log');
  assert.equal(first.status, 200);
  assert.equal(first.body.entries.length, 3);
  assert.equal(first.body.next, 3);
  const [ping, missing, login] = first.body.entries;
  assert.equal(ping.method, 'GET');
  assert.equal(ping.path, '/api/server/ping');
  assert.equal(ping.status, 200);
  assert.equal(ping.auth, 'none');
  assert.equal(missing.status, 404);
  assert.equal(missing.unexpected, true);
  assert.equal(login.status, 201);
  assert.deepEqual(login.bodySummary, { email: 'admin@example.test', password: '[redacted]' });
  assert.ok(!JSON.stringify(first.body).includes('admin-password'));

  await call(mock, 'GET', '/api/server/ping');
  const later = await call(mock, 'GET', `/__mock/log?since=${first.body.next}`);
  assert.equal(later.body.entries.length, 1);
  assert.equal(later.body.entries[0].index, 3);
  assert.equal(later.body.next, 4);
  assert.deepEqual(
    mock.state.requests(3).entries.map((entry) => entry.index),
    [3],
  );
  assert.equal((await call(mock, 'GET', '/__mock/log?since=x')).status, 400);
});

test('the log shows who called, and the query string', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock, { name: 'my-key' });
  await call(mock, 'GET', '/api/albums?assetId=00000000-0000-4000-8000-000000000001', { key });
  await call(mock, 'GET', '/api/users/me', { key: 'wrong' });
  const entries = mock.state.requests().entries;
  const albums = entries.find((entry) => entry.path === '/api/albums');
  assert.equal(albums?.auth, 'apiKey');
  assert.equal(albums?.keyName, 'my-key');
  assert.equal(albums?.query, 'assetId=00000000-0000-4000-8000-000000000001');
  const wrong = entries.find((entry) => entry.path === '/api/users/me');
  assert.equal(wrong?.auth, 'invalid');
  assert.equal(wrong?.status, 401);
  const creation = entries.find((entry) => entry.path === '/api/api-keys');
  assert.equal(creation?.auth, 'session');
});

test('a reset brings the scenario back: same ids, no sessions, keys, rules, assets or log', async (t) => {
  const mock = await startMock(t);
  const before = {
    users: mock.state.users(),
    libraries: mock.state.libraries().map((library) => library.id),
    albums: mock.state.albums().map((album) => album.id),
    tags: mock.state.tags().map((tag) => tag.id),
  };
  const { key, token } = await connect(mock);
  mock.control.generate({ library: 'Photos', prefix: '/mnt/photo/g/IMG_', count: 10 });
  mock.control.fail({ match: '/api/albums*', status: 500 });
  mock.control.latency(10);
  const [photos] = mock.state.libraries();
  await call(mock, 'PUT', `/api/libraries/${photos?.id}`, { key, body: { name: 'Renamed' } });
  const lastBefore = mock.state.requests().next;
  assert.ok(lastBefore > 0);

  const reset = await call(mock, 'POST', '/__mock/reset');
  assert.deepEqual(reset.body, { reset: true });

  assert.equal(mock.state.assets().length, 0);
  assert.deepEqual(mock.state.keys(), []);
  assert.deepEqual(mock.control.failures(), []);
  assert.deepEqual(mock.state.requests().entries, []);
  assert.deepEqual(mock.state.snapshot().settings.latencyMs, 0);
  assert.deepEqual(mock.state.users(), before.users);
  assert.deepEqual(
    mock.state.libraries().map((library) => library.id),
    before.libraries,
  );
  assert.deepEqual(
    mock.state.albums().map((album) => album.id),
    before.albums,
  );
  assert.deepEqual(
    mock.state.tags().map((tag) => tag.id),
    before.tags,
  );
  assert.equal(mock.state.libraries()[0]?.name, 'Photos', 'the library is back to its scenario');

  assert.equal((await call(mock, 'GET', '/api/users/me', { key })).status, 401);
  assert.equal((await call(mock, 'GET', '/api/users/me', { token })).status, 401);
  const next = mock.state.requests();
  assert.ok(
    next.entries[0] !== undefined && next.entries[0].index >= lastBefore,
    'the log keeps counting',
  );

  const again = await connect(mock);
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: again.key })).status, 200);
});

test('generate adds numbered assets with the capture times asked for', async (t) => {
  const mock = await startMock(t);
  const made = mock.control.generate({
    libraryId: 'Photos',
    prefix: '/mnt/photo/burst/IMG_',
    count: 6,
    startAt: '2024-05-01T10:00:00.000Z',
    stepMs: 60_000,
    ties: 3,
  });
  assert.deepEqual(made, {
    created: 6,
    firstPath: '/mnt/photo/burst/IMG_000000.jpg',
    lastPath: '/mnt/photo/burst/IMG_000005.jpg',
  });
  const times = [0, 1, 2, 3, 4, 5].map(
    (n) => mock.state.assetByPath(`/mnt/photo/burst/IMG_00000${n}.jpg`)?.fileCreatedAt,
  );
  assert.deepEqual(times, [
    '2024-05-01T10:00:00.000Z',
    '2024-05-01T10:00:00.000Z',
    '2024-05-01T10:00:00.000Z',
    '2024-05-01T10:01:00.000Z',
    '2024-05-01T10:01:00.000Z',
    '2024-05-01T10:01:00.000Z',
  ]);
  assert.throws(
    () => mock.control.generate({ library: 'Photos', prefix: '/mnt/photo/burst/IMG_', count: 2 }),
    /an asset already exists at \/mnt\/photo\/burst\/IMG_000000\.jpg/,
  );
  assert.equal(mock.state.assets().length, 6, 'a refused call adds nothing');

  const over = await call(mock, 'POST', '/__mock/generate', {
    body: { library: 'Videos', prefix: '/mnt/video/v_', count: 3, extension: 'mp4' },
  });
  assert.equal(over.status, 200);
  assert.equal(over.body.created, 3);
  assert.equal(mock.state.assetByPath('/mnt/video/v_000002.mp4')?.type, 'VIDEO');

  for (const body of [
    {},
    { library: 'Photos', prefix: '/mnt/photo/x_' },
    { library: 'Photos', prefix: 'relative', count: 1 },
    { library: 'Photos', prefix: '/mnt/photo/x_', count: 0 },
    { library: 'Photos', prefix: '/mnt/photo/x_', count: 1, extension: 'j.pg' },
    { library: 'Nope', prefix: '/mnt/photo/x_', count: 1 },
    { library: 'Photos', prefix: '/mnt/photo/x_', count: 1, colour: 'red' },
  ]) {
    const bad = await call(mock, 'POST', '/__mock/generate', { body });
    assert.equal(bad.status, 400, JSON.stringify(body));
  }
});

test('two hundred thousand assets are generated in seconds and searched by cursor', async (t) => {
  const mock = await startMock(t);
  const started = Date.now();
  const made = mock.control.generate({
    library: 'Photos',
    prefix: '/mnt/photo/big/IMG_',
    count: 200_000,
    ties: 7,
  });
  assert.equal(made.created, 200_000);
  assert.ok(Date.now() - started < 15_000, `took ${Date.now() - started} ms`);
  assert.equal(mock.state.snapshot({ limit: 0 }).assets.total, 200_000);

  const { key } = await connect(mock);
  const first = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(null, { size: 1000 }),
  });
  assert.equal(first.status, 200);
  assert.equal(first.body.assets.items.length, 1000);
  assert.equal(typeof first.body.assets.nextCursor, 'string');

  const pages = await searchAll(
    mock,
    key,
    (cursor) =>
      typedBody(cursor, { size: 1000 }, { libraryId: { eq: mock.state.libraries()[0]?.id } }),
    (assets) => assets.nextCursor,
  );
  assert.equal(pages.items.length, 200_000);
  assert.equal(pages.pages, 200);
  assert.equal(new Set(pages.items.map((item: { id: string }) => item.id)).size, 200_000);
});

test('assets are added and changed over HTTP, matched by path', async (t) => {
  const mock = await startMock(t);
  const added = await call(mock, 'POST', '/__mock/assets', {
    body: {
      assets: [
        { path: '/mnt/photo/a.jpg', isFavorite: true },
        { path: '/mnt/video/b.mp4', description: 'clip' },
      ],
    },
  });
  assert.equal(added.status, 200);
  assert.equal(added.body.created, 2);
  assert.equal(added.body.updated, 0);
  assert.deepEqual(
    added.body.items.map((item: { path: string; created: boolean }) => [item.path, item.created]),
    [
      ['/mnt/photo/a.jpg', true],
      ['/mnt/video/b.mp4', true],
    ],
  );
  const [photos, videos] = mock.state.libraries();
  assert.equal(mock.state.assetByPath('/mnt/photo/a.jpg')?.libraryId, photos?.id);
  assert.equal(mock.state.assetByPath('/mnt/video/b.mp4')?.libraryId, videos?.id);

  const changed = await call(mock, 'POST', '/__mock/assets', {
    body: { assets: [{ path: '/mnt/photo/a.jpg', isFavorite: false, rating: 3 }] },
  });
  assert.equal(changed.body.created, 0);
  assert.equal(changed.body.updated, 1);
  assert.equal(changed.body.items[0].id, added.body.items[0].id, 'same asset, same id');
  assert.equal(mock.state.assetByPath('/mnt/photo/a.jpg')?.isFavorite, false);
  assert.equal(mock.state.assetByPath('/mnt/photo/a.jpg')?.rating, 3);
  assert.equal(mock.state.assets().length, 2);

  const trashed = mock.control.addAssets([{ path: '/mnt/photo/a.jpg', trashed: true }]);
  assert.equal(trashed.updated, 1);
  assert.notEqual(mock.state.assetByPath('/mnt/photo/a.jpg')?.deletedAt, null);

  for (const body of [
    {},
    { assets: 'a' },
    { assets: [{}] },
    { assets: [{ path: 'relative.jpg' }] },
    { assets: [{ path: '/elsewhere/a.jpg' }] },
    { assets: [{ path: '/mnt/photo/c.jpg', rating: 9 }] },
    { assets: [{ path: '/mnt/photo/c.jpg', colour: 'red' }] },
  ]) {
    const bad = await call(mock, 'POST', '/__mock/assets', { body });
    assert.equal(bad.status, 400, JSON.stringify(body));
  }
  assert.equal(mock.state.assetByPath('/mnt/photo/c.jpg'), undefined);
});

test('keys are revoked by name or all together, over HTTP too', async (t) => {
  const mock = await startMock(t);
  const one = await connect(mock, { name: 'one' });
  const two = await connect(mock, { name: 'two' });
  assert.deepEqual(
    (await call(mock, 'POST', '/__mock/revoke-key', { body: { name: 'one' } })).body,
    {
      revoked: 1,
    },
  );
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: one.key })).status, 401);
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: two.key })).status, 200);
  assert.equal(mock.control.revokeKey('missing'), 0);
  assert.deepEqual((await call(mock, 'POST', '/__mock/revoke-key')).body, { revoked: 1 });
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: two.key })).status, 401);
  assert.deepEqual(mock.state.keys(), []);
});

test('the control endpoints answer 404 to what they do not know', async (t) => {
  const mock = await startMock(t);
  const unknown = await call(mock, 'GET', '/__mock/nothing');
  assert.equal(unknown.status, 404);
  assert.equal(unknown.body.message, 'Cannot GET /__mock/nothing');
  assert.equal((await call(mock, 'DELETE', '/__mock/state')).status, 404);
  assert.equal(
    (await call(mock, 'POST', '/__mock/scan', { body: { libraryId: 'Nope' } })).status,
    400,
  );
  assert.equal((await call(mock, 'POST', '/__mock/scan', { raw: '{ broken' })).status, 400);
  assert.equal((await call(mock, 'GET', '/other')).status, 404);
  assert.equal(
    mock.state.requests().entries.find((entry) => entry.path === '/other')?.unexpected,
    true,
  );
});

test('the log is bounded, and the control API works on a mock of 200 requests', async (t) => {
  const mock = await startMock(t);
  for (let i = 0; i < 200; i += 1) {
    await call(mock, 'GET', '/api/server/ping');
  }
  const { entries, next } = mock.state.requests();
  assert.equal(entries.length, 200);
  assert.equal(next, 200);
  assert.deepEqual(
    mock.state.requests(190).entries.map((entry) => entry.index),
    [190, 191, 192, 193, 194, 195, 196, 197, 198, 199],
  );
});
