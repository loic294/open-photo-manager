import assert from 'node:assert/strict';
import { readdirSync } from 'node:fs';
import { request } from 'node:https';
import { test } from 'node:test';
import { startMockImmich } from '../index.ts';
import { hasOpenssl } from '../src/tls.ts';
import { ADMIN, scratchDir } from './helpers.ts';

const missingOpenssl = hasOpenssl() ? false : 'the openssl command-line tool is not installed';

interface Answer {
  status: number;
  body: unknown;
}

/** The global fetch would refuse the self-signed certificate, so this trusts it explicitly. */
function send(
  url: string,
  options: { ca?: string; method?: string; body?: unknown } = {},
): Promise<Answer> {
  return new Promise((resolve, reject) => {
    const payload = options.body === undefined ? undefined : JSON.stringify(options.body);
    const req = request(
      url,
      {
        method: options.method ?? 'GET',
        ca: options.ca,
        headers: payload === undefined ? {} : { 'content-type': 'application/json' },
      },
      (res) => {
        let text = '';
        res.setEncoding('utf8');
        res.on('data', (chunk: string) => {
          text += chunk;
        });
        res.on('end', () => resolve({ status: res.statusCode ?? 0, body: JSON.parse(text) }));
      },
    );
    req.on('error', reject);
    req.end(payload);
  });
}

test('a tls mock serves https with a certificate the client can trust', {
  skip: missingOpenssl,
}, async (t) => {
  const mock = await startMockImmich({ tls: true, scanDelayMs: 0 });
  t.after(() => mock.stop());
  assert.equal(mock.tls, true);
  assert.match(mock.url, /^https:\/\/127\.0\.0\.1:\d+$/);
  assert.match(mock.certificate ?? '', /^-----BEGIN CERTIFICATE-----/);

  const trusted = await send(`${mock.url}/api/server/ping`, { ca: mock.certificate });
  assert.deepEqual(trusted, { status: 200, body: { res: 'pong' } });

  const login = await send(`${mock.url}/api/auth/login`, {
    ca: mock.certificate,
    method: 'POST',
    body: ADMIN,
  });
  assert.equal(login.status, 201);

  await assert.rejects(
    send(`${mock.url}/api/server/ping`),
    /self[- ]signed|unable to verify|certificate/i,
    'a client that does not trust the certificate is refused',
  );
});

test('the certificate and its key are removed when the mock stops', {
  skip: missingOpenssl,
}, async (t) => {
  const dir = scratchDir(t, 'tls');
  const previous = process.env.MOCK_IMMICH_SCRATCH_DIR;
  process.env.MOCK_IMMICH_SCRATCH_DIR = dir;
  let mock: Awaited<ReturnType<typeof startMockImmich>>;
  try {
    mock = await startMockImmich({ tls: true });
  } finally {
    process.env.MOCK_IMMICH_SCRATCH_DIR = previous;
  }
  const during = readdirSync(dir);
  assert.equal(during.length, 1);
  assert.match(during[0] ?? '', /^mock-immich-tls-/);
  await mock.stop();
  assert.deepEqual(readdirSync(dir), []);
});

test('a mock without tls has no certificate', async (t) => {
  const mock = await startMockImmich({ scanDelayMs: 0 });
  t.after(() => mock.stop());
  assert.equal(mock.tls, false);
  assert.equal(mock.certificate, undefined);
});

test('tls without the openssl command is a clear startup error', async (t) => {
  const empty = scratchDir(t, 'no-openssl');
  const previous = process.env.PATH;
  process.env.PATH = empty;
  try {
    await assert.rejects(
      startMockImmich({ tls: true }),
      /--tls needs the openssl command-line tool, which was not found in PATH/,
    );
  } finally {
    process.env.PATH = previous;
  }
});
