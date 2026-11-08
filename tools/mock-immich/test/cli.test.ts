import assert from 'node:assert/strict';
import { type ChildProcess, spawn } from 'node:child_process';
import { join } from 'node:path';
import { type TestContext, test } from 'node:test';
import { scenarioFiles } from '../index.ts';
import { DEFAULT_CLI_PORT, parseCli, UsageError } from '../src/cli.ts';
import { hasOpenssl } from '../src/tls.ts';
import { PACKAGE_ROOT, SCRATCH_ROOT, scratchDir, writeTree } from './helpers.ts';

const SERVER = join(PACKAGE_ROOT, 'server.ts');

interface Exit {
  code: number | null;
  signal: NodeJS.Signals | null;
}

interface Launched {
  child: ChildProcess;
  output: { stdout: string; stderr: string };
  exited: Promise<Exit>;
}

function launch(t: TestContext, args: string[]): Launched {
  const child = spawn(process.execPath, [SERVER, ...args], {
    cwd: PACKAGE_ROOT,
    env: { ...process.env, MOCK_IMMICH_SCRATCH_DIR: SCRATCH_ROOT },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const output = { stdout: '', stderr: '' };
  child.stdout?.setEncoding('utf8').on('data', (chunk: string) => {
    output.stdout += chunk;
  });
  child.stderr?.setEncoding('utf8').on('data', (chunk: string) => {
    output.stderr += chunk;
  });
  const exited = new Promise<Exit>((resolve) => {
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  t.after(() => {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGKILL');
    }
  });
  return { child, output, exited };
}

const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** The JSON line the server prints on stdout once it listens. */
async function ready(run: Launched): Promise<{ listening: string; tls: boolean }> {
  const deadline = Date.now() + 30_000;
  while (!run.output.stdout.includes('\n')) {
    if (run.child.exitCode !== null) {
      throw new Error(`the server exited before it was ready:\n${run.output.stderr}`);
    }
    if (Date.now() > deadline) {
      throw new Error(`the server was not ready in time:\n${run.output.stderr}`);
    }
    await pause(20);
  }
  return JSON.parse(run.output.stdout.split('\n')[0] ?? '');
}

test('the command line has defaults, and --mount builds the object startMockImmich takes', () => {
  assert.deepEqual(parseCli([]), {
    help: false,
    options: { port: DEFAULT_CLI_PORT },
    scenarioArgument: undefined,
  });
  assert.equal(DEFAULT_CLI_PORT, 12283);

  const cli = parseCli([
    '--port',
    '0',
    '--host',
    '127.0.0.1',
    '--scenario',
    'tools/mock-immich/scenarios/fixtures.json',
    '--mount',
    '/mnt/photos=./fixtures/out/volumes/photos',
    '--mount',
    '/mnt/archive=./fixtures/out/volumes/archive',
    '--server-version',
    '2.5.1',
    '--scan-delay-ms',
    '0',
    '--tls',
  ]);
  assert.deepEqual(cli.options, {
    port: 0,
    host: '127.0.0.1',
    scenario: 'tools/mock-immich/scenarios/fixtures.json',
    mounts: {
      '/mnt/photos': './fixtures/out/volumes/photos',
      '/mnt/archive': './fixtures/out/volumes/archive',
    },
    serverVersion: '2.5.1',
    scanDelayMs: 0,
    tls: true,
  });
  assert.equal(cli.scenarioArgument, 'tools/mock-immich/scenarios/fixtures.json');
  assert.equal(parseCli(['-h']).help, true);
  assert.equal(parseCli(['--help']).help, true);
  assert.deepEqual(parseCli(['--mount', '/mnt/a=/data/with=equals']).options.mounts, {
    '/mnt/a': '/data/with=equals',
  });
});

test('a command line that is wrong is a usage error that says why', () => {
  const refused = (argv: string[], message: RegExp) =>
    assert.throws(
      () => parseCli(argv),
      (error: unknown) => error instanceof UsageError && message.test(error.message),
      argv.join(' '),
    );
  refused(['--port', 'abc'], /--port must be an integer from 0 to 65535/);
  refused(['--port', '70000'], /--port must be an integer from 0 to 65535/);
  refused(['--port', '-1'], /--port/);
  refused(['--scan-delay-ms', 'soon'], /--scan-delay-ms must be an integer/);
  refused(['--mount', 'nodirectory'], /--mount expects PREFIX=DIR/);
  refused(['--mount', '=dir'], /--mount expects PREFIX=DIR/);
  refused(['--mount', '/mnt/a='], /--mount expects PREFIX=DIR/);
  refused(['--mount', '/mnt/a=/x', '--mount', '/mnt/a=/y'], /--mount \/mnt\/a is given twice/);
  refused(['--colour', 'red'], /colour/);
  refused(['positional'], /positional/i);
  refused(['--port'], /port/);
});

test('the server prints one JSON line when it listens, answers, and stops cleanly on SIGTERM', async (t) => {
  const run = launch(t, ['--port', '0']);
  const info = await ready(run);
  assert.match(info.listening, /^http:\/\/127\.0\.0\.1:\d+$/);
  assert.equal(info.tls, false);

  const pong = await fetch(`${info.listening}/api/server/ping`);
  assert.deepEqual(await pong.json(), { res: 'pong' });

  run.child.kill('SIGTERM');
  assert.deepEqual(await run.exited, { code: 0, signal: null });
  assert.equal(run.output.stdout, `${JSON.stringify(info)}\n`, 'stdout holds that line only');
  assert.match(run.output.stderr, /SIGTERM, stopping/);
  await assert.rejects(fetch(`${info.listening}/api/server/ping`));
});

test('SIGINT stops it cleanly too', async (t) => {
  const run = launch(t, ['--port', '0']);
  await ready(run);
  run.child.kill('SIGINT');
  assert.deepEqual(await run.exited, { code: 0, signal: null });
});

test('a scenario and mounts given on the command line are the ones in use', async (t) => {
  const photos = scratchDir(t, 'cli-photos');
  const archive = scratchDir(t, 'cli-archive');
  writeTree(photos, { 'a/one.jpg': '1', 'a/two.jpg': '2' });
  writeTree(archive, { 'three.jpg': '3' });
  const run = launch(t, [
    '--port',
    '0',
    '--scenario',
    scenarioFiles.fixtures,
    '--mount',
    `/mnt/photos=${photos}`,
    '--mount',
    `/mnt/archive=${archive}`,
    '--scan-delay-ms',
    '0',
  ]);
  const { listening } = await ready(run);
  assert.match(run.output.stderr, /mount \/mnt\/photos ->/);

  const scan = await fetch(`${listening}/__mock/scan`, { method: 'POST' });
  assert.equal(scan.status, 200);
  const state = (await (await fetch(`${listening}/__mock/state`)).json()) as {
    assets: { total: number };
    libraries: Array<{ name: string; assetCount: number }>;
  };
  assert.equal(state.assets.total, 3);
  assert.deepEqual(
    state.libraries.map((library) => [library.name, library.assetCount]),
    [
      ['Photos', 2],
      ['Archive', 1],
    ],
  );
  run.child.kill('SIGTERM');
  await run.exited;
});

test('--tls prints an https address', {
  skip: hasOpenssl() ? false : 'the openssl command-line tool is not installed',
}, async (t) => {
  const run = launch(t, ['--port', '0', '--tls']);
  const info = await ready(run);
  assert.match(info.listening, /^https:\/\/127\.0\.0\.1:\d+$/);
  assert.equal(info.tls, true);
  assert.match(run.output.stderr, /self-signed certificate/);
  run.child.kill('SIGTERM');
  assert.deepEqual(await run.exited, { code: 0, signal: null });
});

test('--help prints the usage on stdout and exits 0', async (t) => {
  const run = launch(t, ['--help']);
  assert.deepEqual(await run.exited, { code: 0, signal: null });
  assert.match(run.output.stdout, /^Usage: node tools\/mock-immich\/server\.ts/);
  for (const option of ['--port', '--host', '--scenario', '--mount', '--server-version', '--tls']) {
    assert.ok(run.output.stdout.includes(option), option);
  }
});

test('a usage error goes to stderr with the usage and exits 2, leaving stdout empty', async (t) => {
  const run = launch(t, ['--port', 'abc']);
  assert.deepEqual(await run.exited, { code: 2, signal: null });
  assert.equal(run.output.stdout, '');
  assert.match(run.output.stderr, /mock-immich: --port must be an integer/);
  assert.match(run.output.stderr, /Usage: node tools\/mock-immich\/server\.ts/);
});

test('a start that fails exits 1 and says what went wrong', async (t) => {
  const missing = launch(t, ['--port', '0', '--scenario', join(SCRATCH_ROOT, 'nothing.json')]);
  assert.deepEqual(await missing.exited, { code: 1, signal: null });
  assert.equal(missing.output.stdout, '');
  assert.match(missing.output.stderr, /invalid scenario .*cannot read the file/s);

  const mount = launch(t, ['--port', '0', '--mount', `/mnt/x=${join(SCRATCH_ROOT, 'absent')}`]);
  assert.deepEqual(await mount.exited, { code: 1, signal: null });
  assert.match(mount.output.stderr, /mount \/mnt\/x: directory .* does not exist/);
});

test('a port that is taken is reported with the way out', async (t) => {
  const first = launch(t, ['--port', '0']);
  const { listening } = await ready(first);
  const second = launch(t, ['--port', new URL(listening).port]);
  assert.deepEqual(await second.exited, { code: 1, signal: null });
  assert.match(second.output.stderr, /EADDRINUSE|address already in use/);
  assert.match(second.output.stderr, /--port 0 picks a free port/);
  first.child.kill('SIGTERM');
  await first.exited;
});

test('listening beyond the loopback interface is warned about', async (t) => {
  const run = launch(t, ['--port', '0', '--host', '127.0.0.2']);
  await Promise.race([ready(run).catch(() => undefined), run.exited]);
  assert.match(run.output.stderr, /warning: listening on 127\.0\.0\.2/);
  if (run.child.exitCode === null) {
    run.child.kill('SIGTERM');
    await run.exited;
  }
});
