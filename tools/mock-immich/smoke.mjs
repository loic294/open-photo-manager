// Contract smoke: the real apm-server, built from this checkout, driven against this mock Immich.
//
//   make mock-immich-smoke
//   node tools/mock-immich/smoke.mjs
//
// Both run `cargo build -p apm-server` first (a no-op when the backend is current) and use the
// debug binary it produces.
//
// It copies the fixture library (`make fixtures` -> fixtures/out/volumes, volumes `photos` and
// `archive`) into a scratch directory, or writes a few tiny files with those names when it is not
// there. The mock runs the `fixtures` scenario with /mnt/photos and /mnt/archive mapped to the
// copies; the backend gets the copies as its volumes. Nothing needs Docker, and the ports are
// picked by the operating system. The scratch directory and both processes are removed even when a
// step fails; a failure prints the mock's request log and the output of the processes.
//
// Environment: APM_SERVER_BIN (use that backend binary as it is, without building),
// MOCK_IMMICH_SCRATCH_DIR (where the scratch directory goes, default tools/mock-immich/.scratch).

import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import {
  accessSync,
  constants,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { scenarioFiles, testUsers } from './index.ts';

const PACKAGE_ROOT = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(PACKAGE_ROOT, '..', '..');
const SCRATCH_ROOT = resolve(process.env.MOCK_IMMICH_SCRATCH_DIR ?? join(PACKAGE_ROOT, '.scratch'));
const FIXTURE_VOLUMES = join(REPO_ROOT, 'fixtures', 'out', 'volumes');

/** The volumes of the fixture library, with the path Immich sees each one at and its library. */
const VOLUMES = [
  { name: 'photos', mount: '/mnt/photos', library: 'Photos' },
  { name: 'archive', mount: '/mnt/archive', library: 'Archive' },
];

/** The ports other agents and the dev servers use: never bind them, even by accident. */
const NEVER_BIND = new Set([8080, 18080, 15173]);
const KEPT_OUTPUT_LINES = 80;

/** A 1x1 JPEG, for the tree that is generated when the fixture library has not been built. */
const TINY_JPEG = Buffer.from(
  '/9j/4AAQSkZJRgABAQEASABIAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////wgALCAABAAEBAREA/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPxA=',
  'base64',
);

const processes = [];
let scratch = '';
let backendUrl = '';
let mockUrl = '';
let stepCount = 0;
let currentStep = '';
let cleanedUp;

// ---------------------------------------------------------------------------------------------
// Processes

/** A child process whose output is kept (the last lines of it, and stdout whole for ready lines). */
function launch(name, command, args, options) {
  const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'], ...options });
  const record = {
    name,
    child,
    stdout: '',
    lines: [],
    exited: new Promise((resolveExit) => {
      child.once('exit', (code, signal) => resolveExit({ code, signal }));
    }),
  };
  const keep = (chunk) => {
    for (const line of String(chunk).split('\n')) {
      if (line.trim() !== '') {
        record.lines.push(line);
      }
    }
    record.lines.splice(0, Math.max(0, record.lines.length - KEPT_OUTPUT_LINES));
  };
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (chunk) => {
    record.stdout += chunk;
    keep(chunk);
  });
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', keep);
  processes.push(record);
  return record;
}

const isRunning = ({ child }) => child.exitCode === null && child.signalCode === null;

async function stop(record) {
  if (!isRunning(record)) {
    return;
  }
  record.child.kill('SIGTERM');
  const timer = setTimeout(() => record.child.kill('SIGKILL'), 8000);
  await record.exited;
  clearTimeout(timer);
}

/** The JSON line the mock prints on stdout once it listens. */
async function readyLine(record, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  while (!record.stdout.includes('\n')) {
    if (!isRunning(record)) {
      throw new Error(`${record.name} exited before it was ready`);
    }
    if (Date.now() > deadline) {
      throw new Error(`${record.name} did not print its ready line within ${timeoutMs} ms`);
    }
    await sleep(25);
  }
  return JSON.parse(record.stdout.split('\n')[0]);
}

async function freePort() {
  for (;;) {
    const port = await new Promise((resolvePort, reject) => {
      const probe = createServer();
      probe.once('error', reject);
      probe.listen(0, '127.0.0.1', () => {
        const { port: chosen } = probe.address();
        probe.close(() => resolvePort(chosen));
      });
    });
    if (!NEVER_BIND.has(port)) {
      return port;
    }
  }
}

function startMock(name, extraArgs, env) {
  return launch(
    name,
    process.execPath,
    [
      join(PACKAGE_ROOT, 'server.ts'),
      '--port',
      '0',
      '--scenario',
      scenarioFiles.fixtures,
      '--scan-delay-ms',
      '0',
      ...extraArgs,
    ],
    { cwd: REPO_ROOT, env },
  );
}

/** APM_SERVER_BIN as it is, or the debug binary of this checkout after `cargo build -p apm-server`
 * (a no-op when it is current: a stale binary would be checked against the wrong code). */
function backendBinary(env) {
  if (process.env.APM_SERVER_BIN !== undefined) {
    const binary = resolve(process.env.APM_SERVER_BIN);
    assert.ok(existsSync(binary), `APM_SERVER_BIN does not exist: ${binary}`);
    return binary;
  }
  console.log('building the backend: cargo build -p apm-server');
  const build = spawnSync('cargo', ['build', '-p', 'apm-server'], {
    cwd: REPO_ROOT,
    env,
    stdio: 'inherit',
  });
  assert.equal(build.status, 0, 'cargo build -p apm-server failed');
  const metadata = spawnSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], {
    cwd: REPO_ROOT,
    env,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  const target =
    metadata.status === 0
      ? JSON.parse(metadata.stdout).target_directory
      : join(REPO_ROOT, 'target');
  const binary = join(target, 'debug', 'apm-server');
  assert.ok(existsSync(binary), `the build did not produce ${binary}`);
  return binary;
}

// ---------------------------------------------------------------------------------------------
// HTTP

async function call(base, method, path, body, headers = {}) {
  const response = await fetch(`${base}${path}`, {
    method,
    headers: { ...(body === undefined ? {} : { 'content-type': 'application/json' }), ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(90_000),
  });
  const text = await response.text();
  let json;
  try {
    json = text === '' ? undefined : JSON.parse(text);
  } catch {
    json = undefined;
  }
  return { status: response.status, body: json, text };
}

/** The backend; every request that is not a read carries the header its CSRF guard requires. */
const backend = (method, path, body) =>
  call(backendUrl, method, path, body, method === 'GET' ? {} : { 'x-apm-request': '1' });

const mock = (method, path, body) => call(mockUrl, method, path, body);

function answer(response, what, status = 200) {
  assert.equal(
    response.status,
    status,
    `${what}: expected ${status}, got ${response.status} ${response.text.slice(0, 500)}`,
  );
  return response.body;
}

const connectBody = (account, extra = {}) => ({
  server_url: mockUrl,
  email: account.email,
  password: account.password,
  allow_insecure_tls: false,
  ...extra,
});

async function waitUntilIdle(timeoutMs = 120_000) {
  const deadline = Date.now() + timeoutMs;
  let quiet = 0;
  while (quiet < 3) {
    const tasks = answer(await backend('GET', '/api/tasks'), 'GET /api/tasks');
    const active = tasks.filter((task) => task.state === 'queued' || task.state === 'running');
    quiet = active.length === 0 ? quiet + 1 : 0;
    assert.ok(
      Date.now() < deadline,
      `tasks are still active after ${timeoutMs} ms: ${JSON.stringify(active)}`,
    );
    await sleep(150);
  }
}

// ---------------------------------------------------------------------------------------------
// The library the volumes are made of

/** A JPEG that differs from every other one: a comment segment right after the start marker. */
function tinyJpeg(comment) {
  const text = Buffer.from(comment);
  const length = text.length + 2;
  const segment = Buffer.concat([Buffer.from([0xff, 0xfe, length >> 8, length & 0xff]), text]);
  return Buffer.concat([TINY_JPEG.subarray(0, 2), segment, TINY_JPEG.subarray(2)]);
}

function writeTinyTree(volumes) {
  const files = {
    photos: [
      '2023/Spring/IMG_0001.jpg',
      '2023/Spring/IMG_0002.jpg',
      '2024/Summer/IMG_0003.jpg',
      '2024/Summer/IMG_0004.jpg',
      '@eaDir/IMG_0001.jpg/SYNOPHOTO_THUMB_M.jpg',
      '.hidden/IMG_0005.jpg',
    ],
    archive: ['2010/DSC_0100.jpg', '2010/DSC_0101.jpg'],
  };
  for (const [volume, paths] of Object.entries(files)) {
    for (const path of paths) {
      const file = join(volumes, volume, path);
      mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, tinyJpeg(`${volume}/${path}`));
    }
    writeFileSync(join(volumes, volume, 'notes.txt'), 'not a photo\n');
  }
}

/**
 * The fixture library has symlinks (a relative link would not follow the copy) and entries with
 * mode 000 (they cannot be read, and not removed afterwards): the copy leaves both out.
 */
function copyable(source) {
  try {
    const info = lstatSync(source);
    if (info.isSymbolicLink()) {
      return false;
    }
    accessSync(source, info.isDirectory() ? constants.R_OK | constants.X_OK : constants.R_OK);
    return true;
  } catch {
    return false;
  }
}

/** Copies the fixture volumes, or writes a tiny tree when `make fixtures` has not been run. */
function prepareVolumes(volumes) {
  mkdirSync(volumes, { recursive: true });
  const built = VOLUMES.every(({ name }) => existsSync(join(FIXTURE_VOLUMES, name)));
  if (!built) {
    writeTinyTree(volumes);
    return 'a generated tree: run `make fixtures` to use the fixture library';
  }
  for (const { name } of VOLUMES) {
    cpSync(join(FIXTURE_VOLUMES, name), join(volumes, name), {
      recursive: true,
      filter: copyable,
      preserveTimestamps: true,
    });
  }
  return 'a copy of fixtures/out/volumes';
}

// ---------------------------------------------------------------------------------------------
// Steps

function skipped(reason) {
  return { skipped: reason };
}

async function step(title, run) {
  stepCount += 1;
  currentStep = `${stepCount}. ${title}`;
  const started = Date.now();
  const result = await run();
  const took = `${Date.now() - started} ms`;
  const number = String(stepCount).padStart(2);
  if (result?.skipped !== undefined) {
    console.log(`skip ${number}. ${title}: ${result.skipped}`);
  } else {
    console.log(`ok   ${number}. ${title}${result ? ` (${result})` : ''} [${took}]`);
  }
}

function hasOpenssl() {
  return spawnSync('openssl', ['version'], { stdio: 'ignore' }).status === 0;
}

async function run() {
  mkdirSync(SCRATCH_ROOT, { recursive: true });
  scratch = mkdtempSync(join(SCRATCH_ROOT, 'smoke-'));
  const volumesDir = join(scratch, 'volumes');
  const dataDir = join(scratch, 'data');
  const webDir = join(scratch, 'web');
  const tmpDir = join(scratch, 'tmp');
  for (const dir of [dataDir, webDir, tmpDir]) {
    mkdirSync(dir, { recursive: true });
  }
  const source = prepareVolumes(volumesDir);
  const binary = backendBinary({ ...process.env, TMPDIR: tmpDir });
  console.log(`volumes: ${source}`);
  console.log(`backend: ${binary} (built ${statSync(binary).mtime.toISOString()})`);

  const plainEnv = { ...process.env, MOCK_IMMICH_SCRATCH_DIR: scratch, TMPDIR: tmpDir };
  const mockProcess = startMock(
    'mock',
    VOLUMES.flatMap(({ name, mount }) => ['--mount', `${mount}=${join(volumesDir, name)}`]),
    plainEnv,
  );
  const ready = await readyLine(mockProcess);
  mockUrl = ready.listening;

  const backendPort = await freePort();
  backendUrl = `http://127.0.0.1:${backendPort}`;
  const backendEnv = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => !key.startsWith('APM_')),
  );
  const backendProcess = launch('backend', binary, [], {
    cwd: scratch,
    env: {
      ...backendEnv,
      TMPDIR: tmpDir,
      APM_VOLUMES_DIR: volumesDir,
      APM_DATA_DIR: dataDir,
      APM_WEB_DIR: webDir,
      APM_LISTEN: `127.0.0.1:${backendPort}`,
      APM_LOG: 'info',
    },
  });
  console.log(`mock: ${mockUrl}   backend: ${backendUrl}`);

  const volumeIds = {};
  let indexed = false;
  let libraries = {};

  await step('the backend answers and lists both fixture volumes', async () => {
    const deadline = Date.now() + 60_000;
    for (;;) {
      assert.ok(isRunning(backendProcess), 'the backend exited before it answered');
      assert.ok(Date.now() < deadline, 'the backend did not answer within 60 s');
      try {
        if ((await backend('GET', '/api/health')).status === 200) {
          break;
        }
      } catch {
        // not listening yet
      }
      await sleep(100);
    }
    const volumes = answer(await backend('GET', '/api/volumes'), 'GET /api/volumes');
    for (const { name } of VOLUMES) {
      const volume = volumes.find((candidate) => candidate.name === name);
      assert.ok(volume, `volume ${name} is missing: ${JSON.stringify(volumes)}`);
      assert.equal(volume.online, true, `volume ${name} is offline`);
      volumeIds[name] = volume.id;
    }
    return VOLUMES.map(({ name }) => `${name}=${volumeIds[name]}`).join(', ');
  });

  await step('the index is built', async () => {
    await waitUntilIdle();
    const started = await backend('POST', '/api/index/rescan', {});
    if (started.status === 501) {
      return skipped(
        'POST /api/index/rescan answers 501 (building-index-controls): nothing is indexed yet, so detect-prefix cannot be checked',
      );
    }
    assert.ok(
      started.status >= 200 && started.status < 300,
      `POST /api/index/rescan: ${started.status} ${started.text.slice(0, 400)}`,
    );
    await waitUntilIdle();
    const tasks = answer(await backend('GET', '/api/tasks'), 'GET /api/tasks');
    const broken = tasks.filter((task) => ['failed', 'interrupted'].includes(task.state));
    assert.deepEqual(broken, [], 'an indexing task did not finish');
    indexed = true;
    return `${tasks.length} tasks finished`;
  });

  await step('the mock scans the mounted fixture directories', async () => {
    answer(await mock('POST', '/__mock/scan', {}), 'POST /__mock/scan');
    const snapshot = answer(await mock('GET', '/__mock/state?limit=1'), 'GET /__mock/state');
    libraries = Object.fromEntries(
      snapshot.libraries.map((library) => [
        library.name,
        { id: library.id, count: library.assetCount },
      ]),
    );
    assert.ok(
      libraries.Photos?.count > 0,
      `nothing was found under /mnt/photos: ${JSON.stringify(libraries)}`,
    );
    assert.ok(libraries.Archive, 'the Archive library is missing');
    return VOLUMES.map(({ library }) => `${library} ${libraries[library].count}`).join(', ');
  });

  await step('a wrong password is refused and nothing is stored', async () => {
    const refused = await backend(
      'POST',
      '/api/immich/connect',
      connectBody(testUsers.admin, { password: 'not-the-password' }),
    );
    assert.equal(refused.status, 400, `${refused.status} ${refused.text.slice(0, 400)}`);
    assert.equal(refused.body?.error?.code, 'immich_auth_failed', refused.text.slice(0, 400));
    const status = answer(await backend('GET', '/api/immich/status'), 'GET /api/immich/status');
    assert.equal(status.connected, false);
    assert.equal(status.mode, 'disconnected');
    return refused.body.error.code;
  });

  await step('the regular account connects in limited mode', async () => {
    const connection = answer(
      await backend('POST', '/api/immich/connect', connectBody(testUsers.user)),
      'connect as the regular account',
    );
    assert.equal(connection.is_admin, false);
    assert.equal(connection.user_name, testUsers.user.name);
    const status = answer(await backend('GET', '/api/immich/status'), 'GET /api/immich/status');
    assert.equal(status.mode, 'limited');
    const listed = answer(
      await backend('GET', '/api/immich/libraries'),
      'GET /api/immich/libraries',
    );
    assert.deepEqual(listed, { libraries: [], limited: true });
    return `${connection.user_name}, no libraries`;
  });

  await step('the administrator connects in full mode', async () => {
    const connection = answer(
      await backend('POST', '/api/immich/connect', connectBody(testUsers.admin)),
      'connect as the administrator',
    );
    assert.equal(connection.is_admin, true);
    assert.equal(connection.email, testUsers.admin.email);
    assert.equal(connection.server_url, mockUrl);
    const status = answer(await backend('GET', '/api/immich/status'), 'GET /api/immich/status');
    assert.equal(status.connected, true);
    assert.equal(status.mode, 'full');
    assert.equal(status.needs_reconnect, false);
    return `${connection.user_name}, ${status.server_version}`;
  });

  await step('the libraries are listed with their asset counts', async () => {
    const listed = answer(
      await backend('GET', '/api/immich/libraries'),
      'GET /api/immich/libraries',
    );
    assert.equal(listed.limited, false);
    for (const { library, mount } of VOLUMES) {
      const found = listed.libraries.find((candidate) => candidate.name === library);
      assert.ok(found, `library ${library} is missing: ${JSON.stringify(listed)}`);
      assert.equal(found.id, libraries[library].id);
      assert.deepEqual(found.import_paths, [mount]);
      assert.equal(found.asset_count, libraries[library].count);
    }
    const photos = listed.libraries.find((candidate) => candidate.name === 'Photos');
    assert.deepEqual(photos.exclusion_patterns, ['**/@eaDir/**']);
    return listed.libraries.map((library) => `${library.name} ${library.asset_count}`).join(', ');
  });

  await step('the volumes are mapped to the libraries', async () => {
    const mappings = VOLUMES.map(({ name, mount, library }) => ({
      volume_id: volumeIds[name],
      library_id: libraries[library].id,
      path_prefix: mount,
    }));
    const stored = answer(
      await backend('PUT', '/api/immich/mappings', { mappings }),
      'PUT /api/immich/mappings',
    );
    assert.deepEqual(
      stored.mappings.map(({ volume_name, library_name, path_prefix }) => [
        volume_name,
        library_name,
        path_prefix,
      ]),
      VOLUMES.map(({ name, library, mount }) => [name, library, mount]).sort(([a], [b]) =>
        a.localeCompare(b),
      ),
    );
    const status = answer(await backend('GET', '/api/immich/status'), 'GET /api/immich/status');
    assert.deepEqual(status.mappings, stored.mappings);
    return stored.mappings
      .map((mapping) => `${mapping.volume_name} -> ${mapping.path_prefix}`)
      .join(', ');
  });

  await step('detect-prefix finds the mount of each volume', async () => {
    if (!indexed) {
      return skipped('the index is empty (see the index step), so there are no files to look up');
    }
    const found = [];
    for (const { name, mount, library } of VOLUMES) {
      const detected = answer(
        await backend('POST', '/api/immich/detect-prefix', {
          volume_id: volumeIds[name],
          library_id: libraries[library].id,
        }),
        `detect-prefix for ${name}`,
      );
      if (libraries[library].count === 0) {
        continue;
      }
      const best = detected.suggestions[0];
      assert.equal(best?.prefix, mount, `${name}: ${JSON.stringify(detected)}`);
      assert.ok(best.matched > 0 && best.matched <= best.sampled, JSON.stringify(best));
      found.push(`${name} ${best.prefix} (${best.matched}/${best.sampled})`);
    }
    return found.join(', ');
  });

  await step('exclude-trash adds the pattern to every library, once', async () => {
    const first = answer(await backend('POST', '/api/immich/exclude-trash', {}), 'exclude-trash');
    assert.deepEqual(first.failed, []);
    assert.deepEqual(first.updated.map((library) => library.name).sort(), ['Archive', 'Photos']);
    assert.deepEqual(first.already_excluded, []);
    const snapshot = answer(await mock('GET', '/__mock/state?limit=1'), 'GET /__mock/state');
    for (const library of snapshot.libraries) {
      assert.ok(
        library.exclusionPatterns.includes(first.pattern),
        `${library.name} does not exclude ${first.pattern}: ${JSON.stringify(library.exclusionPatterns)}`,
      );
    }
    const photos = snapshot.libraries.find((library) => library.name === 'Photos');
    assert.ok(photos.exclusionPatterns.includes('**/@eaDir/**'), 'the existing pattern was lost');
    const again = answer(
      await backend('POST', '/api/immich/exclude-trash', {}),
      'exclude-trash again',
    );
    assert.deepEqual(again.updated, []);
    assert.deepEqual(again.already_excluded.map((library) => library.name).sort(), [
      'Archive',
      'Photos',
    ]);
    return first.pattern;
  });

  await step('the status check reaches Immich', async () => {
    const status = answer(
      await backend('GET', '/api/immich/status?check=true'),
      'GET /api/immich/status?check=true',
    );
    assert.equal(status.connected, true);
    assert.equal(status.mode, 'full');
    assert.equal(status.reachable, true);
    assert.equal(status.needs_reconnect, false);
    assert.equal(status.connection.last_error, null);
    assert.equal(status.version_warning, null);
    assert.equal(status.mappings.length, VOLUMES.length);
    return `${status.server_version}, ${status.mappings.length} mappings`;
  });

  await step('the asset sync matches the scanned assets', async () => {
    const started = await backend('POST', '/api/immich/sync', {});
    if (started.status === 501) {
      return skipped('POST /api/immich/sync answers 501 (syncing-immich-assets)');
    }
    // EXTENSION POINT (syncing-immich-assets): once the sync lands, assert here the matched counts
    // that GET /api/immich/status reports after the run, against the scan of the step above.
    assert.ok(
      started.status >= 200 && started.status < 300,
      `POST /api/immich/sync: ${started.status} ${started.text.slice(0, 400)}`,
    );
    await waitUntilIdle();
    const status = answer(await backend('GET', '/api/immich/status'), 'GET /api/immich/status');
    assert.equal(typeof status.last_sync_at, 'number', 'the sync left no last_sync_at');
    return `last_sync_at ${status.last_sync_at}`;
  });

  // EXTENSION POINT (reflecting-file-ops-in-immich): the outbox flows go here, for example move a
  // file through the backend, POST /__mock/scan, and assert the asset is offline then replaced.

  await step('a revoked key makes the status ask for a new connection', async () => {
    const revoked = answer(await mock('POST', '/__mock/revoke-key', {}), 'POST /__mock/revoke-key');
    assert.ok(revoked.revoked >= 1, JSON.stringify(revoked));
    const status = answer(
      await backend('GET', '/api/immich/status?check=true'),
      'GET /api/immich/status?check=true',
    );
    assert.equal(status.needs_reconnect, true, JSON.stringify(status));
    return `revoked: ${revoked.revoked}`;
  });

  await step('connecting again repairs it', async () => {
    answer(await backend('POST', '/api/immich/connect', connectBody(testUsers.admin)), 'reconnect');
    const status = answer(
      await backend('GET', '/api/immich/status?check=true'),
      'GET /api/immich/status?check=true',
    );
    assert.equal(status.needs_reconnect, false);
    assert.equal(status.reachable, true);
    assert.equal(status.mode, 'full');
    assert.equal(status.mappings.length, VOLUMES.length, 'the mappings were lost');
  });

  await step('a self-signed certificate is refused, then accepted when allowed', async () => {
    if (!hasOpenssl()) {
      return skipped(
        'the openssl command-line tool is not installed, so the mock cannot serve TLS',
      );
    }
    const tlsProcess = startMock('mock-tls', ['--tls'], plainEnv);
    const tlsReady = await readyLine(tlsProcess);
    assert.equal(tlsReady.tls, true);
    assert.match(tlsReady.listening, /^https:\/\/127\.0\.0\.1:\d+$/);
    const refused = await backend(
      'POST',
      '/api/immich/connect',
      connectBody(testUsers.admin, { server_url: tlsReady.listening }),
    );
    assert.equal(refused.status, 502, `${refused.status} ${refused.text.slice(0, 400)}`);
    assert.equal(refused.body?.error?.code, 'immich_unreachable', refused.text.slice(0, 400));
    const accepted = answer(
      await backend(
        'POST',
        '/api/immich/connect',
        connectBody(testUsers.admin, { server_url: tlsReady.listening, allow_insecure_tls: true }),
      ),
      'connect with allow_insecure_tls',
    );
    assert.equal(accepted.allow_insecure_tls, true);
    assert.equal(accepted.is_admin, true);
    const status = answer(
      await backend('GET', '/api/immich/status?check=true'),
      'GET /api/immich/status?check=true',
    );
    assert.equal(status.reachable, true);
    await stop(tlsProcess);
    return `refused with ${refused.body.error.code}`;
  });

  await step('disconnecting revokes the key in Immich', async () => {
    const keysBefore = new Set(
      answer(await mock('GET', '/__mock/state?limit=1'), 'GET /__mock/state').keys.map(
        (key) => key.id,
      ),
    );
    answer(await backend('POST', '/api/immich/connect', connectBody(testUsers.admin)), 'reconnect');
    const created = answer(
      await mock('GET', '/__mock/state?limit=1'),
      'GET /__mock/state',
    ).keys.filter((key) => !keysBefore.has(key.id));
    assert.equal(created.length, 1, `connecting must create one key: ${JSON.stringify(created)}`);
    const result = answer(
      await backend('DELETE', '/api/immich/connection'),
      'DELETE /api/immich/connection',
    );
    assert.equal(result.revoked, true, JSON.stringify(result));
    const after = answer(await mock('GET', '/__mock/state?limit=1'), 'GET /__mock/state').keys;
    assert.ok(
      !after.some((key) => key.id === created[0].id),
      `the key ${created[0].name} is still in the mock: ${JSON.stringify(after)}`,
    );
    const status = answer(await backend('GET', '/api/immich/status'), 'GET /api/immich/status');
    assert.equal(status.connected, false);
    assert.equal(status.mode, 'disconnected');
    return `key "${created[0].name}" is gone`;
  });

  await step('the backend only made calls the mock models', async () => {
    const { entries } = answer(await mock('GET', '/__mock/log'), 'GET /__mock/log');
    const unexpected = entries.filter((entry) => entry.unexpected);
    assert.deepEqual(
      unexpected.map((entry) => `${entry.method} ${entry.path}`),
      [],
      'the backend called routes the mock does not have',
    );
    const violations = answer(
      await mock('GET', '/__mock/state?limit=1'),
      'GET /__mock/state',
    ).violations;
    assert.deepEqual(violations, [], 'a forced delete of an online asset was recorded');
    return `${entries.length} requests, none unexpected, no violations`;
  });
}

// ---------------------------------------------------------------------------------------------
// Reporting and clean-up

async function printFailure(error) {
  console.error(`\nFAIL step ${currentStep || '(setup)'}`);
  console.error(error instanceof Error ? (error.stack ?? error.message) : error);
  if (mockUrl !== '') {
    try {
      const { entries } = (await call(mockUrl, 'GET', '/__mock/log')).body;
      console.error(`\n--- request log of the mock (${entries.length} requests) ---`);
      for (const entry of entries) {
        const flags = [
          entry.auth,
          entry.keyName,
          entry.unexpected ? 'UNEXPECTED' : '',
          entry.injected,
        ]
          .filter(Boolean)
          .join(' ');
        const summary =
          entry.bodySummary === undefined ? '' : ` ${JSON.stringify(entry.bodySummary)}`;
        console.error(
          `#${entry.index} ${entry.method} ${entry.path} -> ${entry.status} [${flags}]${summary}`,
        );
      }
    } catch (logError) {
      console.error(`\n(the request log of the mock is not available: ${logError.message})`);
    }
  }
  for (const record of processes) {
    console.error(`\n--- last output of ${record.name} ---`);
    console.error(record.lines.join('\n'));
  }
}

function cleanUp() {
  cleanedUp ??= (async () => {
    await Promise.all(processes.map(stop));
    if (scratch !== '') {
      rmSync(scratch, { recursive: true, force: true });
    }
  })();
  return cleanedUp;
}

for (const [signal, code] of [
  ['SIGINT', 130],
  ['SIGTERM', 143],
]) {
  process.on(signal, () => {
    cleanUp().finally(() => process.exit(code));
  });
}

let exitCode = 0;
try {
  await run();
  console.log(`\nall ${stepCount} steps passed`);
} catch (error) {
  exitCode = 1;
  await printFailure(error);
} finally {
  await cleanUp();
}
process.exit(exitCode);
