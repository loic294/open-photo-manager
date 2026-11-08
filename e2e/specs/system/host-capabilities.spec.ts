import { basename } from 'node:path';
import type { HealthResponse } from '../../../web/src/api/generated/HealthResponse';
import { AppInstance } from '../../src/app-instance';
import { writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';
import { expectStaysFalse } from '../../src/waits';

test.describe('polling-only host policy', () => {
  test.use({
    appOptions: {
      env: { APM_WATCH_MODE: 'poll' },
      settings: (settings) => ({ ...settings, index: { ...settings.index, watch_enabled: true } }),
    },
  });
  test('poll registers no watcher; warnings and badges explain manual indexing @slow', async ({
    app,
    manifest,
    page,
    shell,
    source,
    settingsPage,
  }) => {
    test.slow();
    await expect
      .poll(async () => (await app.api.indexStatus()).watcher?.state)
      .toBe('polling_only');
    const status = await app.api.indexStatus();
    expect(status.watcher?.watched_dirs).toBe(0);
    expect(status.warnings.map((warning) => warning.code)).toEqual(['volumes_polling_only']);
    expect((await app.api.request<HealthResponse>('GET', '/api/health')).warnings).toEqual(
      status.warnings,
    );
    for (const volume of await app.api.volumes()) {
      expect(volume.watch_mode).toBe('polling_only');
      expect(volume.watch_reason).toContain('APM_WATCH_MODE=poll');
    }
    await page.goto(`${app.url}/browse?src=photos/stale&cmp=photos/stale`);
    await expect(shell.notices()).toContainText('noticed only by the next rescan');
    await shell.navigate('Settings');
    await expect(settingsPage.volumes()).toContainText('polling only');
    await expect(settingsPage.volumes().locator('[data-tip*="APM_WATCH_MODE=poll"]')).toHaveCount(
      2,
    );
    const file = manifest.filesOf('stale.rename')[0];
    await writeFile(app, 'photos', 'stale/poll-added.txt', 'external polling-only change');
    // Polling-only has no event for absence: cover a full native debounce window.
    await expectStaysFalse(
      async () =>
        (await app.api.listing('photos', 'stale')).items.some(
          (item) => item.name === 'poll-added.txt',
        ),
      { forMs: 6_000 },
    );
    await shell.navigate('Browse');
    await expect(source.card(basename(file.path)).element).toBeVisible();
    await shell.reindex();
    await expect(source.card('poll-added.txt').element).toBeVisible({ timeout: 30_000 });
    await app.waitForIdle();
    expect(
      (await app.api.listing('photos', 'stale')).items.some(
        (item) => item.name === 'poll-added.txt',
      ),
    ).toBe(true);
    expect(app.sql('PRAGMA journal_mode')[0]?.journal_mode).toBe('wal');
  });
});

for (const mode of [undefined, 'auto', 'watch']) {
  test.describe(`native policy ${mode ?? 'default'}`, () => {
    test.use({ appOptions: { watcher: true, env: mode ? { APM_WATCH_MODE: mode } : {} } });
    test('native watch and WAL remain supported @slow', async ({
      app,
      page,
      shell,
      settingsPage,
    }) => {
      test.slow();
      expect((await app.api.indexStatus()).watcher?.state).toBe('watching');
      expect((await app.api.indexStatus()).watcher?.unwatched_dirs).toBe(0);
      expect((await app.api.request<HealthResponse>('GET', '/api/health')).warnings).toEqual([]);
      expect((await app.api.indexStatus()).warnings).toEqual([]);
      for (const volume of await app.api.volumes()) {
        expect(volume.watch_mode).toBe('watching');
        expect(volume.watch_reason).toBe(
          mode === 'watch' ? 'filesystem watching is forced by APM_WATCH_MODE=watch' : null,
        );
        expect(volume.case_insensitive).toEqual(expect.any(Boolean));
        if (process.platform === 'darwin') expect(volume.fs_type).toBeNull();
      }
      await page.goto(`${app.url}/browse?src=photos/stale&cmp=photos/stale`);
      await writeFile(app, 'photos', 'stale/native-added.txt', 'a native watched file');
      await expect
        .poll(
          async () =>
            (await app.api.listing('photos', 'stale')).items.some(
              (item) => item.name === 'native-added.txt',
            ),
          { timeout: 30_000 },
        )
        .toBe(true);
      await expect(page.getByText('native-added.txt', { exact: true }).first()).toBeVisible();
      await shell.navigate('Settings');
      await expect(settingsPage.volumes()).not.toContainText('polling only');
      expect(app.sql('PRAGMA journal_mode')[0]?.journal_mode).toBe('wal');
    });
  });
}

test('invalid APM_WATCH_MODE refuses startup rather than silently defaulting @smoke', async () => {
  await expect(AppInstance.start({ env: { APM_WATCH_MODE: 'sometimes' } })).rejects.toThrow(
    /APM_WATCH_MODE[\s\S]*auto[\s\S]*watch[\s\S]*poll/,
  );
});
