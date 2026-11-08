import { test as base, expect } from '@playwright/test';
import { scenarioFiles, startMockImmich } from '../../tools/mock-immich/index.ts';
import { AppInstance, type AppOptions } from './app-instance';
import { snapshotTree } from './disk';
import { Manifest } from './manifest';
import { PageGuard } from './page-guard';
import { BottomBarPage } from './pages/bottom-bar';
import { CleanupPage } from './pages/cleanup-page';
import { ConfirmDialogPage } from './pages/confirm-dialog';
import { DuplicatesPage } from './pages/duplicates-page';
import { FilterBarPage } from './pages/filter-bar';
import { FormatPage } from './pages/format-page';
import { MovePage } from './pages/move-page';
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
  cleanupPage: CleanupPage;
  comparison: PanePage;
  confirmDialog: ConfirmDialogPage;
  duplicatesPage: DuplicatesPage;
  filterBar: FilterBarPage;
  formatPage: FormatPage;
  manifest: Manifest;
  mockImmich: (options?: MockOptions) => Promise<Mock>;
  movePage: MovePage;
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

export async function captureReadOnly(app: AppInstance) {
  return {
    tree: await snapshotTree(app.volumesDir, { hash: true }),
    index: ['files', 'dirs', 'cameras', 'tags', 'file_tags', 'file_sidecars'].map((table) =>
      app.sql(`SELECT * FROM ${table}`),
    ),
  };
}

export async function assertReadOnly(
  app: AppInstance,
  before: Awaited<ReturnType<typeof captureReadOnly>>,
) {
  const after = await captureReadOnly(app);
  expect(after.tree, 'sharedApp volume mutation').toEqual(before.tree);
  expect(after.index, 'sharedApp index mutation').toEqual(before.index);
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
  cleanupPage: async ({ page }, use) => {
    await use(new CleanupPage(page));
  },
  comparison: async ({ page }, use) => {
    await use(new PanePage(page, 'comparison'));
  },
  confirmDialog: async ({ page }, use) => {
    await use(new ConfirmDialogPage(page));
  },
  duplicatesPage: async ({ page }, use) => {
    await use(new DuplicatesPage(page));
  },
  filterBar: async ({ page }, use) => {
    await use(new FilterBarPage(page));
  },
  formatPage: async ({ page }, use) => {
    await use(new FormatPage(page));
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
  movePage: async ({ page }, use) => {
    await use(new MovePage(page));
  },
  page: async ({ page, allowedErrors }, use) => {
    const guard = new PageGuard(page, allowedErrors);
    await use(page);
    guard.assertClean();
  },
  readOnlyGuard: [
    async ({ sharedApp }, use) => {
      const before = await captureReadOnly(sharedApp);
      await use(undefined);
      await assertReadOnly(sharedApp, before);
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
