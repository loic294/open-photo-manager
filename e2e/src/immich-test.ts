import { scenarioFiles, startMockImmich } from '../../tools/mock-immich/index.ts';
import { AppInstance } from './app-instance';
import { ImmichPostgres } from './immich-postgres';
import { Manifest } from './manifest';
import { test as base, expect } from './test';

type Mock = Awaited<ReturnType<typeof startMockImmich>>;

interface BackedImmich {
  app: AppInstance;
  database: ImmichPostgres;
  mock: Mock;
}

/** A private APM, upstream PostgreSQL schema, and fixture-mounted shared Immich mock. */
export const test = base.extend<{ backedImmich: BackedImmich }>({
  backedImmich: [
    // biome-ignore lint/correctness/noEmptyPattern: Playwright requires destructured fixture dependencies.
    async ({}, use, info) => {
      if (info.config.workers !== 1) {
        throw new Error('Backed Immich tests require E2E_WORKERS=1 for one Docker stack at a time');
      }
      const database = await ImmichPostgres.start();
      let app: AppInstance | undefined;
      let mock: Mock | undefined;
      try {
        const runningApp = await AppInstance.start({
          env: {
            APM_IMMICH_DATABASE_URL_FILE: database.secretFile,
            APM_IMMICH_QUIET_SECS: '1',
          },
        });
        app = runningApp;
        const manifest = await Manifest.load(runningApp.outDir);
        let seeded = false;
        let liveMock: Mock;
        liveMock = await startMockImmich({
          scenario: scenarioFiles.fixtures,
          serverVersion: '3.3.3',
          scanDelayMs: 0,
          mounts: Object.fromEntries(
            manifest.data.volumes.map((volume) => [
              `/mnt/${volume}`,
              runningApp.volumePath(volume),
            ]),
          ),
          beforeAssetRead: () => database.refreshMock(liveMock),
          readOnlyBacking: () => seeded,
          beforeAssetDelete: (assets, force, now) => database.deleteAssets(assets, force, now),
          beforeScanCommit: (scan) => database.commitScan(scan),
        });
        mock = liveMock;
        const scans = await mock.control.scan();
        expect(scans.map(({ status, error }) => ({ status, error }))).toEqual(
          scans.map(() => ({ status: 'completed', error: null })),
        );
        await database.seedMock(mock);
        seeded = true;
        await use({ app, database, mock });
      } finally {
        try {
          if (app && info.status !== info.expectedStatus) {
            await info.attach('backend-log', { body: app.logs(), contentType: 'text/plain' });
          }
        } finally {
          try {
            await app?.stop();
          } finally {
            try {
              await mock?.stop();
              if (mock) expect(mock.state.violations()).toEqual([]);
            } finally {
              await database.stop();
            }
          }
        }
      }
    },
    { timeout: 300_000 },
  ],
});

export { expect };
