import { startMockImmich } from './src/app.ts';
import { parseCli, USAGE, UsageError } from './src/cli.ts';

const LOOPBACK_HOSTS = new Set(['127.0.0.1', '::1', 'localhost']);

function describe(error: unknown): string {
  if (error instanceof Error) {
    const { code } = error as NodeJS.ErrnoException;
    return code === 'EADDRINUSE' ? `${error.message} (--port 0 picks a free port)` : error.message;
  }
  return String(error);
}

async function main(): Promise<void> {
  let cli: ReturnType<typeof parseCli>;
  try {
    cli = parseCli(process.argv.slice(2));
  } catch (error) {
    if (error instanceof UsageError) {
      process.stderr.write(`mock-immich: ${error.message}\n\n${USAGE}`);
      process.exitCode = 2;
      return;
    }
    throw error;
  }
  if (cli.help) {
    process.stdout.write(USAGE);
    return;
  }

  const { options } = cli;
  if (options.host !== undefined && !LOOPBACK_HOSTS.has(options.host)) {
    process.stderr.write(
      `mock-immich: warning: listening on ${options.host}: whoever can reach it can sign in with the ` +
        'test accounts and, with a forced delete, remove files under the mounts\n',
    );
  }

  const mock = await startMockImmich(options);
  process.stderr.write(
    `mock-immich: ${mock.url} (scenario ${cli.scenarioArgument ?? 'default'}, state in memory)\n`,
  );
  for (const [prefix, dir] of Object.entries(options.mounts ?? {})) {
    process.stderr.write(`mock-immich: mount ${prefix} -> ${dir}\n`);
  }
  if (mock.tls) {
    process.stderr.write(
      'mock-immich: self-signed certificate: a client must allow insecure TLS to trust it\n',
    );
  }
  process.stdout.write(`${JSON.stringify({ listening: mock.url, tls: mock.tls })}\n`);

  let stopping = false;
  const shutdown = (signal: string) => {
    if (stopping) {
      return;
    }
    stopping = true;
    process.stderr.write(`mock-immich: ${signal}, stopping\n`);
    mock.stop().then(
      () => process.exit(0),
      (error: unknown) => {
        process.stderr.write(`mock-immich: could not stop cleanly: ${describe(error)}\n`);
        process.exit(1);
      },
    );
  };
  process.on('SIGINT', () => shutdown('SIGINT'));
  process.on('SIGTERM', () => shutdown('SIGTERM'));
}

main().catch((error: unknown) => {
  process.stderr.write(`mock-immich: ${describe(error)}\n`);
  process.exitCode = 1;
});
