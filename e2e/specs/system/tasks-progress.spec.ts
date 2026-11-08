import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { chmod, readFile } from 'node:fs/promises';
import { basename } from 'node:path';
import { expectNoSeriousA11yViolations } from '../../src/a11y';
import { emptyFilter } from '../../src/api';
import { CrossDevice } from '../../src/cross-device';
import { makeSparseFile, snapshotTree, writeFile } from '../../src/disk';
import { captureScreens } from '../../src/screens';
import { expect, test } from '../../src/test';
import {
  executePlan,
  indexedFile,
  movePlan,
  taskAction,
  terminalTask,
  trashFixtures,
  trashList,
} from '../../src/tool-oracles';
import { expectStaysFalse } from '../../src/waits';

const COPY_BYTES = 3 * 1024 ** 3;
async function diskHash(path: string) {
  const hash = createHash('sha1');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}
test.describe.configure({ mode: 'default' });
test.use({
  // Reload and intentionally stopped backend: only this recovery scenario permits network failures.
  allowedErrors: [
    [
      /requestfailed: .*\/api\/(?:events|tasks|index\/status|volumes|browse\/\w+|facets|settings|immich\/status).* net::ERR_(?:ABORTED|CONNECTION_REFUSED|EMPTY_RESPONSE|CONNECTION_RESET)/,
      /requestfailed: .*\/api\/files\/\d+\/(?:thumb|preview).* net::ERR_ABORTED/,
      /^console.error: (?:Failed to load resource: net::ERR_(?:CONNECTION_REFUSED|EMPTY_RESPONSE|CONNECTION_RESET)|EventSource's response has a MIME type)/,
    ],
    { scope: 'test' },
  ],
});

test('real cross-device copy preserves bytes and exposes measured progress @slow', async ({
  app,
  page,
  shell,
}, info) => {
  test.setTimeout(300_000);
  test.skip(
    process.platform !== 'darwin',
    'Real cross-device mount fixture requires macOS hdiutil; no copy coverage claimed elsewhere',
  );
  const device = await CrossDevice.create(app, COPY_BYTES);
  try {
    const existingParts = (await snapshotTree(app.volumesDir))
      .filter((entry) => entry.path.endsWith('.apm-part'))
      .map((entry) => entry.path);
    await makeSparseFile(app, 'photos', 'system-copy/large.bin', COPY_BYTES);
    await app.rescan();
    const original = await indexedFile(app, 'photos', 'system-copy/large.bin');
    expect(await diskHash(app.volumePath('photos/system-copy/large.bin'))).toBe(original.hash);
    await page.goto(`${app.url}/browse?src=photos/system-copy&cmp=${device.volume}`);
    await expect(shell.taskTrigger()).toHaveCount(0, { timeout: 10_000 });
    const plan = await movePlan(
      app,
      { volume: 'photos', dir: 'system-copy' },
      { volume: device.volume, dir: 'copied' },
    );
    const task = await executePlan(app, plan);
    await expect(shell.taskTrigger()).toBeVisible();
    await shell.hoverTasks();
    await expect(shell.taskPanel()).toBeVisible();
    await expect(shell.taskPanel()).toContainText('MB/s', { timeout: 30_000 });
    await expect(shell.taskPanel()).toContainText('left');
    const progress = shell.taskTrigger().getByRole('progressbar');
    await expect.poll(async () => Number(await progress.getAttribute('value'))).toBeGreaterThan(0);
    const firstProgress = Number(await progress.getAttribute('value'));
    await expect
      .poll(async () => Number(await progress.getAttribute('value')))
      .toBeGreaterThan(firstProgress);
    const started = performance.now();
    const browse = { volume_id: null, dir: '', filter: emptyFilter, plan: null };
    const flood = await Promise.all(
      Array.from({ length: 200 }, (_, index) =>
        index % 2 === 0 ? app.api.folders(browse) : app.api.facets(browse),
      ),
    );
    expect(flood).toHaveLength(200);
    expect(await app.api.request('GET', '/api/health')).toMatchObject({ status: 'ok' });
    expect(
      performance.now() - started,
      'Read-only POST browse/facet load and health remain responsive',
    ).toBeLessThan(30_000);
    const result = await terminalTask(app, task.id);
    expect(result.state).toBe('done');
    const seconds = (result.finished_at ?? 0) - (result.started_at ?? 0);
    await info.attach('copy-runtime', {
      body: JSON.stringify({
        bytes: COPY_BYTES,
        seconds,
        deviceEvidence: device.evidence,
      }),
      contentType: 'application/json',
    });
    expect(
      seconds,
      'Real file task must last at least 15 seconds; calibrate rather than weaken this gate',
    ).toBeGreaterThanOrEqual(15);
    const moved = await indexedFile(app, device.volume, 'copied/large.bin');
    expect(moved.hash).toBe(original.hash);
    expect(moved.item.size).toBe(COPY_BYTES);
    expect(await diskHash(app.volumePath(`${device.volume}/copied/large.bin`))).toBe(original.hash);
    const tree = await snapshotTree(app.volumesDir);
    expect(
      tree.filter((entry) => entry.path.endsWith('.apm-part')).map((entry) => entry.path),
    ).toEqual(existingParts);
    expect(tree.some((entry) => entry.path === 'photos/system-copy/large.bin')).toBe(false);
  } finally {
    await device.close();
  }
});

test('task hover, keyboard, live reload and cancel leave the rest of the UI responsive @slow', async ({
  app,
  manifest,
  page,
  shell,
  source,
  filterBar,
  viewer,
  bottomBar,
  settingsPage,
}) => {
  test.setTimeout(300_000);
  test.skip(
    process.platform !== 'darwin',
    'Real cross-device mount fixture requires macOS hdiutil',
  );
  const device = await CrossDevice.create(app, COPY_BYTES);
  try {
    const existingParts = (await snapshotTree(app.volumesDir))
      .filter((entry) => entry.path.endsWith('.apm-part'))
      .map((entry) => entry.path);
    const template = manifest.filesOf('exif.full_nikon')[0];
    await makeSparseFile(app, 'photos', 'system-cancel/large.bin', COPY_BYTES);
    await writeFile(
      app,
      'photos',
      'system-queued/queued.jpg',
      await readFile(app.volumePath(`photos/${template.path}`)),
    );
    await app.rescan();
    const longPlan = await movePlan(
      app,
      { volume: 'photos', dir: 'system-cancel' },
      { volume: device.volume, dir: 'cancelled' },
    );
    const queuedPlan = await movePlan(
      app,
      { volume: 'photos', dir: 'system-queued' },
      { volume: device.volume, dir: 'queued' },
    );
    await page.goto(`${app.url}/browse?src=photos/2021/03&cmp=photos/2021/03`);
    await page.evaluate(() => {
      const durations: number[] = [];
      new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) durations.push(entry.duration);
        document.documentElement.dataset.longTasks = JSON.stringify(durations);
      }).observe({ type: 'longtask', buffered: true });
    });
    const task = await executePlan(app, longPlan);
    const queued = await executePlan(app, queuedPlan);
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task.id)?.bytes_done, {
        timeout: 30_000,
      })
      .toBeGreaterThan(0);
    expect((await app.api.tasks()).find((entry) => entry.id === queued.id)?.state).toBe('queued');
    await shell.taskTrigger().focus();
    await expect(shell.taskPanel()).toBeVisible();
    await expect(shell.taskPanel()).toContainText('Waiting');
    await expect(shell.taskPanel()).toContainText('MB/s');
    await expect(shell.taskPanel()).toContainText('left');
    for (const theme of ['light', 'dark'] as const) {
      await shell.theme(theme);
      await expectNoSeriousA11yViolations(page, { include: ['apm-task-indicator'] });
    }
    await captureScreens(page, 'system-task-panel');
    await page.keyboard.press('Escape');
    await expect(shell.taskPanel()).not.toBeVisible();
    await expect(shell.taskTrigger()).toBeFocused();
    await shell.hoverTasks();
    await shell.taskPanel().hover();
    await expect(shell.taskPanel()).toBeVisible();
    await page.keyboard.press('Escape');
    const start = performance.now();
    await source.card(basename(template.path)).select();
    await source.selectAll();
    await expect(source.card(basename(template.path)).element).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await source.clear();
    await filterBar.rating(3);
    await filterBar.clear();
    await source.card(basename(template.path)).click();
    await viewer.expectOpen(basename(template.path));
    await viewer.close();
    await bottomBar.size('source', 192);
    expect(await bottomBar.sizeValue('source')).toBe('192');
    await shell.navigate('Settings');
    await settingsPage.fill('Full rescan interval', '33');
    await settingsPage.saveGeneral();
    await shell.navigate('Browse');
    expect(
      performance.now() - start,
      'Independent interactions complete under a generous bound',
    ).toBeLessThan(15_000);
    await expect(shell.taskTrigger()).toBeVisible();
    const durations = JSON.parse(
      (await page.locator('html').getAttribute('data-long-tasks')) ?? '[]',
    ) as number[];
    expect(Math.max(0, ...durations), 'UI long-task budget').toBeLessThan(1_000);
    await page.reload();
    await shell.hoverTasks();
    await expect(shell.taskPanel()).toContainText('MB/s');
    await expect
      .poll(
        async () => {
          const current = (await app.api.tasks()).find((entry) => entry.id === task.id);
          if (current?.state !== 'running')
            throw new Error('Copy finished before the 15 s non-blocking/cancellation gate');
          return Date.now() / 1000 - (current.started_at ?? Date.now() / 1000);
        },
        { timeout: 30_000 },
      )
      .toBeGreaterThanOrEqual(15);
    const row = shell
      .taskRows()
      .filter({ has: page.getByRole('button', { name: /^Cancel/ }) })
      .first();
    await row.getByRole('button', { name: /^Cancel/ }).click();
    await expect(row).toContainText('Cancelling');
    expect((await terminalTask(app, task.id)).state).toBe('cancelled');
    expect((await terminalTask(app, queued.id)).state).toBe('done');
    expect((await indexedFile(app, 'photos', 'system-cancel/large.bin')).item.size).toBe(
      COPY_BYTES,
    );
    expect(await diskHash(app.volumePath('photos/system-cancel/large.bin'))).toBe(
      (await indexedFile(app, 'photos', 'system-cancel/large.bin')).hash,
    );
    expect((await indexedFile(app, device.volume, 'queued/queued.jpg')).hash).toBe(template.sha1);
    const tree = await snapshotTree(app.volumesDir);
    expect(
      tree.filter((entry) => entry.path.endsWith('.apm-part')).map((entry) => entry.path),
    ).toEqual(existingParts);
    expect(tree.some((entry) => entry.path === `${device.volume}/cancelled/large.bin`)).toBe(false);
    await expect(shell.taskTrigger()).toHaveCount(0, { timeout: 10_000 });
  } finally {
    await device.close();
  }
});

test('failed tasks retain the full error, dismiss persistently and raise a new badge on the next failure', async ({
  app,
  manifest,
  page,
  shell,
  trashPage,
  confirmDialog,
}) => {
  test.setTimeout(120_000);
  test.skip(
    process.getuid?.() === 0,
    'Root bypasses Unix permissions; permission-driven failure is not covered as root',
  );
  await trashFixtures(app, manifest.filesOf('deep.grid_500').slice(0, 1));
  const item = (await trashList(app)).items[0];
  const parent = app.volumePath(`${item.volume}/.apm-trash/${item.batch}/${item.dir}`);
  await chmod(parent, 0o555);
  try {
    await page.goto(`${app.url}/trash`);
    await trashPage.tab('Items');
    await expect(trashPage.rows()).toHaveCount(1);
    for (let attempt = 0; attempt < 2; attempt++) {
      await trashPage.empty();
      const failed = await taskAction(app, page, '/api/trash/empty', () =>
        confirmDialog.confirm('Empty trash'),
      );
      expect(failed.state).toBe('failed');
      expect(failed.error).toBeTruthy();
      await shell.hoverTasks();
      const row = shell.taskRows().filter({ hasText: failed.error ?? 'Missing failure text' });
      await expect(row.locator('[data-state]')).toHaveAttribute('data-state', 'failed');
      await expect(row.locator('.alert-error')).toContainText(failed.error ?? '');
      await expect(shell.taskTrigger().locator('.badge-error')).toHaveText('1');
      // Failed rows have no expiry event; they must survive the successful/cancelled grace period.
      await expectStaysFalse(async () => (await row.count()) === 0, { forMs: 5_500 });
      expect((await trashList(app)).summary.items).toBe(1);
      await row.getByRole('button', { name: /^Dismiss/ }).click();
      await expect(shell.taskTrigger()).toHaveCount(0);
      await page.reload();
      await expect(trashPage.rows()).toHaveCount(1);
      await expect(shell.taskTrigger()).toHaveCount(0);
    }
  } finally {
    await chmod(parent, 0o755);
  }
});

test('SIGKILL during a real copy reconciles the journal and reconnects without reload @slow', async ({
  app,
  page,
  shell,
}) => {
  test.setTimeout(300_000);
  test.skip(
    process.platform !== 'darwin',
    'Real cross-device mount fixture requires macOS hdiutil',
  );
  const device = await CrossDevice.create(app, COPY_BYTES);
  try {
    const existingParts = (await snapshotTree(app.volumesDir))
      .filter((entry) => entry.path.endsWith('.apm-part'))
      .map((entry) => entry.path);
    await makeSparseFile(app, 'photos', 'system-crash/large.bin', COPY_BYTES);
    await app.rescan();
    const original = await indexedFile(app, 'photos', 'system-crash/large.bin');
    expect(await diskHash(app.volumePath('photos/system-crash/large.bin'))).toBe(original.hash);
    await page.goto(`${app.url}/browse?src=photos/system-crash&cmp=${device.volume}`);
    const plan = await movePlan(
      app,
      { volume: 'photos', dir: 'system-crash' },
      { volume: device.volume, dir: 'crashed' },
    );
    const task = await executePlan(app, plan);
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task.id)?.bytes_done, {
        timeout: 30_000,
      })
      .toBeGreaterThan(0);
    await app.kill();
    await expect(shell.connectionBanner()).toBeVisible({ timeout: 30_000 });
    await app.restart();
    await expect(shell.connectionBanner()).not.toBeVisible({ timeout: 45_000 });
    const interrupted = (await app.api.tasks()).find((entry) => entry.id === task.id);
    expect(interrupted?.state).toBe('interrupted');
    await shell.hoverTasks();
    await expect(shell.taskPanel()).toContainText('Interrupted');
    const tree = await snapshotTree(app.volumesDir);
    expect(
      tree.filter((entry) => entry.path.endsWith('.apm-part')).map((entry) => entry.path),
    ).toEqual(existingParts);
    const survivors = tree.filter(
      (entry) => entry.type === 'file' && entry.path.endsWith('/large.bin'),
    );
    expect(survivors).toHaveLength(1);
    expect(survivors[0].path).toBe('photos/system-crash/large.bin');
    expect(await diskHash(app.volumePath(survivors[0].path))).toBe(original.hash);
    const recovered = await indexedFile(app, 'photos', 'system-crash/large.bin');
    expect(recovered.hash).toBe(original.hash);
    expect(recovered.item.size).toBe(COPY_BYTES);
    expect(app.sql("SELECT count(*) AS n FROM fs_journal WHERE state = 'pending'")[0]?.n).toBe(0);
    await shell
      .taskPanel()
      .getByRole('button', { name: /^Dismiss/ })
      .click();
    await expect(shell.taskTrigger()).toHaveCount(0, { timeout: 10_000 });
  } finally {
    await device.close();
  }
});
