import assert from 'node:assert/strict';
import { getEventListeners, once } from 'node:events';
import { Agent, createServer, type RequestListener } from 'node:http';
import { test } from 'node:test';
import { rawRequest } from './http.ts';

async function fixture(handler: RequestListener) {
  const server = createServer(handler);
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  assert(address && typeof address !== 'string');
  return {
    server,
    url: `http://127.0.0.1:${address.port}`,
    async close() {
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
    },
  };
}

test('rawRequest queues 200 requests under the agent socket cap and releases sockets', async () => {
  let requests = 0;
  const received = new Set<number>();
  let activeSockets = 0;
  let maximumSockets = 0;
  const sockets = new Set<import('node:net').Socket>();
  const app = await fixture((request, response) => {
    assert.equal(request.headers['x-opm-request'], '1');
    let body = '';
    request.setEncoding('utf8');
    request.on('data', (chunk) => {
      body += chunk;
    });
    request.on('end', () => {
      received.add(JSON.parse(body).index);
      requests++;
      setImmediate(() => response.end(JSON.stringify({ ok: true })));
    });
  });
  app.server.on('connection', (socket) => {
    sockets.add(socket);
    maximumSockets = Math.max(maximumSockets, ++activeSockets);
    socket.once('close', () => {
      sockets.delete(socket);
      activeSockets--;
    });
  });
  const agent = new Agent({ keepAlive: true, maxSockets: 16 });
  try {
    const outcomes = await Promise.allSettled(
      Array.from({ length: 200 }, (_, index) =>
        rawRequest({
          url: app.url,
          path: '/burst',
          method: 'POST',
          headers: { Host: new URL(app.url).host, 'x-opm-request': '1' },
          body: JSON.stringify({ index }),
          agent,
          signal: AbortSignal.timeout(15_000),
        }),
      ),
    );
    assert.equal(outcomes.filter((outcome) => outcome.status === 'fulfilled').length, 200);
    for (const outcome of outcomes) {
      assert.equal(outcome.status, 'fulfilled');
      if (outcome.status === 'fulfilled') {
        assert.equal(outcome.value.status, 200);
        assert.deepEqual(outcome.value.json(), { ok: true });
      }
    }
    assert.equal(requests, 200);
    assert.equal(received.size, 200);
    assert(maximumSockets > 0 && maximumSockets <= 16);
    const closed = Promise.all([...sockets].map((socket) => once(socket, 'close')));
    agent.destroy();
    await closed;
    assert.equal(sockets.size, 0);
    assert.equal(Object.keys(agent.requests).length, 0);
  } finally {
    agent.destroy();
    await app.close();
  }
});

test('rawRequest preserves HTTP failures, JSON parse failures and socket errors', async () => {
  const app = await fixture((request, response) => {
    if (request.url === '/reset') {
      request.socket.destroy();
      return;
    }
    response.statusCode = 503;
    response.end('not json');
  });
  try {
    const response = await rawRequest({ url: app.url, path: '/status' });
    assert.equal(response.status, 503);
    assert.equal(response.text, 'not json');
    assert.throws(() => response.json(), SyntaxError);
    await assert.rejects(rawRequest({ url: app.url, path: '/reset' }), { code: 'ECONNRESET' });
  } finally {
    await app.close();
  }
});

test('rawRequest absolute deadline aborts a queued request before socket assignment', async () => {
  let arrived = 0;
  let release: (() => void) | undefined;
  const app = await fixture((_request, response) => {
    arrived++;
    release = () => response.end('{}');
  });
  const agent = new Agent({ keepAlive: true, maxSockets: 1 });
  const first = rawRequest({ url: app.url, path: '/held', agent });
  try {
    await once(app.server, 'request');
    const controller = new AbortController();
    const { signal } = controller;
    const queued = rawRequest({
      url: app.url,
      path: '/queued',
      agent,
      signal,
    });
    let rejected = false;
    const rejection = assert.rejects(queued, (error: unknown) => {
      rejected = true;
      return (
        error instanceof Error &&
        error.name === 'AbortError' &&
        'code' in error &&
        error.code === 'ABORT_ERR' &&
        error.cause === signal.reason
      );
    });
    controller.abort(new Error('Queued deadline'));
    await new Promise<void>((resolve) => setImmediate(resolve));
    assert.equal(rejected, true, 'Queued request rejects while the blocker is still held');
    await rejection;
    assert.equal(arrived, 1);
    assert.equal(getEventListeners(signal, 'abort').length, 0);
    release?.();
    await first;
    assert.equal(arrived, 1);
  } finally {
    release?.();
    await first;
    agent.destroy();
    await app.close();
  }
});

test('rawRequest absolute deadline aborts an active response and closes its socket', async () => {
  const app = await fixture((_request, response) => {
    response.writeHead(200);
    response.write('{');
  });

  try {
    await assert.rejects(
      rawRequest({ url: app.url, path: '/held', signal: AbortSignal.timeout(50) }),
      { code: 'ABORT_ERR' },
    );
  } finally {
    await app.close();
  }
});

test('rawRequest removes signal listeners on success, socket failure and pre-abort', async () => {
  let arrived = 0;
  const app = await fixture((request, response) => {
    arrived++;
    if (request.url === '/reset') request.socket.destroy();
    else response.end('{}');
  });
  try {
    const success = new AbortController();
    await rawRequest({ url: app.url, path: '/', signal: success.signal });
    assert.equal(getEventListeners(success.signal, 'abort').length, 0);
    const reset = new AbortController();
    await assert.rejects(rawRequest({ url: app.url, path: '/reset', signal: reset.signal }), {
      code: 'ECONNRESET',
    });
    assert.equal(getEventListeners(reset.signal, 'abort').length, 0);
    const aborted = new AbortController();
    const reason = new Error('Already expired');
    aborted.abort(reason);
    await assert.rejects(
      rawRequest({ url: app.url, path: '/never', signal: aborted.signal }),
      (error: unknown) => error instanceof Error && error.cause === reason,
    );
    assert.equal(getEventListeners(aborted.signal, 'abort').length, 0);
    assert.equal(arrived, 2);
  } finally {
    await app.close();
  }
});
