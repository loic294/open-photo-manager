import type { TaskDto } from '../../../web/src/api/generated/TaskDto';
import { connectThroughUi, mapAndSync, waitForTask } from '../../src/immich';
import { expect, test } from '../../src/immich-readonly-test';

// Navigating from Settings to Browse closes the old SSE stream.
test.use({ allowedErrors: [/^requestfailed: .*\/api\/events net::ERR_ABORTED$/] });

test('a gated Immich sync leaves browsing usable and coalesces follow-up requests', async ({
  app,
  page,
  shell,
  settingsPage,
  source,
  comparison,
  filterBar,
  mockImmich,
}) => {
  test.setTimeout(180_000);
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app, ['photos']);
  mock.control.gate('POST /api/search/metadata', { times: 1 });
  let task: TaskDto | undefined;
  let followUp: TaskDto | undefined;
  try {
    task = await app.api.request<TaskDto>('POST', '/api/immich/sync', { full: true });
    await mock.control.waitForHeld(1);
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task?.id)?.state)
      .toBe('running');
    followUp = await app.api.request<TaskDto>('POST', '/api/immich/sync', { full: true });
    const merged = await app.api.request<TaskDto>('POST', '/api/immich/sync', { full: true });
    expect(followUp.id).not.toBe(task.id);
    expect(merged.id).toBe(followUp.id);
    const url = new URL('/browse', app.url);
    url.searchParams.set('src', 'photos/sidecars');
    url.searchParams.set('cmp', 'photos/sidecars');
    await page.goto(url.href);
    await expect(source.card('IMG_0010.JPG').element).toBeVisible();
    await expect(comparison.card('IMG_0010.JPG').element).toBeVisible();
    await source.card('IMG_0010.JPG').select();
    await expect(source.card('IMG_0010.JPG').element).toHaveAttribute('aria-selected', 'true');
    await filterBar.searchCamera('Sony');
    await expect(filterBar.element.getByRole('searchbox')).toHaveValue('Sony');
    expect((await app.api.tasks()).find((entry) => entry.id === task?.id)?.state).toBe('running');
  } finally {
    mock.control.releaseGate();
  }
  if (!task) throw new Error('Immich sync did not start');
  if (!followUp) throw new Error('Immich follow-up sync did not start');
  await waitForTask(app, task.id);
  await waitForTask(app, followUp.id);
});

test('a sync can be cancelled from the task indicator and run again', async ({
  app,
  page,
  shell,
  settingsPage,
  mockImmich,
}) => {
  test.setTimeout(180_000);
  const mock = await mockImmich({ serverVersion: '3.3.3', scanDelayMs: 0 });
  expect((await mock.control.scan()).every((job) => job.status === 'completed')).toBe(true);
  await connectThroughUi(app, page, shell, settingsPage, mock.url);
  await mapAndSync(app, ['photos']);
  mock.control.gate('POST /api/search/metadata', { times: 1 });
  try {
    const task = await app.api.request<TaskDto>('POST', '/api/immich/sync', { full: true });
    await mock.control.waitForHeld(1);
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task.id)?.state)
      .toBe('running');
    await shell.hoverTasks();
    const row = shell
      .taskIndicator()
      .locator('opm-task-row')
      .filter({
        has: page.getByRole('button', { name: `Cancel ${task.title}`, exact: true }),
      });
    await expect(row).toBeVisible();
    await row.getByRole('button', { name: `Cancel ${task.title}`, exact: true }).click();
    await expect
      .poll(async () => (await app.api.tasks()).find((entry) => entry.id === task.id)?.state, {
        timeout: 120_000,
      })
      .toBe('cancelled');
  } finally {
    mock.control.releaseGate();
  }
  const next = await app.api.request<TaskDto>('POST', '/api/immich/sync', { full: true });
  await waitForTask(app, next.id);
});
