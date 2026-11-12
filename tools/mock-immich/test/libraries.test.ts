import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ADMIN,
  call,
  connect,
  eventually,
  scratchDir,
  startMock,
  USER,
  writeTree,
} from './helpers.ts';

test('the libraries are listed with the fields the client reads', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  const reply = await call(mock, 'GET', '/api/libraries', { key });
  assert.equal(reply.status, 200);
  assert.equal(reply.body.length, 2);
  const photos = reply.body.find((library: { name: string }) => library.name === 'Photos');
  assert.deepEqual(photos.importPaths, ['/mnt/photo']);
  assert.deepEqual(photos.exclusionPatterns, ['**/@eaDir/**']);
  assert.equal(photos.assetCount, 0);
  assert.equal(photos.refreshedAt, null);
  for (const field of ['id', 'ownerId', 'createdAt', 'updatedAt']) {
    assert.equal(typeof photos[field], 'string', field);
  }
  const admin = mock.state.users().find((user) => user.email === ADMIN.email);
  assert.equal(photos.ownerId, admin?.id);

  const one = await call(mock, 'GET', `/api/libraries/${photos.id}`, { key });
  assert.equal(one.status, 200);
  assert.deepEqual(one.body, photos);
});

test('the ids of the scenario are the same on every start', async (t) => {
  const first = await startMock(t);
  const second = await startMock(t);
  assert.deepEqual(
    first.state.libraries().map((library) => library.id),
    second.state.libraries().map((library) => library.id),
  );
});

test('a library of somebody else is "not found", and a malformed id is a 400', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  const unknown = await call(mock, 'GET', '/api/libraries/00000000-0000-4000-8000-000000000009', {
    key,
  });
  assert.equal(unknown.status, 400);
  assert.equal(unknown.body.message, 'Not found or no library.read access');
  const malformed = await call(mock, 'GET', '/api/libraries/not-a-uuid', { key });
  assert.equal(malformed.status, 400);
  assert.equal(malformed.body.message, 'id must be a UUID');
  const regular = await connect(mock, { account: USER });
  const hidden = await call(mock, 'GET', `/api/libraries/${mock.state.libraries()[0]?.id}`, {
    key: regular.key,
  });
  assert.equal(hidden.status, 403);
});

test('the exclusion patterns are updated alone, like the application does', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  const [library] = mock.state.libraries();
  const patterns = ['**/@eaDir/**', '**/.opm-trash/**'];
  const updated = await call(mock, 'PUT', `/api/libraries/${library?.id}`, {
    key,
    body: { exclusionPatterns: patterns },
  });
  assert.equal(updated.status, 200);
  assert.deepEqual(updated.body.exclusionPatterns, patterns);
  assert.deepEqual(updated.body.importPaths, library?.importPaths);
  assert.equal(updated.body.name, library?.name);
  assert.notEqual(updated.body.updatedAt, library?.updatedAt);
  const again = await call(mock, 'GET', `/api/libraries/${library?.id}`, { key });
  assert.deepEqual(again.body.exclusionPatterns, patterns);

  const bad = await call(mock, 'PUT', `/api/libraries/${library?.id}`, {
    key,
    body: { exclusionPatterns: 'nope', other: true },
  });
  assert.equal(bad.status, 400);
  assert.ok(bad.body.message.includes('property other should not exist'));
});

test('a scan request is answered at once and the files show up after the scan delay', async (t) => {
  const dir = scratchDir(t, 'libraries-scan');
  writeTree(dir, { 'a.jpg': 'a', 'sub/b.png': 'b' });
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir }, scanDelayMs: 150 });
  const { key } = await connect(mock);
  const [library] = mock.state.libraries();

  const started = Date.now();
  const reply = await call(mock, 'POST', `/api/libraries/${library?.id}/scan`, { key });
  assert.equal(reply.status, 204);
  assert.ok(Date.now() - started < 140, 'the scan request must not wait for the scan');
  assert.equal(mock.state.assets().length, 0);

  await eventually(() => assert.equal(mock.state.assets().length, 2));
  const [job] = mock.state.jobs();
  assert.equal(job?.status, 'completed');
  assert.equal(job?.summary?.imported, 2);
  const listed = await call(mock, 'GET', `/api/libraries/${library?.id}`, { key });
  assert.equal(listed.body.assetCount, 2);
  assert.notEqual(listed.body.refreshedAt, null);
});

test('with no scan delay the scan request returns when the scan is over', async (t) => {
  const dir = scratchDir(t, 'libraries-sync-scan');
  writeTree(dir, { 'a.jpg': 'a' });
  const mock = await startMock(t, { mounts: { '/mnt/photo': dir } });
  const { key } = await connect(mock);
  const [library] = mock.state.libraries();
  const reply = await call(mock, 'POST', `/api/libraries/${library?.id}/scan`, { key });
  assert.equal(reply.status, 204);
  assert.equal(mock.state.assets().length, 1);
});

test('an unknown route is a 404 and is flagged in the request log', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  const reply = await call(mock, 'GET', '/api/does-not-exist', { key });
  assert.equal(reply.status, 404);
  assert.equal(reply.body.message, 'Cannot GET /api/does-not-exist');
  const outside = await call(mock, 'GET', '/somewhere/else');
  assert.equal(outside.status, 404);
  const entries = mock.state.requests().entries;
  assert.equal(entries.at(-2)?.unexpected, true);
  assert.equal(entries.at(-1)?.unexpected, true);
  const known = entries.find((entry) => entry.path === '/api/auth/login');
  assert.equal(known?.unexpected, undefined);
});
