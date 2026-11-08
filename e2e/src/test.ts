import { test as base, expect } from '@playwright/test';
import { scenarioFiles, startMockImmich } from '../../tools/mock-immich/index.ts';
import { AppInstance, type AppOptions } from './app-instance';
import { snapshotTree } from './disk';
import { Manifest } from './manifest';
import { PageGuard } from './page-guard';
import { BottomBarPage } from './pages/bottom-bar';
import { ConfirmDialogPage } from './pages/confirm-dialog';
import { FilterBarPage } from './pages/filter-bar';
import { PanePage } from './pages/pane';
import { SettingsPage } from './pages/settings';
import { ShellPage } from './pages/shell';
import { TrashPage } from './pages/trash';
import { ViewerPage } from './pages/viewer';

type Mock = Awaited<ReturnType<typeof startMockImmich>>;
type MockOptions = Parameters<typeof startMockImmich>[0];
interface Fixtures {
  allowedErrors: (string | RegExp)[];
  appOptions: AppOptions;
  app: AppInstance;
  bottomBar: BottomBarPage;
  comparison: PanePage;
  confirmDialog: ConfirmDialogPage;
  filterBar: FilterBarPage;
  manifest: Manifest;
  mockImmich: (options?: MockOptions) => Promise<Mock>;
  readOnlyGuard: undefined;
  settingsPage: SettingsPage;
  shell: ShellPage;
  source: PanePage;
  trashPage: TrashPage;
  viewer: ViewerPage;
}
interface Workers {
  sharedApp: AppInstance;
}

export function indexSnapshot(app: AppInstance) {
  return app.sql(
    'SELECT id, volume_id, dir_path, name, size, mtime, hex(hash) AS hash, camera_id, capture_ts, capture_src, rating, flag FROM files ORDER BY id',
  );
}

export const test = base.extend<Fixtures, Workers>({
  allowedErrors: [[], { option: true }],
  appOptions: [{}, { option: true }],
  app: async ({ appOptions }, use, info) => {
    const app = await AppInstance.start(appOptions);
    try {
      await use(app);
    } finally {
      if (info.status !== info.expectedStatus)
        await info.attach('backend-log', { body: app.logs(), contentType: 'text/plain' });
      await app.stop();
    }
  },
  bottomBar: async ({ page }, use) => {
    await use(new BottomBarPage(page));
  },
  comparison: async ({ page }, use) => {
    await use(new PanePage(page, 'comparison'));
  },
  confirmDialog: async ({ page }, use) => {
    await use(new ConfirmDialogPage(page));
  },
  filterBar: async ({ page }, use) => {
    await use(new FilterBarPage(page));
  },
  manifest: async ({ sharedApp }, use) => {
    await use(await Manifest.load(sharedApp.outDir));
  },
  mockImmich: async ({ app }, use) => {
    const mocks: Mock[] = [];
    await use(async (options = {}) => {
      const manifest = await Manifest.load(app.outDir);
      const mock = await startMockImmich({
        ...options,
        port: 0,
        scenario: options.scenario ?? scenarioFiles.fixtures,
        mounts: Object.fromEntries(
          manifest.data.volumes.map((volume) => [`/mnt/${volume}`, app.volumePath(volume)]),
        ),
      });
      mocks.push(mock);
      return mock;
    });
    for (const mock of mocks) {
      await mock.stop();
      expect(mock.state.violations()).toEqual([]);
    }
  },
  page: async ({ page, allowedErrors }, use) => {
    const guard = new PageGuard(page, allowedErrors);
    await use(page);
    guard.assertClean();
  },
  readOnlyGuard: [
    async ({ sharedApp }, use) => {
      const tree = await snapshotTree(sharedApp.volumesDir, { hash: true });
      const index = indexSnapshot(sharedApp);
      await use(undefined);
      expect(
        await snapshotTree(sharedApp.volumesDir, { hash: true }),
        'sharedApp volume mutation',
      ).toEqual(tree);
      expect(indexSnapshot(sharedApp), 'sharedApp index mutation').toEqual(index);
    },
    { auto: true },
  ],
  settingsPage: async ({ page }, use) => {
    await use(new SettingsPage(page));
  },
  sharedApp: [
    // biome-ignore lint/correctness/noEmptyPattern: Playwright inspects fixture dependencies by destructuring.
    async ({}, use) => {
      const app = await AppInstance.start();
      try {
        await use(app);
      } finally {
        await app.stop();
      }
    },
    { scope: 'worker', timeout: 120_000 },
  ],
  shell: async ({ page }, use) => {
    await use(new ShellPage(page));
  },
  source: async ({ page }, use) => {
    await use(new PanePage(page, 'source'));
  },
  trashPage: async ({ page }, use) => {
    await use(new TrashPage(page));
  },
  viewer: async ({ page }, use) => {
    await use(new ViewerPage(page));
  },
});
export { expect };
