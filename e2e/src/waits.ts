import { stat } from 'node:fs/promises';
import { basename, dirname } from 'node:path';
import { setTimeout } from 'node:timers/promises';
import { expect } from '@playwright/test';
import type { AppInstance } from './app-instance';

export async function waitForTasksIdle(app: AppInstance, newTaskAfter?: number) {
  await expect
    .poll(
      async () => {
        const tasks = await app.api.tasks();
        const fresh = tasks.filter(
          (task) => task.kind === 'index' && task.id > (newTaskAfter ?? -1),
        );
        const failed = fresh.find(
          (task) => task.state === 'failed' || task.state === 'interrupted',
        );
        if (failed) throw new Error(`Index task ${failed.id}: ${failed.error}`);
        return (
          (newTaskAfter === undefined || fresh.some((task) => task.state === 'done')) &&
          tasks.every((task) => task.state !== 'running' && task.state !== 'queued') &&
          (await app.api.indexStatus()).active_task_id === null
        );
      },
      {
        timeout: 120_000,
        message: 'This process must finish a new startup task, then become idle',
      },
    )
    .toBe(true);
}

export async function waitForFile(app: AppInstance, path: string) {
  await expect
    .poll(async () => {
      try {
        return (await stat(app.volumePath(path))).isFile();
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
        throw error;
      }
    })
    .toBe(true);
}

export async function waitForIndexed(app: AppInstance, path: string) {
  const [volume, ...parts] = path.split('/');
  const relative = parts.join('/');
  await expect
    .poll(async () =>
      (
        await app.api.listing(volume, dirname(relative) === '.' ? '' : dirname(relative))
      ).items.some((item) => item.name === basename(relative)),
    )
    .toBe(true);
}

export async function expectStaysFalse(
  condition: () => Promise<boolean>,
  options: { forMs: number },
) {
  const end = performance.now() + options.forMs;
  do {
    expect(await condition()).toBe(false);
    await setTimeout(Math.min(50, Math.max(0, end - performance.now())));
  } while (performance.now() < end);
}
