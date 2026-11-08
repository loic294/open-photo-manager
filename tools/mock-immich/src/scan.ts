import { randomUUID } from 'node:crypto';
import type { Dirent } from 'node:fs';
import { readdir, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { addAsset, isMediaFile, setCaptureDate } from './assets.ts';
import { compileExclusions, isExcluded } from './glob.ts';
import { type MockContext, withBackingLock } from './mock.ts';
import { toRealPath } from './mounts.ts';
import type { Job, Library, ScanSummary } from './types.ts';
import { isUnder, sleep, trimSlash } from './util.ts';

const MAX_JOBS = 200;

interface Found {
  /** Path of the Immich server to the modification time (ms) of the file behind it. */
  files: Map<string, number>;
  /** Import paths whose directory was reachable through a mount. */
  scanned: string[];
  /** Import paths no mount covers: not looked at, their assets are left alone. */
  unmounted: string[];
}

const errorCode = (error: unknown): string | undefined =>
  typeof error === 'object' && error !== null && 'code' in error && typeof error.code === 'string'
    ? error.code
    : undefined;

/** The path is gone, or is not a directory any more. */
const isMissing = (error: unknown): boolean => {
  const code = errorCode(error);
  return code === 'ENOENT' || code === 'ENOTDIR';
};

/** The process may not look at the path: a directory it cannot list, a link into one. */
const isDenied = (error: unknown): boolean => {
  const code = errorCode(error);
  return code === 'EACCES' || code === 'EPERM';
};

/**
 * Walks the directories like Immich's crawler: hidden entries and non-media files are skipped.
 * A directory below the import path that cannot be listed is skipped, so one locked folder does
 * not fail the scan of the whole library; the import path itself must be listable.
 */
async function walk(
  realDir: string,
  immichDir: string,
  exclusions: readonly RegExp[],
  out: Map<string, number>,
  isRoot = false,
): Promise<void> {
  let entries: Dirent[];
  try {
    entries = await readdir(realDir, { withFileTypes: true });
  } catch (error) {
    if (isMissing(error) || (!isRoot && isDenied(error))) {
      return;
    }
    throw error;
  }
  entries.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  for (const entry of entries) {
    if (entry.name.startsWith('.')) {
      continue;
    }
    const immichPath = `${immichDir === '/' ? '' : immichDir}/${entry.name}`;
    const realPath = join(realDir, entry.name);
    if (entry.isDirectory()) {
      if (!isExcluded(exclusions, immichPath)) {
        await walk(realPath, immichPath, exclusions, out);
      }
      continue;
    }
    if (!isMediaFile(entry.name) || isExcluded(exclusions, immichPath)) {
      continue;
    }
    // A symlink to a file is followed; a symlink to a directory is not. A link that dangles, loops
    // or leads somewhere unreadable is not a file.
    if (!entry.isFile() && !entry.isSymbolicLink()) {
      continue;
    }
    try {
      const info = await stat(realPath);
      if (info.isFile()) {
        out.set(immichPath, Math.floor(info.mtimeMs));
      }
    } catch (error) {
      if (!isMissing(error) && !isDenied(error) && errorCode(error) !== 'ELOOP') {
        throw error;
      }
    }
  }
}

async function discover(ctx: MockContext, library: Library): Promise<Found> {
  const exclusions = compileExclusions(library.exclusionPatterns);
  const found: Found = { files: new Map(), scanned: [], unmounted: [] };
  for (const root of library.importPaths) {
    const base = trimSlash(root);
    const realRoot = toRealPath(ctx.mounts, base);
    if (realRoot === undefined) {
      found.unmounted.push(root);
      continue;
    }
    found.scanned.push(root);
    if (!isExcluded(exclusions, base)) {
      await walk(realRoot, base, exclusions, found.files, true);
    }
  }
  return found;
}

/** Applies what the walk found, in one synchronous step so no request sees a half-done scan. */
function apply(ctx: MockContext, library: Library, found: Found): ScanSummary {
  const { data } = ctx;
  const summary: ScanSummary = {
    libraryId: library.id,
    imported: 0,
    updated: 0,
    restored: 0,
    offlined: 0,
    unchanged: 0,
    seen: found.files.size,
    unmounted: found.unmounted,
  };
  const tick = ctx.clock.now();

  for (const [path, mtimeMs] of found.files) {
    const existing = data.assetsByPath.get(path);
    if (existing === undefined) {
      addAsset(ctx, {
        libraryId: library.id,
        ownerId: library.ownerId,
        originalPath: path,
        fileCreatedAt: mtimeMs,
        fileModifiedAt: mtimeMs,
        mtimeMs,
        now: tick,
      });
      summary.imported += 1;
      continue;
    }
    if (existing.libraryId !== library.id) {
      continue;
    }
    const changed = existing.mtimeMs !== null && existing.mtimeMs !== mtimeMs;
    if (changed) {
      existing.fileModifiedAt = mtimeMs;
      setCaptureDate(ctx, existing, mtimeMs);
    }
    existing.mtimeMs = mtimeMs;
    if (existing.isOffline) {
      existing.isOffline = false;
      existing.deletedAt = null;
      existing.updatedAt = tick;
      summary.restored += 1;
    } else if (changed) {
      existing.updatedAt = tick;
      summary.updated += 1;
    } else {
      summary.unchanged += 1;
    }
  }

  // An asset the walk did not find is offline and trashed, like Immich does, unless its import
  // path could not be scanned.
  const looked = (path: string): boolean => {
    const roots = library.importPaths.filter((root) => isUnder(path, root));
    return roots.length === 0 || roots.some((root) => found.scanned.includes(root));
  };
  for (const asset of data.assets.values()) {
    if (
      asset.libraryId === library.id &&
      !asset.isOffline &&
      !found.files.has(asset.originalPath) &&
      looked(asset.originalPath)
    ) {
      asset.isOffline = true;
      asset.deletedAt ??= tick;
      asset.updatedAt = tick;
      summary.offlined += 1;
    }
  }
  library.refreshedAt = ctx.clock.now();
  return summary;
}

async function scanLibrary(ctx: MockContext, libraryId: string, generation: number) {
  const library = ctx.data.libraries.get(libraryId);
  if (library === undefined) {
    throw new Error(`library ${libraryId} is gone`);
  }
  const found = await discover(ctx, library);
  if (ctx.data.generation !== generation) {
    return null;
  }
  if (!ctx.readOnlyBacking?.()) return apply(ctx, library, found);
  return withBackingLock(ctx, async () => {
    if (ctx.data.generation !== generation || ctx.stopSignal.aborted) return null;
    if (!ctx.beforeScanCommit) throw new Error('backing store does not support mock mutations');
    await ctx.beforeAssetRead?.();
    const data = structuredClone(ctx.data);
    const draft: MockContext = { ...ctx, data };
    const draftLibrary = data.libraries.get(libraryId);
    if (!draftLibrary) throw new Error(`library ${libraryId} is gone`);
    const before = [...ctx.data.assets.values()]
      .filter((asset) => asset.libraryId === libraryId)
      .map((asset) => ({ ...asset }));
    const summary = apply(draft, draftLibrary, found);
    const after = [...data.assets.values()]
      .filter((asset) => asset.libraryId === libraryId)
      .map((asset) => ({ ...asset }));
    await ctx.beforeScanCommit({ libraryId, before, after });
    ctx.data.assets = data.assets;
    ctx.data.assetsByPath = data.assetsByPath;
    ctx.data.nextSeq = data.nextSeq;
    ctx.data.order = data.order;
    library.refreshedAt = draftLibrary.refreshedAt;
    return summary;
  });
}

/**
 * Queues a library scan behind the earlier ones of the same library. After `delayMs` it looks at
 * the disk (at that moment, not at the time of the request), then settles. `done` never rejects:
 * the outcome is on the job.
 */
export function queueScan(
  ctx: MockContext,
  libraryId: string,
  delayMs: number,
): { job: Job; done: Promise<void> } {
  const { data } = ctx;
  const job: Job = {
    id: randomUUID(),
    kind: 'library-scan',
    libraryId,
    status: 'queued',
    queuedAt: new Date().toISOString(),
    startedAt: null,
    finishedAt: null,
    summary: null,
    error: null,
  };
  data.jobs.push(job);
  if (data.jobs.length > MAX_JOBS) {
    data.jobs.splice(0, data.jobs.length - MAX_JOBS);
  }
  const generation = data.generation;
  const stale = () => data.generation !== generation || ctx.stopSignal.aborted;
  const finish = (status: Job['status']) => {
    job.status = status;
    job.finishedAt = new Date().toISOString();
  };

  const previous = ctx.scans.get(libraryId) ?? Promise.resolve();
  const done = previous.then(async () => {
    if (stale()) {
      finish('cancelled');
      return;
    }
    job.status = 'running';
    job.startedAt = new Date().toISOString();
    try {
      if (delayMs > 0) {
        await sleep(delayMs, ctx.stopSignal);
      }
      const summary = stale() ? null : await scanLibrary(ctx, libraryId, generation);
      job.summary = summary;
      finish(summary === null ? 'cancelled' : 'completed');
    } catch (error) {
      if (ctx.stopSignal.aborted) {
        finish('cancelled');
      } else {
        job.error = error instanceof Error ? error.message : String(error);
        finish('failed');
      }
    }
  });
  ctx.scans.set(libraryId, done);
  return { job, done };
}
