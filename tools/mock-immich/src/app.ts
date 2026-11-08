import {
  createServer as createHttpServer,
  type IncomingMessage,
  type Server,
  type ServerResponse,
} from 'node:http';
import { createServer as createHttpsServer } from 'node:https';
import type { AddressInfo } from 'node:net';
import { createControl, createState, type MockControl, type MockState } from './control.ts';
import { createControlRouter } from './control-http.ts';
import { createFaults } from './faults.ts';
import type { MockContext, Settings } from './mock.ts';
import { normaliseMounts } from './mounts.ts';
import { createRequestHandler } from './pipeline.ts';
import { createRequestLog } from './requestlog.ts';
import { createRouter } from './router.ts';
import { routes } from './routes/index.ts';
import { loadScenario } from './scenario.ts';
import { createData, seedData } from './state.ts';
import { type Certificate, createSelfSignedCertificate } from './tls.ts';
import type { Scenario } from './types.ts';
import { createClock } from './util.ts';
import { createChecker } from './validate.ts';
import { parseServerVersion } from './version.ts';

export interface MockImmichOptions {
  /** 0 (the default) picks a free port. */
  port?: number;
  /** Loopback by default: the mock has no business on the network. */
  host?: string;
  /** A parsed scenario or the path of a JSON file; `scenarios/default.json` when absent. */
  scenario?: string | object;
  /** Real directories that stand for the paths of the server: `{ '/mnt/photos': '/data/photos' }`. */
  mounts?: Record<string, string>;
  /** Overrides the scenario's version (`3.4.0`, `2.5.1`). */
  serverVersion?: string;
  /** How long a library scan waits before it looks at the disk (250 by default). */
  scanDelayMs?: number;
  latencyMs?: number;
  /** Serve https with a self-signed certificate (needs the `openssl` command). */
  tls?: boolean;
}

export interface MockImmich {
  url: string;
  port: number;
  host: string;
  tls: boolean;
  /** The PEM certificate of a `tls` mock, to trust it from a client. */
  certificate?: string;
  state: MockState;
  control: MockControl;
  stop(): Promise<void>;
}

const DEFAULT_VERSION = '3.4.0';
const DEFAULT_SETTINGS: Settings = {
  scanDelayMs: 250,
  latencyMs: 0,
  legacyUnknownFields: 'reject',
  unknownAssetStatus: 400,
};
const OPTION_KEYS = [
  'port',
  'host',
  'scenario',
  'mounts',
  'serverVersion',
  'scanDelayMs',
  'latencyMs',
  'tls',
];

function checkOptions(options: MockImmichOptions): void {
  const c = createChecker(options, [], 'options.');
  c.only(OPTION_KEYS);
  c.integer('port', { min: 0, max: 65_535 });
  c.string('host', { nonEmpty: true });
  c.string('serverVersion', { nonEmpty: true });
  c.integer('scanDelayMs', { min: 0 });
  c.integer('latencyMs', { min: 0 });
  c.boolean('tls');
  if (c.errors.length > 0) {
    throw new Error(`invalid options:\n  - ${c.errors.join('\n  - ')}`);
  }
}

/** An explicit option beats the scenario's `options`, which beat the defaults. */
function resolveSettings(scenario: Scenario, options: MockImmichOptions): Settings {
  const settings: Settings = { ...DEFAULT_SETTINGS, ...scenario.options };
  settings.scanDelayMs = options.scanDelayMs ?? settings.scanDelayMs;
  settings.latencyMs = options.latencyMs ?? settings.latencyMs;
  return settings;
}

function urlHost(host: string): string {
  if (host === '0.0.0.0') {
    return '127.0.0.1';
  }
  if (host === '::') {
    return '[::1]';
  }
  return host.includes(':') ? `[${host}]` : host;
}

function listen(server: Server, port: number, host: string): Promise<AddressInfo> {
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, host, () => {
      server.off('error', reject);
      resolve(server.address() as AddressInfo);
    });
  });
}

/** Starts a mock Immich server; `stop()` closes it and removes what it wrote to disk. */
export async function startMockImmich(options: MockImmichOptions = {}): Promise<MockImmich> {
  checkOptions(options);
  const { scenario } = loadScenario(options.scenario);
  const versionText = options.serverVersion ?? scenario.serverVersion ?? DEFAULT_VERSION;
  const version = parseServerVersion(versionText);
  if (version === null) {
    throw new Error(`invalid server version ${JSON.stringify(versionText)}: expected 3.4.0`);
  }

  const settings = resolveSettings(scenario, options);
  const abort = new AbortController();
  const ctx: MockContext = {
    data: createData(),
    clock: createClock(),
    scenario,
    version,
    settings,
    baseSettings: { ...settings },
    mounts: normaliseMounts(options.mounts),
    faults: createFaults(),
    log: createRequestLog(),
    scans: new Map(),
    stopSignal: abort.signal,
  };
  seedData(ctx);

  const control = createControl(ctx);
  const state = createState(ctx);
  const handler = createRequestHandler(
    ctx,
    createRouter(routes),
    createControlRouter(ctx, control, state),
  );
  const serve = (req: IncomingMessage, res: ServerResponse) => {
    handler(req, res).catch((error: unknown) => {
      process.stderr.write(`mock-immich: request handler failed: ${String(error)}\n`);
      res.destroy();
    });
  };

  const host = options.host ?? '127.0.0.1';
  let certificate: Certificate | undefined;
  let server: Server;
  let address: AddressInfo;
  try {
    certificate = options.tls ? createSelfSignedCertificate() : undefined;
    server = certificate
      ? createHttpsServer({ key: certificate.key, cert: certificate.cert }, serve)
      : createHttpServer(serve);
    address = await listen(server, options.port ?? 0, host);
  } catch (error) {
    certificate?.cleanup();
    throw error;
  }

  let stopping: Promise<void> | undefined;
  const stop = () => {
    stopping ??= (async () => {
      abort.abort();
      ctx.faults.releaseGates();
      await new Promise<void>((resolve) => {
        server.close(() => resolve());
        server.closeAllConnections();
      });
      certificate?.cleanup();
    })();
    return stopping;
  };

  const tls = certificate !== undefined;
  return {
    url: `${tls ? 'https' : 'http'}://${urlHost(host)}:${address.port}`,
    port: address.port,
    host,
    tls,
    ...(certificate === undefined ? {} : { certificate: certificate.cert }),
    state,
    control,
    stop,
  };
}
