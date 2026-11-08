import { readFile } from 'node:fs/promises';
import { basename, dirname } from 'node:path';
import { expect, type Page } from '@playwright/test';
import type { ConflictPolicy } from '../../web/src/api/generated/ConflictPolicy';
import type { FileDetail } from '../../web/src/api/generated/FileDetail';
import type { MoveParams } from '../../web/src/api/generated/MoveParams';
import type { PlanDto } from '../../web/src/api/generated/PlanDto';
import type { TaskDto } from '../../web/src/api/generated/TaskDto';
import type { TrashBatchesResponse } from '../../web/src/api/generated/TrashBatchesResponse';
import type { TrashListResponse } from '../../web/src/api/generated/TrashListResponse';
import { emptyFilter } from './api';
import type { AppInstance } from './app-instance';
import { writeFile } from './disk';
import type { FixtureFile, Manifest } from './manifest';

export type MatchMode = 'hash' | 'name_and_time' | 'name_different_time' | 'name_prefix';

const MODE_NUMBER: Record<MatchMode, number> = {
  hash: 1,
  name_and_time: 2,
  name_different_time: 3,
  name_prefix: 4,
};

type DuplicateGroup = {
  source: { volume: string; path: string } | null;
  members: {
    volume: string;
    path: string;
    modes: number[];
    match: boolean;
    match_plain_prefix?: boolean;
  }[];
};

export function expectedMatches(
  manifest: Manifest,
  mode: MatchMode,
  sourceFiles: readonly FixtureFile[],
  options: { strictPrefix?: boolean } = {},
): FixtureFile[] {
  const number = MODE_NUMBER[mode];
  const sources = new Set(sourceFiles.map((file) => `${file.volume}/${file.path}`));
  const groups = manifest.duplicateGroups(number) as DuplicateGroup[];
  const matches = new Map<string, FixtureFile>();
  for (const group of groups) {
    if (!group.source || !sources.has(`${group.source.volume}/${group.source.path}`)) continue;
    for (const member of group.members) {
      const matched =
        (member.match && member.modes.includes(number)) ||
        (mode === 'name_prefix' &&
          options.strictPrefix === true &&
          member.match_plain_prefix === true);
      if (matched) {
        const file = manifest.file(member.volume, member.path);
        matches.set(`${file.volume}/${file.path}`, file);
      }
    }
  }
  return [...matches.values()];
}

function stem(fileName: string): string {
  const dot = fileName.lastIndexOf('.');
  return dot > 0 ? fileName.slice(0, dot) : fileName;
}

function dateParts(file: FixtureFile) {
  const value =
    file.capture_ts_expected ??
    (file.capture_src_expected === 'fs' && file.mtime !== null
      ? new Date(file.mtime * 1000).toISOString().slice(0, 19)
      : null);
  const date = value ? new Date(`${value}Z`) : new Date(0);
  return {
    YYYY: String(date.getUTCFullYear()),
    YY: String(date.getUTCFullYear()).slice(-2),
    mm: String(date.getUTCMonth() + 1).padStart(2, '0'),
    MMM: new Intl.DateTimeFormat('en-US', { month: 'short', timeZone: 'UTC' }).format(date),
    MMMM: new Intl.DateTimeFormat('en-US', { month: 'long', timeZone: 'UTC' }).format(date),
    dd: String(date.getUTCDate()).padStart(2, '0'),
    HH: String(date.getUTCHours()).padStart(2, '0'),
    min: String(date.getUTCMinutes()).padStart(2, '0'),
    sec: String(date.getUTCSeconds()).padStart(2, '0'),
  };
}

export function expectedDestination(file: FixtureFile, pattern: string): string {
  const parts = file.path.split('/');
  const filename = parts.at(-1) ?? file.path;
  const folder = parts.length > 1 ? (parts.at(-2) ?? '') : '';
  const extensionIndex = filename.lastIndexOf('.');
  const extension = extensionIndex > 0 ? filename.slice(extensionIndex + 1) : '';
  const tokens: Record<string, string> = {
    ...dateParts(file),
    filename,
    name: stem(filename),
    ext: extension,
    camera: file.camera_expected ?? '',
    folder,
  };
  const hasName = /\{(?:filename|name)\}/.test(pattern);
  const rendered = pattern.replace(/\{([^{}]+)\}/g, (_token, name: string) => {
    if (!(name in tokens)) throw new Error(`Unknown oracle token {${name}}`);
    return tokens[name];
  });
  const path = rendered.split('/').filter(Boolean).join('/');
  return hasName ? path : `${path}/${filename}`;
}

export function expectedEmptyDirs(
  manifest: Manifest,
  base: { volume: string; dir: string; recursive?: boolean },
): string[] {
  const below = manifest
    .emptyDirs()
    .filter(
      (entry) =>
        entry.volume === base.volume &&
        entry.path !== base.dir &&
        (base.dir === '' || entry.path.startsWith(`${base.dir}/`)),
    )
    .map((entry) => entry.path);
  if (base.recursive !== false) return below;
  const nonEmptyBranches = new Set(
    manifest.data.files
      .filter((file) => file.volume === base.volume && file.indexable && !file.skipped)
      .map((file) => {
        const relative = file.path.slice(base.dir ? base.dir.length + 1 : 0);
        return relative.split('/')[0] ?? '';
      }),
  );
  return below.filter((path) => {
    const relative = path.slice(base.dir ? base.dir.length + 1 : 0);
    return !nonEmptyBranches.has(relative.split('/')[0] ?? '');
  });
}

// testing-e2e-system: real API/disk setup shared by system task, watcher and trash specs.
export async function indexedFile(app: AppInstance, volume: string, path: string) {
  const listing = await app.api.listing(volume, dirname(path) === '.' ? '' : dirname(path));
  const item = listing.items.find((item) => item.name === basename(path));
  if (!item) throw new Error(`Not indexed: ${volume}/${path}`);
  return app.api.request<FileDetail>('GET', `/api/files/${item.id}`);
}

export async function terminalTask(app: AppInstance, id: number) {
  await expect
    .poll(
      async () => {
        const task = (await app.api.tasks()).find((task) => task.id === id);
        return task && !['queued', 'running'].includes(task.state);
      },
      { timeout: 120_000 },
    )
    .toBe(true);
  const task = (await app.api.tasks()).find((task) => task.id === id);
  if (!task) throw new Error(`Task ${id} disappeared`);
  return task;
}

export async function movePlan(
  app: AppInstance,
  source: { volume: string; dir: string },
  destination: { volume: string; dir: string },
  policy: ConflictPolicy | null = 'smart',
) {
  const volumes = await app.api.volumes();
  const id = (name: string) => {
    const volume = volumes.find((volume) => volume.name === name);
    if (!volume) throw new Error(`Unknown volume ${name}`);
    return volume.id;
  };
  const params: MoveParams = {
    source: {
      scope: { volume_id: id(source.volume), dir: source.dir, recursive: true },
      filter: emptyFilter,
    },
    destination: { volume_id: id(destination.volume), dir: destination.dir },
    move_type: 'flat',
    pattern: null,
    conflict_policy: policy,
    match_mode: null,
    strict_prefix: false,
  };
  return app.api.request<PlanDto>('POST', '/api/plans', { kind: 'move', params, replaces: null });
}

export function executePlan(app: AppInstance, plan: PlanDto, options: object = {}) {
  return app.api.request<TaskDto>('POST', `/api/plans/${plan.id}/execute`, { options, only: null });
}

/** Retain a byte-identical survivor for each owner before deleting through the duplicate planner. */
export async function trashFixtures(app: AppInstance, files: readonly FixtureFile[]) {
  if (files.length === 0) throw new Error('Trash setup needs at least one fixture');
  const first = files[0];
  if (
    files.some((file) => file.volume !== first.volume || dirname(file.path) !== dirname(first.path))
  )
    throw new Error('Trash setup requires one source folder');
  const targetVolume = first.volume === 'photos' ? 'archive' : 'photos';
  const survivorDir = `system-survivors-${basename(dirname(first.path))}`;
  for (const file of files)
    await writeFile(
      app,
      targetVolume,
      `${survivorDir}/${basename(file.path)}`,
      await readFile(app.volumePath(`${file.volume}/${file.path}`)),
    );
  await app.rescan();
  const volumes = await app.api.volumes();
  const sourceId = volumes.find((volume) => volume.name === first.volume)?.id;
  const comparisonId = volumes.find((volume) => volume.name === targetVolume)?.id;
  if (sourceId === undefined || comparisonId === undefined)
    throw new Error('Fixture volumes missing');
  const ids = await Promise.all(
    files.map(async (file) => (await indexedFile(app, file.volume, file.path)).item.id),
  );
  const selection = await app.api.request<{ id: number }>('POST', '/api/selections', {
    ids,
    label: 'system trash setup',
  });
  const plan = await app.api.request<PlanDto>('POST', '/api/plans', {
    kind: 'duplicates',
    replaces: null,
    params: {
      mode: 'hash',
      strict_prefix: false,
      source: {
        scope: { volume_id: sourceId, dir: dirname(first.path), recursive: false },
        filter: { ...emptyFilter, selection_id: selection.id },
      },
      comparison: {
        scope: { volume_id: comparisonId, dir: survivorDir, recursive: false },
        filter: emptyFilter,
      },
    },
  });
  const task = await executePlan(app, plan, { action: 'delete_source' });
  expect((await terminalTask(app, task.id)).state).toBe('done');
  return task;
}

export function trashList(app: AppInstance, query = '') {
  return app.api.request<TrashListResponse>(
    'GET',
    `/api/trash?limit=200${query ? `&${query}` : ''}`,
  );
}

export function trashBatches(app: AppInstance) {
  return app.api.request<TrashBatchesResponse>('GET', '/api/trash/batches?limit=200');
}

/** Observe the real UI submission before clicking, then wait for its specific task to settle. */
export async function taskAction(
  app: AppInstance,
  page: Page,
  path: string,
  action: () => Promise<void>,
) {
  const response = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === path && response.request().method() === 'POST',
  );
  await action();
  const submitted = await response;
  expect(submitted.ok()).toBe(true);
  const task = (await submitted.json()) as TaskDto;
  return terminalTask(app, task.id);
}
