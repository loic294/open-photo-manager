import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ADMIN, APP_PERMISSIONS, call, connect, login, startMock, USER } from './helpers.ts';

test('login answers 201 with the session the client reads', async (t) => {
  const mock = await startMock(t);
  const reply = await call(mock, 'POST', '/api/auth/login', { body: ADMIN });
  assert.equal(reply.status, 201);
  assert.deepEqual(Object.keys(reply.body).sort(), [
    'accessToken',
    'isAdmin',
    'isOnboarded',
    'name',
    'profileImagePath',
    'shouldChangePassword',
    'userEmail',
    'userId',
  ]);
  assert.equal(reply.body.userEmail, ADMIN.email);
  assert.equal(reply.body.name, 'Admin');
  assert.equal(reply.body.isAdmin, true);
  assert.equal(reply.body.isOnboarded, true);
  assert.equal(reply.body.shouldChangePassword, false);
  assert.match(reply.body.accessToken, /^[\w-]{20,}$/);

  const regular = await call(mock, 'POST', '/api/auth/login', { body: USER });
  assert.equal(regular.status, 201);
  assert.equal(regular.body.isAdmin, false);
  assert.notEqual(regular.body.userId, reply.body.userId);
});

test('a wrong password or an unknown account is the same 401', async (t) => {
  const mock = await startMock(t);
  const expected = {
    message: 'Incorrect email or password',
    error: 'Unauthorized',
    statusCode: 401,
  };
  const wrong = await call(mock, 'POST', '/api/auth/login', {
    body: { email: ADMIN.email, password: 'nope' },
  });
  assert.equal(wrong.status, 401);
  assert.deepEqual(wrong.body, expected);
  const unknown = await call(mock, 'POST', '/api/auth/login', {
    body: { email: 'nobody@example.test', password: 'whatever' },
  });
  assert.equal(unknown.status, 401);
  assert.deepEqual(unknown.body, expected);
  const log = mock.state.requests().entries;
  assert.ok(JSON.stringify(log).includes('[redacted]'));
  assert.ok(!JSON.stringify(log).includes('admin-password'));
});

test('a malformed login body is a 400 listing every problem', async (t) => {
  const mock = await startMock(t);
  const bad = await call(mock, 'POST', '/api/auth/login', {
    body: { email: 'not-an-email', password: '', extra: 1 },
  });
  assert.equal(bad.status, 400);
  assert.equal(bad.body.error, 'Bad Request');
  assert.equal(bad.body.statusCode, 400);
  assert.deepEqual(bad.body.message.sort(), [
    'email must be an email',
    'password should not be empty',
    'property extra should not exist',
  ]);

  const empty = await call(mock, 'POST', '/api/auth/login', { body: {} });
  assert.equal(empty.status, 400);
  assert.ok(empty.body.message.includes('email must be a string'));

  const broken = await call(mock, 'POST', '/api/auth/login', { raw: '{"email":' });
  assert.equal(broken.status, 400);
});

test('an API key is created with the bearer token and works at once', async (t) => {
  const mock = await startMock(t);
  const token = await login(mock);
  const created = await call(mock, 'POST', '/api/api-keys', {
    token,
    body: { name: 'apm', permissions: APP_PERMISSIONS },
  });
  assert.equal(created.status, 201);
  assert.match(created.body.id, /^[0-9a-f-]{36}$/);
  assert.equal(created.body.name, 'apm');
  assert.deepEqual(created.body.permissions, APP_PERMISSIONS);
  assert.equal(typeof created.body.secret, 'string');
  assert.equal(created.body.apiKey.id, created.body.id);

  const me = await call(mock, 'GET', '/api/users/me', { key: created.body.secret });
  assert.equal(me.status, 200);
  assert.equal(me.body.email, ADMIN.email);
  assert.equal(me.body.isAdmin, true);

  const [stored] = mock.state.keys();
  assert.equal(stored?.id, created.body.id);
  assert.ok(!JSON.stringify(mock.state.keys()).includes(created.body.secret));
  const entry = mock.state.requests().entries.at(-1);
  assert.equal(entry?.auth, 'apiKey');
  assert.equal(entry?.keyName, 'apm');
});

test('creating a key needs a session or the apiKey.create permission, and valid permissions', async (t) => {
  const mock = await startMock(t);
  const token = await login(mock);

  const unknown = await call(mock, 'POST', '/api/api-keys', {
    token,
    body: { name: 'apm', permissions: ['asset.read', 'bogus.permission'] },
  });
  assert.equal(unknown.status, 400);
  assert.ok(
    unknown.body.message.some((line: string) =>
      line.startsWith('permissions.1 must be one of the following values: all, '),
    ),
    JSON.stringify(unknown.body),
  );

  const none = await call(mock, 'POST', '/api/api-keys', { token, body: { name: 'apm' } });
  assert.equal(none.status, 400);
  const empty = await call(mock, 'POST', '/api/api-keys', {
    token,
    body: { name: 'apm', permissions: [] },
  });
  assert.equal(empty.status, 400);

  const anonymous = await call(mock, 'POST', '/api/api-keys', {
    body: { name: 'apm', permissions: ['all'] },
  });
  assert.equal(anonymous.status, 401);

  const { key } = await connect(mock, { permissions: ['asset.read'] });
  const lacking = await call(mock, 'POST', '/api/api-keys', {
    key,
    body: { name: 'child', permissions: ['asset.read'] },
  });
  assert.equal(lacking.status, 403);
  assert.equal(lacking.body.message, 'Missing required permission: apiKey.create');
});

test('a key is held to its permissions (403) and a wrong key is a 401', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock, { permissions: ['asset.read'] });

  const search = await call(mock, 'POST', '/api/search/metadata', { key, body: {} });
  assert.equal(search.status, 200);
  const me = await call(mock, 'GET', '/api/users/me', { key });
  assert.equal(me.status, 403);
  assert.deepEqual(me.body, {
    message: 'Missing required permission: user.read',
    error: 'Forbidden',
    statusCode: 403,
  });
  const update = await call(mock, 'PUT', '/api/assets', { key, body: { ids: [] } });
  assert.equal(update.status, 403);
  assert.equal(update.body.message, 'Missing required permission: asset.update');

  const wrong = await call(mock, 'GET', '/api/users/me', { key: 'not-a-key' });
  assert.equal(wrong.status, 401);
  assert.equal(wrong.body.message, 'Invalid API key');
  assert.equal(mock.state.requests().entries.at(-1)?.auth, 'invalid');
  const missing = await call(mock, 'GET', '/api/users/me');
  assert.equal(missing.status, 401);
  assert.equal(mock.state.requests().entries.at(-1)?.auth, 'none');

  const everything = await connect(mock, { permissions: ['all'] });
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: everything.key })).status, 200);
});

test('a regular account gets 403 on the libraries, an administrator does not', async (t) => {
  const mock = await startMock(t);
  const regular = await connect(mock, { account: USER });
  const forbidden = await call(mock, 'GET', '/api/libraries', { key: regular.key });
  assert.equal(forbidden.status, 403);
  assert.deepEqual(forbidden.body, { message: 'Forbidden', error: 'Forbidden', statusCode: 403 });
  const me = await call(mock, 'GET', '/api/users/me', { key: regular.key });
  assert.equal(me.status, 200);
  assert.equal(me.body.isAdmin, false);

  const admin = await connect(mock);
  assert.equal((await call(mock, 'GET', '/api/libraries', { key: admin.key })).status, 200);
});

test('a key revokes itself, and the control endpoint revokes keys by name or all', async (t) => {
  const mock = await startMock(t);
  const first = await connect(mock, { name: 'first' });
  const gone = await call(mock, 'DELETE', `/api/api-keys/${first.keyId}`, { key: first.key });
  assert.equal(gone.status, 204);
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: first.key })).status, 401);

  const unknown = await call(mock, 'DELETE', '/api/api-keys/00000000-0000-4000-8000-000000000001', {
    token: first.token,
  });
  assert.equal(unknown.status, 400);
  assert.equal(unknown.body.message, 'API Key not found');

  const a = await connect(mock, { name: 'a' });
  const b = await connect(mock, { name: 'b' });
  const revokedA = await call(mock, 'POST', '/__mock/revoke-key', { body: { name: 'a' } });
  assert.deepEqual(revokedA.body, { revoked: 1 });
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: a.key })).status, 401);
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: b.key })).status, 200);
  const revokedAll = await call(mock, 'POST', '/__mock/revoke-key', { body: {} });
  assert.deepEqual(revokedAll.body, { revoked: 1 });
  assert.equal((await call(mock, 'GET', '/api/users/me', { key: b.key })).status, 401);
  assert.equal(mock.control.revokeKey(), 0);
});

test('a login session is accepted only where the application uses one', async (t) => {
  const mock = await startMock(t);
  const token = await login(mock);
  const refused = await call(mock, 'GET', '/api/users/me', { token });
  assert.equal(refused.status, 401);
  assert.match(refused.body.message, /login session only/);

  const loggedOut = await call(mock, 'POST', '/api/auth/logout', { token });
  assert.equal(loggedOut.status, 200);
  assert.deepEqual(loggedOut.body, { successful: true, redirectUri: '/auth/login?autoLaunch=0' });
  const stale = await call(mock, 'POST', '/api/api-keys', {
    token,
    body: { name: 'apm', permissions: ['all'] },
  });
  assert.equal(stale.status, 401);
  assert.equal(stale.body.message, 'Invalid user token');

  const { key } = await connect(mock);
  const withKey = await call(mock, 'POST', '/api/auth/logout', { key });
  assert.equal(withKey.status, 403);
});

test('ping and version need no credential; about and me do', async (t) => {
  const mock = await startMock(t);
  assert.deepEqual((await call(mock, 'GET', '/api/server/ping')).body, { res: 'pong' });
  const version = await call(mock, 'GET', '/api/server/version');
  assert.equal(version.status, 200);
  assert.equal(version.body.major, 3);
  assert.equal(version.body.minor, 4);
  assert.equal(version.body.patch, 0);
  assert.equal((await call(mock, 'GET', '/api/server/about')).status, 401);

  const { key } = await connect(mock);
  const about = await call(mock, 'GET', '/api/server/about', { key });
  assert.equal(about.status, 200);
  assert.equal(about.body.version, 'v3.4.0');
  assert.equal(about.body.licensed, false);
  assert.match(about.body.versionUrl, /releases\/tag\/v3\.4\.0$/);
});

test('the server version is a scenario or option choice, release candidates included', async (t) => {
  const rc = await startMock(t, { serverVersion: '3.4.0-rc.0' });
  const { key } = await connect(rc);
  const about = await call(rc, 'GET', '/api/server/about', { key });
  assert.equal(about.body.version, 'v3.4.0-rc.0');
  assert.equal((await call(rc, 'GET', '/api/server/version')).body.prerelease, 0);

  const old = await startMock(t, { serverVersion: 'v2.5.1' });
  const version = await call(old, 'GET', '/api/server/version');
  assert.deepEqual([version.body.major, version.body.minor, version.body.patch], [2, 5, 1]);
});
