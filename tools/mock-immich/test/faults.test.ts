import assert from 'node:assert/strict';
import { test } from 'node:test';
import { call, connect, startMock } from './helpers.ts';

const SEARCH = '/api/search/metadata';

test('a failure rule answers the status asked for, for as many calls as `times` says', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  const rule = mock.control.fail({ match: 'GET /api/users/me', status: 503, times: 2 });
  assert.deepEqual(rule, {
    id: rule.id,
    match: 'GET /api/users/me',
    status: 503,
    times: 2,
    after: 0,
    delayMs: 5000,
    seen: 0,
    fired: 0,
  });

  const first = await call(mock, 'GET', '/api/users/me', { key });
  assert.equal(first.status, 503);
  assert.deepEqual(first.body, {
    message: 'failure injected by the mock',
    error: 'Service Unavailable',
    statusCode: 503,
  });
  assert.equal((await call(mock, 'GET', '/api/users/me', { key })).status, 503);
  assert.equal((await call(mock, 'GET', '/api/users/me', { key })).status, 200);
  assert.equal((await call(mock, 'GET', '/api/server/about', { key })).status, 200);

  const [view] = mock.control.failures();
  assert.equal(view?.seen, 3);
  assert.equal(view?.fired, 2);
  const log = mock.state.requests().entries.filter((entry) => entry.path === '/api/users/me');
  assert.deepEqual(
    log.map((entry) => [entry.status, entry.injected]),
    [
      [503, 'fail'],
      [503, 'fail'],
      [200, undefined],
    ],
  );
});

test('`after` lets some calls through first, and a rule with no `times` never stops', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.fail({ match: 'POST /api/search/metadata', status: 500, after: 2, times: 1 });
  const statuses: number[] = [];
  for (let i = 0; i < 5; i += 1) {
    statuses.push((await call(mock, 'POST', SEARCH, { key, body: { order: 'asc' } })).status);
  }
  assert.deepEqual(statuses, [200, 200, 500, 200, 200]);

  mock.control.clearFailures();
  mock.control.fail({ match: '/api/search/*', status: 429 });
  for (let i = 0; i < 4; i += 1) {
    assert.equal((await call(mock, 'POST', SEARCH, { key, body: {} })).status, 429);
  }
});

test('a rule is matched on the method and on the path, with * as a wildcard', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.fail({ match: 'PUT /api/assets', status: 500 });
  const [library] = mock.state.libraries();
  const id = '00000000-0000-4000-8000-000000000001';
  assert.equal((await call(mock, 'PUT', '/api/assets', { key, body: { ids: [id] } })).status, 500);
  assert.equal(
    (await call(mock, 'DELETE', '/api/assets', { key, body: { ids: [id] } })).status,
    400,
  );
  assert.equal((await call(mock, 'GET', `/api/libraries/${library?.id}`, { key })).status, 200);

  mock.control.clearFailures();
  mock.control.fail({ match: '/api/libraries/*', status: 502 });
  assert.equal((await call(mock, 'GET', `/api/libraries/${library?.id}`, { key })).status, 502);
  assert.equal((await call(mock, 'GET', '/api/libraries', { key })).status, 200);
});

test('a dropped call closes the connection without an answer', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.fail({ match: 'GET /api/users/me', status: 'drop', times: 1 });
  await assert.rejects(call(mock, 'GET', '/api/users/me', { key }));
  assert.equal((await call(mock, 'GET', '/api/users/me', { key })).status, 200);
  const entry = mock.state.requests().entries.find((item) => item.injected === 'drop');
  assert.equal(entry?.status, 0);
  assert.equal(entry?.path, '/api/users/me');
});

test('a slow call is answered normally after the delay', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.fail({ match: 'GET /api/users/me', status: 'slow', delayMs: 200, times: 1 });
  const started = Date.now();
  const slow = await call(mock, 'GET', '/api/users/me', { key });
  assert.equal(slow.status, 200);
  assert.ok(Date.now() - started >= 190);
  const fast = Date.now();
  assert.equal((await call(mock, 'GET', '/api/users/me', { key })).status, 200);
  assert.ok(Date.now() - fast < 150);
  const entry = mock.state.requests().entries.find((item) => item.injected === 'slow');
  assert.equal(entry?.status, 200);
  assert.ok((entry?.durationMs ?? 0) >= 190);
});

test('the control API cannot be failed, delayed or held', async (t) => {
  const mock = await startMock(t);
  mock.control.fail({ match: '/*', status: 500 });
  mock.control.latency(200);
  mock.control.gate('/*');
  const started = Date.now();
  const state = await call(mock, 'GET', '/__mock/state');
  assert.equal(state.status, 200);
  assert.ok(Date.now() - started < 150);
  assert.equal((await call(mock, 'GET', '/__mock/fail')).body.rules.length, 1);
  assert.equal(mock.state.requests().entries.length, 0);
});

test('failure rules are managed over HTTP too', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  const created = await call(mock, 'POST', '/__mock/fail', {
    body: { match: 'GET /api/server/about', status: 504, times: 1, after: 1 },
  });
  assert.equal(created.status, 200);
  assert.equal(created.body.status, 504);
  assert.equal(created.body.after, 1);
  assert.equal((await call(mock, 'GET', '/__mock/fail')).body.rules.length, 1);
  assert.equal((await call(mock, 'GET', '/api/server/about', { key })).status, 200);
  assert.equal((await call(mock, 'GET', '/api/server/about', { key })).status, 504);
  assert.deepEqual((await call(mock, 'DELETE', '/__mock/fail')).body, { cleared: true });
  assert.deepEqual((await call(mock, 'GET', '/__mock/fail')).body, { rules: [] });

  const bad = async (body: unknown) => {
    const reply = await call(mock, 'POST', '/__mock/fail', { body });
    assert.equal(reply.status, 400, JSON.stringify(reply.body));
  };
  await bad({ match: '/api/x', status: 200 });
  await bad({ match: '/api/x', status: 700 });
  await bad({ match: '/api/x', status: 'explode' });
  await bad({ match: 'no slash', status: 500 });
  await bad({ status: 500 });
  await bad({ match: '/api/x', status: 500, times: 0 });
  await bad({ match: '/api/x', status: 500, colour: 'red' });
  assert.throws(() => mock.control.fail({ match: '/api/x', status: 200 }));
});

test('a gate holds the matching calls until it is released', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  const gate = mock.control.gate('POST /api/search/metadata', { times: 1 });
  assert.equal(gate.held, 0);

  const pending = call(mock, 'POST', SEARCH, { key, body: {} });
  const held = await mock.control.waitForHeld(1, 2000);
  assert.equal(held[0]?.method, 'POST');
  assert.equal(held[0]?.path, SEARCH);
  const entry = mock.state.requests().entries.at(-1);
  assert.equal(entry?.status, 0);
  assert.equal(entry?.injected, 'gate');

  const other = await call(mock, 'POST', SEARCH, { key, body: {} });
  assert.equal(other.status, 200, 'a gate with times: 1 holds one call only');
  assert.equal(mock.control.releaseGate(), 1);
  const reply = await pending;
  assert.equal(reply.status, 200);
  assert.deepEqual(mock.state.snapshot().held, []);
  const done = mock.state.requests().entries.find((item) => item.injected === 'gate');
  assert.equal(done?.status, 200);
});

test('gates work over HTTP, and waiting for a call that never comes is a 408', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  const made = await call(mock, 'POST', '/__mock/gate', { body: { match: '/api/server/about' } });
  assert.equal(made.status, 200);
  assert.equal(made.body.held, 0);

  const timedOut = await call(mock, 'GET', '/__mock/gate?waitHeld=1&timeoutMs=100');
  assert.equal(timedOut.status, 408);

  const pending = call(mock, 'GET', '/api/server/about', { key });
  const waited = await call(mock, 'GET', '/__mock/gate?waitHeld=1&timeoutMs=3000');
  assert.equal(waited.status, 200);
  assert.equal(waited.body.held.length, 1);
  assert.equal(waited.body.gates[0].held, 1);
  const released = await call(mock, 'POST', '/__mock/gate/release');
  assert.deepEqual(released.body, { released: 1 });
  assert.equal((await pending).status, 200);
  assert.equal((await call(mock, 'GET', '/api/server/about', { key })).status, 200);
});

test('a call that is held and given up is forgotten', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.gate('/api/users/me');
  const controller = new AbortController();
  const pending = fetch(`${mock.url}/api/users/me`, {
    headers: { 'x-api-key': key },
    signal: controller.signal,
  });
  await mock.control.waitForHeld(1, 2000);
  controller.abort();
  await assert.rejects(pending);
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.deepEqual(mock.state.snapshot().held, []);
});

test('a reset lets held calls go and forgets the gates and the rules', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.gate('GET /api/users/me');
  const pending = call(mock, 'GET', '/api/users/me', { key });
  await mock.control.waitForHeld(1, 2000);
  mock.control.fail({ match: '/api/*', status: 500 });
  mock.control.reset();
  assert.deepEqual(mock.control.failures(), []);
  assert.deepEqual(mock.state.snapshot().gates, []);
  const reply = await pending;
  assert.equal(reply.status, 401, 'the key is gone after the reset');
});

test('latency delays every API call and can be set over HTTP', async (t) => {
  const mock = await startMock(t);
  const { key } = await connect(mock);
  mock.control.latency(150);
  const started = Date.now();
  assert.equal((await call(mock, 'GET', '/api/server/ping')).status, 200);
  assert.ok(Date.now() - started >= 140);
  assert.equal(
    (await call(mock, 'POST', '/__mock/latency', { body: { ms: 0 } })).body.latencyMs,
    0,
  );
  const fast = Date.now();
  assert.equal((await call(mock, 'GET', '/api/users/me', { key })).status, 200);
  assert.ok(Date.now() - fast < 100);
  assert.equal((await call(mock, 'POST', '/__mock/latency', { body: { ms: -1 } })).status, 400);
  assert.equal((await call(mock, 'POST', '/__mock/latency', { body: {} })).status, 400);
});
