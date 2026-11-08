import assert from 'node:assert/strict';
import { relative } from 'node:path';
import { test } from 'node:test';
import { ScenarioError, scenarioFiles, startMockImmich, testUsers } from '../index.ts';
import { ADMIN, call, connect, login, scratchDir, startMock, USER, writeTree } from './helpers.ts';

const MINIMAL = {
  users: [{ email: 'boss@example.test', password: 'boss-password', name: 'Boss', isAdmin: true }],
  libraries: [{ name: 'Main', importPaths: ['/mnt/main'], exclusionPatterns: [] }],
};

const ID = '11111111-1111-4111-8111-111111111111';

test('the mock listens on the loopback interface, on a free port, and keeps nothing else', async (t) => {
  const mock = await startMock(t);
  assert.equal(mock.host, '127.0.0.1');
  assert.equal(mock.tls, false);
  assert.equal(mock.certificate, undefined);
  assert.ok(mock.port > 0);
  assert.equal(mock.url, `http://127.0.0.1:${mock.port}`);
  assert.deepEqual((await call(mock, 'GET', '/api/server/ping')).body, { res: 'pong' });
});

test('stop() can be called twice, and the port is free afterwards', async () => {
  const mock = await startMockImmich({ scanDelayMs: 0 });
  await Promise.all([mock.stop(), mock.stop()]);
  await mock.stop();
  await assert.rejects(fetch(`${mock.url}/api/server/ping`));
});

test('the default scenario has the two test accounts, two libraries, albums and tags', async (t) => {
  const mock = await startMock(t);
  assert.deepEqual(
    mock.state.users().map((user) => [user.email, user.isAdmin]),
    [
      ['admin@example.test', true],
      ['user@example.test', false],
    ],
  );
  assert.deepEqual(
    mock.state.libraries().map((library) => [library.name, library.importPaths]),
    [
      ['Photos', ['/mnt/photo']],
      ['Videos', ['/mnt/video']],
    ],
  );
  assert.deepEqual(
    mock.state.albums().map((album) => album.name),
    ['Holidays', 'Family', 'To print'],
  );
  assert.deepEqual(
    mock.state.tags().map((tag) => [tag.name, tag.value]),
    [
      ['Travel', 'Travel'],
      ['Birthdays', 'Family/Birthdays'],
    ],
  );
  assert.equal(mock.state.assets().length, 0);
  assert.ok(await login(mock, ADMIN));
  assert.ok(await login(mock, USER));
});

test('the shipped scenarios are exported with the test accounts, and every one starts', async (t) => {
  assert.deepEqual(Object.keys(scenarioFiles).sort(), ['default', 'fixtures', 'limited']);
  assert.equal(testUsers.admin.email, ADMIN.email);
  assert.equal(testUsers.admin.password, ADMIN.password);
  assert.equal(testUsers.user.email, USER.email);
  assert.equal(testUsers.user.password, USER.password);
  for (const file of Object.values(scenarioFiles)) {
    const mock = await startMock(t, { scenario: file });
    assert.equal((await call(mock, 'GET', '/api/server/ping')).status, 200, file);
    await mock.stop();
  }
});

test('the fixtures scenario: the default users, albums and tags, and the two fixture volumes', async (t) => {
  const fixtures = await startMock(t, { scenario: scenarioFiles.fixtures });
  const regular = await startMock(t);

  assert.deepEqual(fixtures.state.users(), regular.state.users());
  assert.deepEqual(fixtures.state.albums(), regular.state.albums());
  assert.deepEqual(fixtures.state.tags(), regular.state.tags());
  assert.deepEqual(
    fixtures.state
      .libraries()
      .map((library) => [library.name, library.importPaths, library.exclusionPatterns]),
    [
      ['Photos', ['/mnt/photos'], ['**/@eaDir/**']],
      ['Archive', ['/mnt/archive'], []],
    ],
  );
  assert.equal(fixtures.state.assets().length, 0);
  assert.ok(await login(fixtures, ADMIN));
});

test('the limited scenario: one regular user, on an old server', async (t) => {
  const mock = await startMock(t, { scenario: scenarioFiles.limited });
  assert.deepEqual(
    mock.state.users().map((user) => [user.email, user.isAdmin]),
    [['user@example.test', false]],
  );
  const refused = await call(mock, 'POST', '/api/auth/login', { body: ADMIN });
  assert.equal(refused.status, 401);
  const version = await call(mock, 'GET', '/api/server/version');
  assert.deepEqual([version.body.major, version.body.minor, version.body.patch], [2, 5, 1]);
  const { key } = await connect(mock, { account: USER });
  const flat = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: { page: 1, size: 5 },
  });
  assert.equal(flat.status, 200);
  const typed = await call(mock, 'POST', '/api/search/metadata', {
    key,
    body: { filter: { trashedAt: { eq: null } }, size: 5 },
  });
  assert.equal(typed.status, 400);
});

test('the ids are the same on every start, so a stored mapping stays valid', async (t) => {
  const first = await startMock(t);
  const second = await startMock(t);
  const ids = (mock: typeof first) => ({
    users: mock.state.users().map((user) => user.id),
    libraries: mock.state.libraries().map((library) => library.id),
    albums: mock.state.albums().map((album) => album.id),
    tags: mock.state.tags().map((tag) => tag.id),
  });
  assert.deepEqual(ids(first), ids(second));
  for (const id of Object.values(ids(first)).flat()) {
    assert.match(String(id), /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
  }
  const keys = [first, second].map((mock) => connect(mock));
  const [a, b] = await Promise.all(keys);
  assert.notEqual(a?.key, b?.key, 'sessions and keys are random');
});

test('a scenario can be given as a parsed object', async (t) => {
  const mock = await startMock(t, {
    scenario: {
      description: 'rich',
      serverVersion: '3.3.0',
      users: [{ id: ID, email: 'boss@example.test', password: 'pw', name: 'Boss', isAdmin: true }],
      libraries: [{ name: 'Main', importPaths: ['/mnt/main'], exclusionPatterns: ['**/skip/**'] }],
      assets: [
        {
          path: '/mnt/main/a.jpg',
          fileCreatedAt: '2024-01-01T00:00:00.000Z',
          isFavorite: true,
          rating: 4,
          description: 'first',
        },
        { path: '/mnt/main/b.mp4', type: 'VIDEO', visibility: 'archive' },
        { path: '/mnt/main/gone.jpg', isOffline: true },
      ],
      generate: [{ library: 'Main', prefix: '/mnt/main/gen/IMG_', count: 5 }],
      albums: [{ name: 'Best', assets: ['/mnt/main/a.jpg'] }],
      tags: [{ name: 'Solo' }, { value: 'Trips/2024' }],
    },
  });
  assert.equal(mock.state.users()[0]?.id, ID);
  assert.equal(mock.state.assets().length, 8);
  const first = mock.state.assetByPath('/mnt/main/a.jpg');
  assert.equal(first?.isFavorite, true);
  assert.equal(first?.rating, 4);
  assert.equal(first?.description, 'first');
  assert.equal(first?.fileCreatedAt, '2024-01-01T00:00:00.000Z');
  assert.equal(mock.state.assetByPath('/mnt/main/b.mp4')?.type, 'VIDEO');
  assert.equal(mock.state.assetByPath('/mnt/main/b.mp4')?.visibility, 'archive');
  const gone = mock.state.assetByPath('/mnt/main/gone.jpg');
  assert.equal(gone?.isOffline, true);
  assert.notEqual(gone?.deletedAt, null, 'an offline asset is in the trash');
  assert.ok(mock.state.assetByPath('/mnt/main/gen/IMG_000004.jpg'));
  assert.equal(mock.state.albums()[0]?.assetIds.length, 1);
  assert.deepEqual(
    mock.state.tags().map((tag) => [tag.name, tag.value]),
    [
      ['Solo', 'Solo'],
      ['2024', 'Trips/2024'],
    ],
  );
  const about = await call(mock, 'GET', '/api/server/version');
  assert.deepEqual([about.body.major, about.body.minor], [3, 3]);
});

test('a scenario can be given as a file path, absolute or relative to the working directory', async (t) => {
  const dir = scratchDir(t, 'scenario');
  writeTree(dir, { 'mine.json': JSON.stringify({ ...MINIMAL, description: 'from a file' }) });
  const absolute = `${dir}/mine.json`;
  const byAbsolute = await startMock(t, { scenario: absolute });
  assert.equal(byAbsolute.state.snapshot().description, 'from a file');
  assert.deepEqual(
    byAbsolute.state.users().map((user) => user.email),
    ['boss@example.test'],
  );

  const byRelative = await startMock(t, { scenario: relative(process.cwd(), absolute) });
  assert.equal(byRelative.state.snapshot().description, 'from a file');
});

test('a bad scenario is refused with every problem, each with its path', async () => {
  const bad = {
    colour: 'red',
    serverVersion: 'three',
    users: [
      { email: 'a@example.test', password: 'x' },
      { email: 'a@example.test', password: 'y' },
    ],
    libraries: [{ name: 'Main', importPaths: ['relative/path'], owner: 'nobody@example.test' }],
    assets: [{ path: '/elsewhere/a.jpg' }],
    tags: [{}],
  };
  const error = await startMockImmich({ scenario: bad }).then(
    () => assert.fail('the scenario must be refused'),
    (reason: unknown) => reason,
  );
  assert.ok(error instanceof ScenarioError);
  const text = error.errors.join('\n');
  for (const expected of [
    'property scenario.colour should not exist',
    'scenario.serverVersion',
    'scenario.users[1].email is already used by users[0]',
    'scenario.libraries[0].importPaths must hold absolute paths',
    'scenario.libraries[0].owner must be the email of a user',
    'scenario.assets[0].path lies under no import path',
    'scenario.tags[0].value or name is required',
  ]) {
    assert.ok(text.includes(expected), `${expected}\n--- got:\n${text}`);
  }
  assert.ok(error.errors.length >= 7);
  assert.match(error.message, /^invalid scenario \(scenario object\):/);
});

test('a scenario file that is missing or is not JSON is refused, and so is an album of unknown assets', async (t) => {
  const dir = scratchDir(t, 'scenario');
  writeTree(dir, { 'broken.json': '{ not json' });
  await assert.rejects(
    startMockImmich({ scenario: `${dir}/missing.json` }),
    (error: unknown) =>
      error instanceof ScenarioError && /cannot read the file/.test(error.errors.join()),
  );
  await assert.rejects(
    startMockImmich({ scenario: `${dir}/broken.json` }),
    (error: unknown) =>
      error instanceof ScenarioError && /not valid JSON/.test(error.errors.join()),
  );
  await assert.rejects(
    startMockImmich({
      scenario: { ...MINIMAL, albums: [{ name: 'Lost', assets: ['/mnt/main/nothing.jpg'] }] },
    }),
    /scenario\.albums\[0\]\.assets\[0\] has no asset at \/mnt\/main\/nothing\.jpg/,
  );
  await assert.rejects(startMockImmich({ scenario: { ...MINIMAL, users: [] } }), ScenarioError);
});

test('mounts given as an object lead the scan to the real directories', async (t) => {
  const photos = scratchDir(t, 'photos');
  const archive = scratchDir(t, 'archive');
  writeTree(photos, {
    '2023/a.jpg': 'a',
    '2023/b.jpg': 'b',
    '@eaDir/a.jpg/thumb.jpg': 'thumbnail',
  });
  writeTree(archive, { 'old/c.jpg': 'c', 'notes.txt': 'not media' });
  const mock = await startMock(t, {
    scenario: scenarioFiles.fixtures,
    mounts: { '/mnt/photos': photos, '/mnt/archive': archive },
  });

  const jobs = await mock.control.scan();
  assert.equal(jobs.length, 2);
  assert.ok(jobs.every((job) => job.status === 'completed'));
  assert.deepEqual(
    mock.state
      .assets()
      .map((asset) => asset.originalPath)
      .sort(),
    ['/mnt/archive/old/c.jpg', '/mnt/photos/2023/a.jpg', '/mnt/photos/2023/b.jpg'],
  );
  const counts = Object.fromEntries(
    mock.state.libraries().map((library) => [library.name, library.assetCount]),
  );
  assert.deepEqual(counts, { Photos: 2, Archive: 1 });
  assert.deepEqual(
    mock.state.snapshot().mounts,
    [
      { prefix: '/mnt/photos', dir: photos },
      { prefix: '/mnt/archive', dir: archive },
    ].sort((a, b) => b.prefix.length - a.prefix.length),
  );
});

test('a mount that makes no sense stops the start with a clear message', async (t) => {
  const dir = scratchDir(t, 'mount');
  writeTree(dir, { 'file.txt': 'x' });
  const refused = (mounts: Record<string, string>) => startMockImmich({ mounts });
  await assert.rejects(refused({ 'mnt/photos': dir }), /must be an absolute path/);
  await assert.rejects(refused({ '/mnt/../etc': dir }), /must not contain "\.\."/);
  await assert.rejects(refused({ '/mnt/photos': `${dir}/missing` }), /does not exist/);
  await assert.rejects(refused({ '/mnt/photos': `${dir}/file.txt` }), /is not a directory/);
  await assert.rejects(refused({ '/mnt/photos': '' }), /non-empty path/);
  await assert.rejects(refused({ '/mnt/photos': dir, '/mnt/photos/': dir }), /is given twice/);
});

test('an explicit option beats the scenario, which beats the default', async (t) => {
  const scenario = {
    ...MINIMAL,
    options: { scanDelayMs: 300, latencyMs: 5, unknownAssetStatus: 404 },
  };
  const start = async (options: Parameters<typeof startMockImmich>[0]) => {
    const mock = await startMockImmich(options);
    t.after(() => mock.stop());
    return mock.state.snapshot().settings;
  };
  assert.deepEqual(await start({ scenario }), {
    scanDelayMs: 300,
    latencyMs: 5,
    legacyUnknownFields: 'reject',
    unknownAssetStatus: 404,
  });
  assert.deepEqual(await start({ scenario, scanDelayMs: 10, latencyMs: 0 }), {
    scanDelayMs: 10,
    latencyMs: 0,
    legacyUnknownFields: 'reject',
    unknownAssetStatus: 404,
  });
  assert.deepEqual(await start({}), {
    scanDelayMs: 250,
    latencyMs: 0,
    legacyUnknownFields: 'reject',
    unknownAssetStatus: 400,
  });
});

test('the server version comes from the option, then the scenario, then 3.4.0', async (t) => {
  const versionOf = async (options: Parameters<typeof startMockImmich>[0]) => {
    const mock = await startMockImmich({ scanDelayMs: 0, ...options });
    t.after(() => mock.stop());
    const { body } = await call(mock, 'GET', '/api/server/version');
    return [body.major, body.minor, body.patch].join('.');
  };
  assert.equal(await versionOf({}), '3.4.0');
  assert.equal(await versionOf({ scenario: scenarioFiles.limited }), '2.5.1');
  assert.equal(
    await versionOf({ scenario: scenarioFiles.limited, serverVersion: '3.2.0' }),
    '3.2.0',
  );
  await assert.rejects(startMockImmich({ serverVersion: 'abc' }), /invalid server version/);
});

test('options that do not exist or make no sense are refused, all at once', async () => {
  await assert.rejects(
    startMockImmich({ colour: 'red' } as never),
    /property options\.colour should not exist/,
  );
  const error = await startMockImmich({
    port: 70_000,
    scanDelayMs: 1.5,
    latencyMs: -1,
    tls: 'yes',
    host: '',
  } as never).then(
    () => assert.fail('the options must be refused'),
    (reason: unknown) => reason,
  );
  assert.ok(error instanceof Error);
  assert.match(error.message, /^invalid options:/);
  for (const field of ['port', 'scanDelayMs', 'latencyMs', 'tls', 'host']) {
    assert.ok(error.message.includes(`options.${field}`), `${field}\n${error.message}`);
  }
});

test('a port that is taken is a startup error, not a crash', async (t) => {
  const first = await startMock(t);
  await assert.rejects(startMockImmich({ port: first.port }), /EADDRINUSE|address already in use/);
});
