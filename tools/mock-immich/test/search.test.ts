import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { scenarioFiles } from '../index.ts';
import { call, connect, flatBody, searchAll, startMock, typedBody, USER } from './helpers.ts';

const PHOTOS = '/mnt/photo/gen/IMG_';

/** Many assets that share capture times, so only the tie-break keeps the paging exact. */
async function seeded(t: Parameters<typeof startMock>[0], count: number, options = {}) {
  const mock = await startMock(t, options);
  mock.control.generate({ library: 'Photos', prefix: PHOTOS, count, ties: 3, stepMs: 1000 });
  const { key } = await connect(mock);
  return { mock, key };
}

const cursorOf = (answer: { nextCursor: string | null }) => answer.nextCursor;
const pageOf = (answer: { nextPage: string | null }) => answer.nextPage;

test('the typed search returns every one of 2,500 assets exactly once, oldest first', async (t) => {
  const { mock, key } = await seeded(t, 2500);
  const { items, pages } = await searchAll(mock, key, (token) => typedBody(token), cursorOf);
  assert.equal(pages, 10);
  assert.equal(items.length, 2500);
  assert.equal(new Set(items.map((item) => item.id)).size, 2500);
  const expected = Array.from(
    { length: 2500 },
    (_, index) => `${PHOTOS}${String(index).padStart(6, '0')}.jpg`,
  );
  assert.deepEqual(
    items.map((item) => item.originalPath),
    expected,
  );
});

test('the typed search can run newest first and with the largest page', async (t) => {
  const { mock, key } = await seeded(t, 2500);
  const { items, pages } = await searchAll(
    mock,
    key,
    (token) =>
      typedBody(token, { size: 1000, orderBy: { field: 'fileCreatedAt', direction: 'desc' } }),
    cursorOf,
  );
  assert.equal(pages, 3);
  assert.equal(items.length, 2500);
  assert.equal(new Set(items.map((item) => item.id)).size, 2500);
  const times = items.map((item) => Date.parse(item.fileCreatedAt));
  for (let i = 1; i < times.length; i += 1) {
    assert.ok((times[i] ?? 0) <= (times[i - 1] ?? 0), `item ${i} is out of order`);
  }
  assert.equal(items[0].originalPath, `${PHOTOS}002499.jpg`);
  assert.equal(items.at(-1).originalPath, `${PHOTOS}000000.jpg`);
});

test('the flat search pages by number and the last page has no next page', async (t) => {
  const { mock, key } = await seeded(t, 2500, { serverVersion: '2.5.1' });
  const { items, pages } = await searchAll(
    mock,
    key,
    (token) => flatBody(token === null ? null : Number(token)),
    pageOf,
  );
  assert.equal(pages, 10);
  assert.equal(items.length, 2500);
  assert.equal(new Set(items.map((item) => item.id)).size, 2500);
  assert.equal(items[0].originalPath, `${PHOTOS}000000.jpg`);

  const last = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: flatBody(10),
  });
  assert.equal(last.body.assets.nextPage, null);
  assert.equal(last.body.assets.nextCursor, null);
  const beyond = await call(mock, 'POST', '/api/search/metadata', { key, body: flatBody(11) });
  assert.deepEqual(beyond.body.assets.items, []);
});

test('the answer has the shape the client parses', async (t) => {
  const { mock, key } = await seeded(t, 3);
  const reply = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(null, { size: 2 }),
  });
  assert.equal(reply.status, 200);
  const { assets, albums } = reply.body;
  assert.deepEqual(albums, { total: 0, count: 0, items: [], facets: [] });
  assert.equal(assets.total, 2);
  assert.equal(assets.count, 2);
  assert.deepEqual(assets.facets, []);
  assert.equal(typeof assets.nextCursor, 'string');
  assert.equal(assets.nextPage, null);
  const [first] = assets.items;
  assert.equal(first.originalPath, `${PHOTOS}000000.jpg`);
  assert.equal(first.originalFileName, 'IMG_000000.jpg');
  assert.equal(first.libraryId, mock.state.libraries()[0]?.id);
  assert.equal(first.isOffline, false);
  assert.equal(first.isTrashed, false);
  assert.equal(first.type, 'IMAGE');
  assert.equal(first.visibility, 'timeline');
  assert.equal(first.exifInfo, undefined);
  assert.equal(first.deletedAt, undefined);
  assert.match(first.updatedAt, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/);
  assert.match(first.fileCreatedAt, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/);

  const withExif = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(null, { withExif: true }),
  });
  assert.equal(withExif.body.assets.items[0].exifInfo.rating, null);
  assert.equal(withExif.body.assets.nextCursor, null);
});

test('the typed search leaves the trash in unless it is asked not to', async (t) => {
  const mock = await startMock(t);
  mock.control.addAssets([
    { path: '/mnt/photo/online.jpg', fileCreatedAt: '2024-01-01T00:00:00.000Z' },
    { path: '/mnt/photo/trashed.jpg', trashed: true, fileCreatedAt: '2024-01-02T00:00:00.000Z' },
    { path: '/mnt/photo/offline.jpg', isOffline: true, fileCreatedAt: '2024-01-03T00:00:00.000Z' },
  ]);
  const { key } = await connect(mock);
  const names = async (body: unknown) => {
    const reply = await call(mock, 'POST', '/api/search/metadata', { key, body });
    assert.equal(reply.status, 200, JSON.stringify(reply.body));
    return reply.body.assets.items.map(
      (item: { originalFileName: string }) => item.originalFileName,
    );
  };

  assert.deepEqual(await names(typedBody(null)), ['online.jpg']);
  assert.deepEqual(
    await names({ orderBy: { field: 'fileCreatedAt', direction: 'asc' }, filter: {} }),
    ['online.jpg', 'trashed.jpg', 'offline.jpg'],
  );
  assert.deepEqual(
    await names({
      orderBy: { field: 'fileCreatedAt', direction: 'asc' },
      filter: { isOffline: { eq: true } },
    }),
    ['offline.jpg'],
  );
  assert.deepEqual(
    await names(typedBody(null, {}, { isOffline: { eq: false }, trashedAt: { ne: null } })),
    ['trashed.jpg'],
  );
  const trashed = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: { orderBy: { field: 'fileCreatedAt', direction: 'asc' }, filter: {} },
  });
  const [, flagged, lost] = trashed.body.assets.items;
  assert.equal(flagged.isTrashed, true);
  assert.equal(flagged.isOffline, false);
  assert.equal(lost.isTrashed, true);
  assert.equal(lost.isOffline, true);
});

test('the flat search hides the trash unless withDeleted or isOffline asks for it', async (t) => {
  const mock = await startMock(t, { serverVersion: '2.5.1' });
  mock.control.addAssets([
    { path: '/mnt/photo/online.jpg', fileCreatedAt: '2024-01-01T00:00:00.000Z' },
    { path: '/mnt/photo/trashed.jpg', trashed: true, fileCreatedAt: '2024-01-02T00:00:00.000Z' },
    { path: '/mnt/photo/offline.jpg', isOffline: true, fileCreatedAt: '2024-01-03T00:00:00.000Z' },
  ]);
  const { key } = await connect(mock);
  const names = async (extra: Record<string, unknown>) => {
    const reply = await call(mock, 'POST', '/api/search/metadata', {
      key,
      body: flatBody(1, extra),
    });
    assert.equal(reply.status, 200, JSON.stringify(reply.body));
    return reply.body.assets.items.map(
      (item: { originalFileName: string }) => item.originalFileName,
    );
  };
  assert.deepEqual(await names({}), ['online.jpg']);
  assert.deepEqual(await names({ withDeleted: true }), [
    'online.jpg',
    'trashed.jpg',
    'offline.jpg',
  ]);
  assert.deepEqual(await names({ isOffline: true }), ['offline.jpg']);
  assert.deepEqual(await names({ isOffline: false, withDeleted: true }), [
    'online.jpg',
    'trashed.jpg',
  ]);
});

test('the visibility filter works in both styles; locked assets are never returned', async (t) => {
  const mock = await startMock(t);
  mock.control.addAssets([
    { path: '/mnt/photo/a.jpg', fileCreatedAt: '2024-01-01T00:00:00.000Z' },
    { path: '/mnt/photo/b.jpg', visibility: 'archive', fileCreatedAt: '2024-01-02T00:00:00.000Z' },
    { path: '/mnt/photo/c.jpg', visibility: 'hidden', fileCreatedAt: '2024-01-03T00:00:00.000Z' },
    { path: '/mnt/photo/d.jpg', visibility: 'locked', fileCreatedAt: '2024-01-04T00:00:00.000Z' },
  ]);
  const { key } = await connect(mock);
  const names = async (body: unknown) => {
    const reply = await call(mock, 'POST', '/api/search/metadata', { key, body });
    assert.equal(reply.status, 200, JSON.stringify(reply.body));
    return reply.body.assets.items.map(
      (item: { originalFileName: string }) => item.originalFileName,
    );
  };
  const order = { orderBy: { field: 'fileCreatedAt', direction: 'asc' } };
  assert.deepEqual(await names({ ...order, filter: { visibility: { eq: 'archive' } } }), ['b.jpg']);
  assert.deepEqual(
    await names({ ...order, filter: { visibility: { in: ['archive', 'hidden'] } } }),
    ['b.jpg', 'c.jpg'],
  );
  assert.deepEqual(await names({ ...order, filter: { visibility: { ne: 'timeline' } } }), [
    'b.jpg',
    'c.jpg',
  ]);
  assert.deepEqual(await names({ ...order, filter: { visibility: { notIn: ['hidden'] } } }), [
    'a.jpg',
    'b.jpg',
  ]);
  assert.deepEqual(await names({ ...order, filter: {} }), ['a.jpg', 'b.jpg', 'c.jpg']);

  assert.deepEqual(await names({ order: 'asc' }), ['a.jpg', 'b.jpg']);
  assert.deepEqual(await names({ order: 'asc', visibility: 'archive' }), ['b.jpg']);
  assert.deepEqual(await names({ order: 'asc', visibility: 'hidden' }), ['c.jpg']);
  assert.deepEqual(await names({ order: 'asc', visibility: 'locked' }), []);
});

test('updatedAt and updatedAfter find exactly what changed since the high-water mark', async (t) => {
  const { mock, key } = await seeded(t, 20);
  const everything = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(null),
  });
  const highWater = everything.body.assets.items
    .map((item: { updatedAt: string }) => item.updatedAt)
    .sort()
    .at(-1);

  const unchanged = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(null, {}, { updatedAt: { gt: highWater } }),
  });
  assert.deepEqual(unchanged.body.assets.items, []);

  const target = everything.body.assets.items[7];
  const update = await call(mock, 'PUT', '/api/assets', {
    key,
    body: { ids: [target.id], isFavorite: true },
  });
  assert.equal(update.status, 204);

  const typed = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(null, {}, { updatedAt: { gt: highWater } }),
  });
  assert.deepEqual(
    typed.body.assets.items.map((item: { id: string }) => item.id),
    [target.id],
  );
  assert.equal(typed.body.assets.items[0].isFavorite, true);
  assert.ok(typed.body.assets.items[0].updatedAt > highWater);

  const flat = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: flatBody(1, { updatedAfter: highWater }),
  });
  assert.deepEqual(
    flat.body.assets.items.map((item: { id: string }) => item.id),
    [target.id],
  );

  const exact = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: typedBody(
      null,
      {},
      {
        updatedAt: {
          gte: typed.body.assets.items[0].updatedAt,
          lte: typed.body.assets.items[0].updatedAt,
        },
      },
    ),
  });
  assert.equal(exact.body.assets.items.length, 1);
});

test('the string and library filters follow the client body', async (t) => {
  const mock = await startMock(t);
  mock.control.addAssets([
    { path: '/mnt/photo/2024/a.jpg', fileCreatedAt: '2024-01-01T00:00:00.000Z' },
    { path: '/mnt/photo/2024/b.png', fileCreatedAt: '2024-01-02T00:00:00.000Z' },
    { path: '/mnt/photo/2023/c.jpg', fileCreatedAt: '2024-01-03T00:00:00.000Z' },
    { path: '/mnt/video/2024/d.mp4', fileCreatedAt: '2024-01-04T00:00:00.000Z' },
  ]);
  const { key } = await connect(mock);
  const [photos, videos] = ['Photos', 'Videos'].map(
    (name) => mock.state.libraries().find((library) => library.name === name)?.id,
  );
  const names = async (filter: Record<string, unknown>) => {
    const reply = await call(mock, 'POST', '/api/search/metadata', {
      key,
      body: typedBody(null, {}, filter),
    });
    assert.equal(reply.status, 200, JSON.stringify(reply.body));
    return reply.body.assets.items.map(
      (item: { originalFileName: string }) => item.originalFileName,
    );
  };
  assert.deepEqual(await names({ libraryId: { eq: photos } }), ['a.jpg', 'b.png', 'c.jpg']);
  assert.deepEqual(await names({ libraryId: { eq: videos } }), ['d.mp4']);
  assert.deepEqual(await names({ libraryId: { ne: photos } }), ['d.mp4']);
  assert.deepEqual(await names({ originalPath: { startsWith: '/mnt/photo/2024/' } }), [
    'a.jpg',
    'b.png',
  ]);
  assert.deepEqual(await names({ originalPath: { endsWith: '.jpg' } }), ['a.jpg', 'c.jpg']);
  assert.deepEqual(await names({ originalFileName: { eq: 'c.jpg' } }), ['c.jpg']);
  assert.deepEqual(await names({ originalFileName: { in: ['a.jpg', 'd.mp4'] } }), [
    'a.jpg',
    'd.mp4',
  ]);
  assert.deepEqual(await names({ originalPath: { like: '/mnt/photo/%/a.jpg' } }), ['a.jpg']);
  assert.deepEqual(await names({ originalPath: { notLike: '%.jpg' } }), ['b.png', 'd.mp4']);
  assert.deepEqual(await names({ originalFileName: { ne: 'a.jpg', notIn: ['b.png'] } }), [
    'c.jpg',
    'd.mp4',
  ]);

  const flat = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: flatBody(1, { originalPath: '/mnt/photo/2024', libraryId: photos }),
  });
  assert.deepEqual(
    flat.body.assets.items.map((item: { originalFileName: string }) => item.originalFileName),
    ['a.jpg', 'b.png'],
  );
});

test('the full typed body of the client is accepted', async (t) => {
  const { mock, key } = await seeded(t, 5);
  const [library] = mock.state.libraries();
  const reply = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: {
      size: 250,
      orderBy: { field: 'fileCreatedAt', direction: 'asc' },
      withExif: true,
      filter: {
        libraryId: { eq: library?.id },
        trashedAt: { eq: null },
        isOffline: { eq: false },
        updatedAt: { gt: '2024-05-01T10:00:00.123Z' },
        visibility: { eq: 'timeline' },
        originalFileName: { eq: 'IMG_000001.jpg' },
        originalPath: { startsWith: '/mnt/photo/' },
      },
    },
  });
  assert.equal(reply.status, 200, JSON.stringify(reply.body));
  assert.equal(reply.body.assets.items.length, 1);
  assert.equal(reply.body.assets.items[0].originalFileName, 'IMG_000001.jpg');
});

test('bad bodies are 400s that name the problem', async (t) => {
  const { mock, key } = await seeded(t, 3);
  const bad = async (body: unknown) => {
    const reply = await call(mock, 'POST', '/api/search/metadata', { key, body });
    assert.equal(reply.status, 400, JSON.stringify(reply.body));
    return JSON.stringify(reply.body.message);
  };
  assert.match(await bad(typedBody(null, { size: 0 })), /size/);
  assert.match(await bad(typedBody(null, { size: 1001 })), /size/);
  assert.match(
    await bad(typedBody(null, { nonsense: true })),
    /property nonsense should not exist/,
  );
  assert.match(await bad(typedBody(null, {}, { nonsense: { eq: 1 } })), /nonsense/);
  assert.match(await bad(typedBody(null, {}, { libraryId: { eq: 'not-a-uuid' } })), /libraryId/);
  assert.match(await bad(typedBody(null, {}, { updatedAt: { gt: 'yesterday' } })), /updatedAt/);
  assert.match(await bad(typedBody(null, {}, { visibility: { eq: 'secret' } })), /visibility/);
  assert.match(await bad(typedBody('garbage')), /Invalid cursor/);
  assert.match(await bad({ order: 'sideways' }), /order/);
  assert.match(await bad({ page: 0 }), /page/);
  assert.match(await bad({ withDeleted: 'yes' }), /withDeleted/);
});

test('a server older than 3.2 refuses the typed fields like the real one', async (t) => {
  const { mock, key } = await seeded(t, 3, { serverVersion: '3.1.9' });
  const reply = await call(mock, 'POST', '/api/search/metadata', { key, body: typedBody(null) });
  assert.equal(reply.status, 400);
  assert.deepEqual(reply.body.message, [
    'property filter should not exist',
    'property orderBy should not exist',
  ]);
  const flat = await call(mock, 'POST', '/api/search/metadata', { key, body: flatBody(1) });
  assert.equal(flat.status, 200);

  const edge = await startMock(t, { serverVersion: '3.2.0' });
  const { key: edgeKey } = await connect(edge);
  const typed = await call(edge, 'POST', '/api/search/metadata', {
    key: edgeKey,
    body: typedBody(null),
  });
  assert.equal(typed.status, 200);
});

test('a lenient old server drops the typed fields and answers the flat search', async (t) => {
  const scenario = JSON.parse(readFileSync(scenarioFiles.default, 'utf8'));
  scenario.options = { legacyUnknownFields: 'ignore' };
  scenario.serverVersion = '2.5.1';
  const mock = await startMock(t, { scenario });
  mock.control.generate({ library: 'Photos', prefix: PHOTOS, count: 4 });
  const { key } = await connect(mock);
  const reply = await call(mock, 'POST', '/api/search/metadata', { key, body: typedBody(null) });
  assert.equal(reply.status, 200);
  assert.equal(reply.body.assets.items.length, 4);
  assert.equal(reply.body.assets.nextCursor, null);
});

test('an account sees only the assets of its own', async (t) => {
  const { mock } = await seeded(t, 4);
  const regular = await connect(mock, { account: USER });
  const reply = await call(mock, 'POST', '/api/search/metadata', {
    key: regular.key,
    body: typedBody(null),
  });
  assert.equal(reply.status, 200);
  assert.deepEqual(reply.body.assets.items, []);
});
