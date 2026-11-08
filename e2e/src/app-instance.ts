import { type ChildProcess, spawn } from 'node:child_process';
import { once } from 'node:events';
import { cp, mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { createServer, type Server } from 'node:net';
import { join, relative, resolve, sep } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { expect } from '@playwright/test';
import type { Scope } from '../../web/src/api/generated/Scope';
import type { SettingsDto } from '../../web/src/api/generated/SettingsDto';
import { Api } from './api';
import { fixturePermissions } from './fixture-copy';
import { binary, cache, golden, root } from './paths';
import { waitForTasksIdle } from './waits';

export interface AppOptions {
  env?: Record<string, string>;
  settings?: (settings: SettingsDto) => SettingsDto;
  watcher?: boolean;
  cold?: boolean;
}

export async function reservePort(port = 0): Promise<Server> {
  const server = createServer();
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolve);
  });
  return server;
}

export async function run(command: string, args: string[]) {
  const child = spawn(command, args, { cwd: root, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  child.stdout.on('data', (data) => {
    output += data;
  });
  child.stderr.on('data', (data) => {
    output += data;
  });
  const [code] = await once(child, 'exit');
  if (code !== 0) throw new Error(`${command} exited ${code}\n${output}`);
  return output;
}

export class AppInstance {
  url = '';
  readonly outDir: string;
  readonly volumesDir: string;
  readonly dataDir: string;
  readonly api = new Api(() => this.url);
  private process?: ChildProcess;
  private reservation?: Server;
  private output = '';
  private startupAfter = -1;

  private constructor(
    readonly directory: string,
    private options: AppOptions,
  ) {
    this.outDir = join(directory, 'out');
    this.volumesDir = join(this.outDir, 'volumes');
    this.dataDir = join(directory, 'data');
  }

  static async start(options: AppOptions = {}, seed = golden): Promise<AppInstance> {
    await mkdir(cache, { recursive: true });
    const app = new AppInstance(await mkdtemp(join(cache, 'instance-')), options);
    try {
      await cp(join(seed, 'out'), app.outDir, {
        recursive: true,
        preserveTimestamps: true,
        verbatimSymlinks: true,
      });
      await fixturePermissions(app.outDir, false);
      if (!options.cold)
        await cp(join(seed, 'data'), app.dataDir, {
          recursive: true,
          preserveTimestamps: true,
          verbatimSymlinks: true,
        });
      await mkdir(app.dataDir, { recursive: true });
      await app.resume();
      if (!options.cold) await app.waitForIdle();
      const settings = await app.api.settings();
      let next = {
        ...settings,
        index: { ...settings.index, watch_enabled: options.watcher ?? false },
      };
      if (options.settings) next = options.settings(next);
      await app.api.saveSettings(next);
      if (options.watcher) {
        await expect
          .poll(async () => {
            const status = await app.api.indexStatus();
            if (!('watcher' in status)) throw new Error('Watcher status API has not landed');
            return status.watcher;
          })
          .toBe('watching');
      }
      return app;
    } catch (error) {
      await app.stop();
      throw new Error(`App start failed: ${error}\n${app.logs()}`, { cause: error });
    }
  }

  volumePath(path: string): string {
    const target = resolve(this.volumesDir, path);
    const rel = relative(this.volumesDir, target);
    if (!rel || rel.startsWith(`..${sep}`) || rel === '..' || resolve(target) === this.volumesDir)
      throw new Error(`Unsafe fixture path ${path}`);
    return target;
  }

  async resume() {
    if (this.process) throw new Error('App is already running');
    this.startupAfter = -1;
    try {
      this.startupAfter = Number(
        this.sql('SELECT coalesce(max(id), 0) AS id FROM tasks')[0]?.id ?? 0,
      );
    } catch (error) {
      if (!(error instanceof Error) || !/unable to open database|no such table/.test(error.message))
        throw error;
    }
    this.reservation ??= await reservePort(this.url ? Number(new URL(this.url).port) : 0);
    const address = this.reservation.address();
    if (!address || typeof address === 'string') throw new Error('Expected an IP port');
    this.url = `http://127.0.0.1:${address.port}`;
    await new Promise<void>((resolve, reject) =>
      this.reservation?.close((error) => (error ? reject(error) : resolve())),
    );
    this.reservation = undefined;
    const child = spawn(binary, [], {
      cwd: root,
      env: {
        ...process.env,
        ...this.options.env,
        TZ: 'UTC',
        APM_LOG: 'info',
        APM_LISTEN: `127.0.0.1:${address.port}`,
        APM_DATA_DIR: this.dataDir,
        APM_VOLUMES_DIR: this.volumesDir,
        APM_WEB_DIR: join(root, 'web/dist'),
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    this.process = child;
    child.stdout.on('data', (data) => {
      this.output += data;
    });
    child.stderr.on('data', (data) => {
      this.output += data;
    });
    let spawnError: Error | undefined;
    child.on('error', (error) => {
      spawnError = error;
    });
    await expect
      .poll(
        async () => {
          if (spawnError) throw spawnError;
          if (child.exitCode !== null)
            throw new Error(`Server exited ${child.exitCode}: ${this.logs()}`);
          try {
            return (await fetch(`${this.url}/api/health`, { signal: AbortSignal.timeout(500) })).ok;
          } catch (error) {
            if (
              error instanceof TypeError ||
              (error instanceof Error && error.name === 'TimeoutError')
            )
              return false;
            throw error;
          }
        },
        { timeout: 10_000 },
      )
      .toBe(true);
  }

  async waitForIdle() {
    await waitForTasksIdle(this, this.startupAfter);
  }
  async rescan(targets: Scope[] = []) {
    const task = await this.api.rescan({ targets, force: false, confirm_removals: false });
    await waitForTasksIdle(this, task.task_id - 1);
    return task;
  }

  private async terminate(signal: NodeJS.Signals) {
    const child = this.process;
    if (!child) return;
    if (child.exitCode === null && child.signalCode === null) {
      const exited = once(child, 'exit');
      child.kill(signal);
      const timer = setTimeout(() => child.kill('SIGKILL'), 10_000);
      try {
        await exited;
      } finally {
        clearTimeout(timer);
      }
    }
    this.process = undefined;
  }

  async kill() {
    await this.terminate('SIGKILL');
  }
  async suspend() {
    await this.terminate('SIGTERM');
    await expect(async () => {
      this.reservation = await reservePort(Number(new URL(this.url).port));
    }).toPass({ timeout: 10_000 });
  }
  async restart(options: { newPort?: boolean } = {}) {
    await this.suspend();
    if (options.newPort) {
      await new Promise<void>((resolve) => this.reservation?.close(() => resolve()));
      this.reservation = await reservePort();
    }
    await this.resume();
    await this.waitForIdle();
  }
  async stop() {
    await this.terminate('SIGTERM');
    if (this.reservation)
      await new Promise<void>((resolve) => this.reservation?.close(() => resolve()));
    this.reservation = undefined;
    if (process.env.E2E_KEEP !== '1') {
      await fixturePermissions(this.outDir, true);
      await rm(this.directory, { recursive: true, force: true });
    }
  }
  logs() {
    return this.output;
  }
  sql(query: string) {
    const db = new DatabaseSync(join(this.dataDir, 'apm.db'), { readOnly: true });
    try {
      return db.prepare(query).all();
    } finally {
      db.close();
    }
  }
  async applyFixtureMutations() {
    await run('bash', ['fixtures/mutations.sh', '--out', this.outDir]);
    const rows = (await readFile(join(this.outDir, 'mutations.tsv'), 'utf8')).trim().split('\n');
    const columns = rows.shift()?.replace(/^#\s*/, '').split('\t') ?? [];
    return rows.map((row) =>
      Object.fromEntries(row.split('\t').map((value, index) => [columns[index], value])),
    );
  }
}
