import { parseArgs } from 'node:util';
import type { MockImmichOptions } from './app.ts';

/** A command line the user got wrong: the message is shown with the usage text. */
export class UsageError extends Error {}

export const DEFAULT_CLI_PORT = 12_283;

export const USAGE = `Usage: node tools/mock-immich/server.ts [options]

A mock Immich server for development, browser checks and end-to-end tests.
It listens on the loopback interface and keeps its state in memory.

Options:
  --port N               Port to listen on; 0 picks a free one (default ${DEFAULT_CLI_PORT})
  --host ADDRESS         Address to bind (default 127.0.0.1; anything else is your decision)
  --scenario FILE        Scenario JSON file (default scenarios/default.json)
  --mount PREFIX=DIR     The real directory DIR is what the server sees at PREFIX, for example
                         --mount /mnt/photos=./volumes/photos (repeatable)
  --server-version V     Version to report and to behave as, for example 3.4.0 or 2.5.1
  --scan-delay-ms N      How long a library scan waits before it looks at the disk (default 250)
  --tls                  Serve https with a self-signed certificate (needs the openssl command)
  -h, --help             Show this text

The only line on stdout is {"listening":"http://127.0.0.1:12283","tls":false} once the server
is ready; everything else goes to stderr. SIGINT and SIGTERM stop it cleanly.
`;

export interface Cli {
  help: boolean;
  options: MockImmichOptions;
  /** What the user typed for --scenario, for the startup message. */
  scenarioArgument: string | undefined;
}

function integerOption(name: string, text: string | undefined, max: number): number | undefined {
  if (text === undefined) {
    return undefined;
  }
  if (!/^\d+$/.test(text) || Number(text) > max) {
    throw new UsageError(
      `--${name} must be an integer from 0 to ${max}, got ${JSON.stringify(text)}`,
    );
  }
  return Number(text);
}

/** `--mount PREFIX=DIR` (split at the first `=`) into the object `startMockImmich` takes. */
function mountsOption(specs: readonly string[] | undefined): Record<string, string> | undefined {
  if (specs === undefined) {
    return undefined;
  }
  const mounts: Record<string, string> = {};
  for (const spec of specs) {
    const at = spec.indexOf('=');
    if (at <= 0 || at === spec.length - 1) {
      throw new UsageError(
        `--mount expects PREFIX=DIR, for example /mnt/photos=./volumes/photos, got ${JSON.stringify(spec)}`,
      );
    }
    const prefix = spec.slice(0, at);
    if (Object.hasOwn(mounts, prefix)) {
      throw new UsageError(`--mount ${prefix} is given twice`);
    }
    mounts[prefix] = spec.slice(at + 1);
  }
  return mounts;
}

export function parseCli(argv: readonly string[]): Cli {
  let values: ReturnType<typeof read>;
  try {
    values = read(argv);
  } catch (error) {
    throw new UsageError(error instanceof Error ? error.message : String(error));
  }

  const options: MockImmichOptions = {
    port: integerOption('port', values.port, 65_535) ?? DEFAULT_CLI_PORT,
  };
  const scanDelayMs = integerOption('scan-delay-ms', values['scan-delay-ms'], 2_147_483_647);
  const mounts = mountsOption(values.mount);
  if (values.host !== undefined) {
    options.host = values.host;
  }
  if (values.scenario !== undefined) {
    options.scenario = values.scenario;
  }
  if (mounts !== undefined) {
    options.mounts = mounts;
  }
  if (values['server-version'] !== undefined) {
    options.serverVersion = values['server-version'];
  }
  if (scanDelayMs !== undefined) {
    options.scanDelayMs = scanDelayMs;
  }
  if (values.tls) {
    options.tls = true;
  }
  return { help: values.help === true, options, scenarioArgument: values.scenario };
}

function read(argv: readonly string[]) {
  return parseArgs({
    args: [...argv],
    allowPositionals: false,
    strict: true,
    options: {
      port: { type: 'string' },
      host: { type: 'string' },
      scenario: { type: 'string' },
      mount: { type: 'string', multiple: true },
      'server-version': { type: 'string' },
      'scan-delay-ms': { type: 'string' },
      tls: { type: 'boolean' },
      help: { type: 'boolean', short: 'h' },
    },
  }).values;
}
