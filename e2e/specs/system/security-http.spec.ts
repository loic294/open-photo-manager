import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { rawRequest } from '../../src/http';
import { root } from '../../src/paths';
import { apiRoutes, implementedRoutes } from '../../src/routes';
import { assertReadOnly, captureReadOnly, expect, test } from '../../src/test';

const routes = apiRoutes();
const standardHeaders = {
  'x-content-type-options': 'nosniff',
  'referrer-policy': 'no-referrer',
  'x-frame-options': 'DENY',
  'cross-origin-opener-policy': 'same-origin',
  'cross-origin-resource-policy': 'same-origin',
  'permissions-policy': 'camera=(), microphone=(), geolocation=(), payment=()',
};

test('route inventory matches every landed method router @smoke', async () => {
  expect(implementedRoutes()).toEqual(
    routes.map((route) => `${route.method} ${route.path}`).sort(),
  );
  expect(new Set(routes.map((route) => `${route.method} ${route.path}`)).size).toBe(routes.length);
});

test('all non-GET routes reject missing, ambiguous and cross-site markers', async ({ app }) => {
  test.setTimeout(90_000);
  const before = await captureReadOnly(app);
  const host = new URL(app.url).host;
  for (const route of routes.filter((route) => route.method !== 'GET')) {
    const rejectedHeaders: Record<string, string | string[]>[] = [
      {},
      { 'x-apm-request': '0' },
      { 'x-apm-request': 'true' },
      { 'x-apm-request': ['1', '1'] },
      { 'x-apm-request': '1', Origin: 'https://evil.example' },
      { 'x-apm-request': '1', Origin: 'null' },
      { 'x-apm-request': '1', Origin: 'not an origin' },
      { 'x-apm-request': '1', 'Sec-Fetch-Site': 'cross-site' },
      { 'x-apm-request': '1', 'Sec-Fetch-Site': 'same-site' },
    ];
    for (const headers of rejectedHeaders) {
      const response = await rawRequest({
        url: app.url,
        method: route.method,
        path: route.path,
        headers: { Host: host, 'Content-Type': 'application/json', ...headers },
        body: JSON.stringify(route.body ?? {}),
      });
      expect(response.status, `${route.method} ${route.path} ${JSON.stringify(headers)}`).toBe(403);
      expect(response.json()).toMatchObject({ error: { code: 'csrf_rejected' } });
      expect(
        Object.keys(response.headers).filter((name) => name.startsWith('access-control-')),
      ).toEqual([]);
    }
  }
  await assertReadOnly(app, before);
});

test('same-origin and script requests reach validation; preflight has no CORS @smoke', async ({
  app,
}) => {
  const host = new URL(app.url).host;
  const settings = await app.api.settings();
  const acceptedHeaders: Record<string, string>[] = [
    {},
    { Origin: app.url },
    { Origin: app.url, 'Sec-Fetch-Site': 'same-origin' },
    { 'Sec-Fetch-Site': 'none' },
  ];
  for (const headers of acceptedHeaders) {
    const response = await rawRequest({
      url: app.url,
      method: 'PUT',
      path: '/api/settings',
      headers: { Host: host, 'x-apm-request': '1', 'Content-Type': 'application/json', ...headers },
      body: JSON.stringify(settings),
    });
    expect(response.status).toBe(200);
    expect(response.json()).toEqual(settings);
  }
  for (const route of routes) {
    const response = await rawRequest({
      url: app.url,
      method: 'OPTIONS',
      path: route.path,
      headers: {
        Host: host,
        Origin: 'https://evil.example',
        'Access-Control-Request-Method': route.method,
      },
    });
    expect(response.status, route.path).toBe(405);
    expect(
      Object.keys(response.headers).filter((name) => name.startsWith('access-control-')),
    ).toEqual([]);
  }
});

test('every GET leaves indexed counts and volume content unchanged', async ({ app }) => {
  test.setTimeout(90_000);
  const before = await captureReadOnly(app);
  const counts = () =>
    ['files', 'dirs', 'trash_items', 'selections', 'plans'].map((table) =>
      app.sql(`SELECT count(*) AS n FROM ${table}`),
    );
  const initial = counts();
  for (const route of routes.filter((route) => route.method === 'GET')) {
    const response = await rawRequest({
      url: app.url,
      path: route.path,
      headersOnly: route.path === '/api/events',
    });
    expect(response.status, route.path).not.toBe(405);
    expect(response.status, route.path).not.toBe(500);
    expect(
      Object.keys(response.headers).filter((name) => name.startsWith('access-control-')),
    ).toEqual([]);
    expect(counts(), route.path).toEqual(initial);
  }
  // On-demand media may fill pipeline/cache fields, but cannot change identities or metadata.
  const after = await captureReadOnly(app);
  expect(after.tree).toEqual(before.tree);
  expect(app.sql('SELECT count(*) AS n FROM trash_items')).toEqual(initial[2]);
});

test('headers, limits, fallbacks, request IDs and log redaction @smoke', async ({ app }) => {
  const index = await readFile(join(root, 'web/dist/index.html'), 'utf8');
  const asset = /(?:src|href)="([^"]*\/assets\/[^"]+\.js)"/.exec(index)?.[1];
  expect(asset, 'production hashed JavaScript asset').toBeTruthy();
  const file = (await app.api.listing('photos', '2021/03')).items[0];
  expect(file).toBeDefined();
  for (const path of [
    '/',
    asset ?? '',
    '/api/health',
    '/api/no-such-route',
    `/api/files/${file.id}/thumb`,
    `/api/files/${file.id}/content`,
  ]) {
    const response = await rawRequest({ url: app.url, path });
    for (const [header, value] of Object.entries(standardHeaders))
      expect(response.headers[header], `${path} ${header}`).toBe(value);
    expect(response.headers['content-security-policy']).toBeTruthy();
    if (path.endsWith('/content'))
      expect(response.headers['content-security-policy']).toBe('sandbox');
    if (path === '/api/health') expect(response.headers['cache-control']).toBe('no-store');
  }
  const request = (method: string, path: string, body?: string, id?: string) =>
    rawRequest({
      url: app.url,
      method,
      path,
      body,
      headers: {
        Host: new URL(app.url).host,
        'x-apm-request': '1',
        'Content-Type': 'application/json',
        'x-request-id': id ?? 'system-e2e',
      },
    });
  const unknown = await request('GET', '/api/x');
  expect(unknown.status).toBe(404);
  expect(unknown.json()).toMatchObject({ error: { code: 'route_not_found' } });
  const wrong = await request('DELETE', '/api/health');
  expect(wrong.status).toBe(405);
  expect(wrong.json()).toMatchObject({ error: { code: 'method_not_allowed' } });
  expect(
    (await request('GET', '/api/health', undefined, 'valid_ID-42')).headers['x-request-id'],
  ).toBe('valid_ID-42');
  expect(
    (await request('GET', '/api/health', undefined, 'invalid id')).headers['x-request-id'],
  ).toMatch(/^[a-zA-Z0-9_-]+$/);
  // A declared oversized body must be refused before the server reads it.
  const oversized = await rawRequest({
    url: app.url,
    method: 'PUT',
    path: '/api/settings',
    headers: {
      Host: new URL(app.url).host,
      'x-apm-request': '1',
      'Content-Type': 'application/json',
      'Content-Length': String(1024 ** 2 + 1),
    },
  });
  expect(oversized.status).toBe(413);
  expect(oversized.json()).toMatchObject({ error: { code: 'payload_too_large' } });
  expect((await request('GET', '/api/health?secret=ZZTOP-4242')).status).toBe(200);
  await request('POST', '/api/selections', JSON.stringify({ secret: 'ZZTOP-4242' }));
  expect(app.logs()).not.toContain('ZZTOP-4242');
  expect((await app.api.request<{ status: string }>('GET', '/api/health')).status).toBe('ok');
});

test('Host rejection covers API, event stream, media and escaped HTML', async ({ app }) => {
  for (const host of ['evil.example.com', 'evil.example.com:80', '', '<script>x</script>']) {
    for (const path of ['/api/health', '/api/events', '/api/files/1/content', '/']) {
      const response = await rawRequest({ url: app.url, path, headers: { Host: host } });
      expect(response.status, `${host} ${path}`).toBe(403);
      if (path.startsWith('/api/'))
        expect(response.json()).toMatchObject({ error: { code: 'host_not_allowed' } });
      else {
        expect(response.text).toContain('APM_ALLOWED_HOSTS');
        expect(response.text).not.toContain('<script>x</script>');
        for (const [header, value] of Object.entries(standardHeaders))
          expect(response.headers[header]).toBe(value);
      }
    }
  }
  for (const headers of [[], ['Host', 'localhost', 'Host', 'evil.example.com']]) {
    const response = await rawRequest({ url: app.url, path: '/api/health', headers });
    // HTTP/1.1 transport may reject duplicate/missing Host before axum sees the request.
    expect([400, 403]).toContain(response.status);
    if (response.status === 403)
      expect(response.json()).toMatchObject({ error: { code: 'host_not_allowed' } });
  }
  for (const host of ['localhost', '127.0.0.1', '[::1]', '192.0.2.42', '[2001:db8::1]']) {
    expect(
      (await rawRequest({ url: app.url, path: '/api/health', headers: { Host: host } })).status,
      host,
    ).toBe(200);
  }
});

test.describe('configured host names', () => {
  test.use({ appOptions: { env: { APM_ALLOWED_HOSTS: 'nas.local, *.example.com' } } });
  test('explicit host and wildcard do not allow the wildcard apex', async ({ app }) => {
    for (const [host, status] of [
      ['nas.local:42', 200],
      ['a.example.com', 200],
      ['example.com', 403],
    ] as const) {
      expect(
        (await rawRequest({ url: app.url, path: '/api/health', headers: { Host: host } })).status,
      ).toBe(status);
    }
  });
});
