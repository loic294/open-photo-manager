import { snapshotTree } from './disk';
import { test as base, expect } from './test';

/** Unbacked connection tests must not remove any fixture file. */
export const test = base.extend<{ immichFileGuard: undefined }>({
  immichFileGuard: [
    async ({ app, mockImmich }, use) => {
      // Establish the mock fixture too, so its violation check always runs.
      void mockImmich;
      const before = (await snapshotTree(app.volumesDir))
        .filter((entry) => entry.type !== 'directory')
        .map((entry) => entry.path);
      try {
        await use(undefined);
      } finally {
        const after = new Set(
          (await snapshotTree(app.volumesDir))
            .filter((entry) => entry.type !== 'directory')
            .map((entry) => entry.path),
        );
        expect(
          before.filter((path) => !after.has(path)),
          'fixture files disappeared',
        ).toEqual([]);
      }
    },
    { auto: true },
  ],
});

export { expect };
