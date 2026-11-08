import { assetState } from './assets.ts';
import {
  type FailRuleInput,
  type FailRuleView,
  type GateView,
  type HeldView,
  parseFailRule,
} from './faults.ts';
import { ApiError, badRequest } from './http.ts';
import type { MockContext, Settings } from './mock.ts';
import type { LogEntry } from './requestlog.ts';
import { libraryView } from './routes/libraries.ts';
import { queueScan } from './scan.ts';
import { readAssetInput, readGenerateInput } from './scenario.ts';
import { type Generated, generateAssets, upsertAsset } from './seeding.ts';
import { resetState } from './state.ts';
import type { AssetInput, Job, Library, Mount, Violation, Visibility } from './types.ts';
import { iso, type Obj } from './util.ts';
import { createChecker } from './validate.ts';

/** The fields of `POST /__mock/assets`, with the same meaning. */
export type AssetOptions = AssetInput;

/** The fields of `POST /__mock/generate`, with the same defaults. */
export interface GenerateOptions {
  /** The library, by id or by name; give this or its alias `library`. */
  libraryId?: string;
  library?: string;
  /** The path of the first asset without its number and extension: `/mnt/photos/gen/IMG_`. */
  prefix: string;
  count: number;
  extension?: string;
  startAt?: string;
  stepMs?: number;
  ties?: number;
}

export interface AddedAssets {
  created: number;
  updated: number;
  items: Array<{ id: string; path: string; created: boolean }>;
}

/** A row read from the disposable Immich database, not an instruction to write to it. */
export interface BackingAssetPath {
  id: string;
  ownerId: string;
  libraryId: string;
  originalPath: string;
  originalFileName: string;
  checksum: string;
  updatedAt: string;
  isOffline: boolean;
  isFavorite: boolean;
  rating: number | null;
  description: string;
  visibility: Visibility;
  deletedAt: string | null;
}

export interface BackingAssociation {
  id: string;
  name: string;
  assetIds: string[];
}

export interface BackingAssociations {
  albums: BackingAssociation[];
  tags: BackingAssociation[];
}

export interface StateQuery {
  /** Keeps the assets whose original path contains this text. */
  path?: string;
  limit?: number;
  offset?: number;
}

export interface UserView {
  id: string;
  email: string;
  name: string;
  isAdmin: boolean;
}

export interface AlbumView {
  id: string;
  name: string;
  ownerId: string;
  /** The ids of the assets in the album. */
  assetIds: string[];
}

export interface TagView {
  id: string;
  /** The last part of the tag path. */
  name: string;
  /** The full tag path (`Family/Birthdays`). */
  value: string;
  assetIds: string[];
}

/** An API key as the state shows it: the secret is never kept, only its hash. */
export interface KeyView {
  id: string;
  name: string;
  permissions: string[];
  userId: string;
  createdAt: string;
}

/** What `GET /__mock/state` answers (and `state.snapshot()` returns). */
export interface StateSnapshot {
  settings: Settings;
  /** The version as the server reports it, with its `v` (`v3.4.0`). */
  serverVersion: string;
  description: string | null;
  mounts: Mount[];
  users: UserView[];
  libraries: Obj[];
  /** One page of the assets (`StateQuery`), `total` counting every match. */
  assets: { total: number; offset: number; limit: number; items: Obj[] };
  albums: AlbumView[];
  tags: TagView[];
  keys: KeyView[];
  jobs: Job[];
  violations: Violation[];
  failures: FailRuleView[];
  gates: GateView[];
  held: HeldView[];
}

/** Reads the model without going through the HTTP API (and without leaving a trace in the log). */
export interface MockState {
  assets(): Obj[];
  assetByPath(path: string): Obj | undefined;
  albumsOf(assetId: string): Array<{ id: string; name: string }>;
  /** Forced deletes of online assets: the application must never cause one. */
  violations(): Violation[];
  requests(since?: number): { entries: LogEntry[]; next: number };
  users(): UserView[];
  libraries(): Obj[];
  albums(): AlbumView[];
  tags(): TagView[];
  keys(): KeyView[];
  jobs(): Job[];
  /** What `GET /__mock/state` answers. */
  snapshot(query?: StateQuery): StateSnapshot;
}

/** Changes the mock while it runs. The backing-state seam is programmatic only. */
export interface MockControl {
  /** Scans one library (id or name) or all of them now; settles when every scan is over. */
  scan(library?: string): Promise<Job[]>;
  fail(rule: FailRuleInput): FailRuleView;
  failures(): FailRuleView[];
  clearFailures(): void;
  /** Deletes the API keys (all, or those named `name`): the next call gets a 401. */
  revokeKey(name?: string): number;
  /** Seeds the scenario again; clears sessions, keys, failure rules, gates, latency and the log. */
  reset(): void;
  /** Holds the requests that match until `releaseGate()` (the first `times` ones when given). */
  gate(match: string, options?: { times?: number }): GateView;
  /** Lets the held requests go and removes the gates; returns how many were held. */
  releaseGate(): number;
  waitForHeld(count: number, timeoutMs?: number): Promise<HeldView[]>;
  /** Delays every `/api` request. */
  latency(ms: number): void;
  generate(options: GenerateOptions): Generated;
  /** Creates the assets, or changes the fields given of the ones that exist (matched by path). */
  addAssets(inputs: readonly AssetOptions[]): AddedAssets;
  /** Atomically reflect a complete database snapshot for all currently modelled assets. */
  reconcileAssetPaths(rows: readonly BackingAssetPath[], associations?: BackingAssociations): void;
}

const DEFAULT_LIMIT = 100;
export const MAX_LIMIT = 5000;
const DEFAULT_HELD_TIMEOUT_MS = 10_000;

function librariesFor(ctx: MockContext, ref: string | undefined): Library[] {
  const all = [...ctx.data.libraries.values()];
  if (ref === undefined) {
    return all;
  }
  const found = all.filter((library) => library.id === ref || library.name === ref);
  if (found.length === 0) {
    throw badRequest(`unknown library ${JSON.stringify(ref)}`);
  }
  return found;
}

export function createState(ctx: MockContext): MockState {
  const { data } = ctx;
  const state: MockState = {
    assets: () => [...data.assets.values()].map(assetState),
    assetByPath(path) {
      const asset = data.assetsByPath.get(path);
      return asset === undefined ? undefined : assetState(asset);
    },
    albumsOf: (assetId) =>
      [...data.albums.values()]
        .filter((album) => album.assetIds.has(assetId))
        .map((album) => ({ id: album.id, name: album.name })),
    violations: () => data.violations.map((violation) => ({ ...violation })),
    requests: (since) => ctx.log.since(since),
    users: () =>
      [...data.users.values()].map(({ id, email, name, isAdmin }) => ({
        id,
        email,
        name,
        isAdmin,
      })),
    libraries: () => [...data.libraries.values()].map((library) => libraryView(ctx, library)),
    albums: () =>
      [...data.albums.values()].map((album) => ({
        id: album.id,
        name: album.name,
        ownerId: album.ownerId,
        assetIds: [...album.assetIds],
      })),
    tags: () =>
      [...data.tags.values()].map((tag) => ({
        id: tag.id,
        name: tag.name,
        value: tag.value,
        assetIds: [...tag.assetIds],
      })),
    keys: () =>
      [...data.keys.values()].map((key) => ({
        id: key.id,
        name: key.name,
        permissions: key.permissions,
        userId: key.userId,
        createdAt: iso(key.createdAt),
      })),
    jobs: () => data.jobs.map((job) => structuredClone(job)),
    snapshot(query = {}) {
      const limit = query.limit ?? DEFAULT_LIMIT;
      const offset = query.offset ?? 0;
      const text = query.path ?? '';
      const matching = [...data.assets.values()].filter((asset) =>
        asset.originalPath.includes(text),
      );
      return {
        settings: { ...ctx.settings },
        serverVersion: ctx.version.text,
        description: ctx.scenario.description ?? null,
        mounts: ctx.mounts.map((mount) => ({ ...mount })),
        users: state.users(),
        libraries: state.libraries(),
        assets: {
          total: matching.length,
          offset,
          limit,
          items: matching.slice(offset, offset + limit).map(assetState),
        },
        albums: state.albums(),
        tags: state.tags(),
        keys: state.keys(),
        jobs: state.jobs(),
        violations: state.violations(),
        failures: ctx.faults.rules(),
        gates: ctx.faults.gates(),
        held: ctx.faults.held(),
      };
    },
  };
  return state;
}

/** Validates `raw` like `POST /__mock/generate` does, then adds the assets. */
export function runGenerate(ctx: MockContext, raw: unknown): Generated {
  const c = createChecker(raw);
  const input = readGenerateInput(c);
  c.done();
  if (input === undefined) {
    throw badRequest('invalid generate options');
  }
  return generateAssets(ctx, input);
}

/** Validates every entry like `POST /__mock/assets` does, then upserts them in order. */
export function runAddAssets(ctx: MockContext, raw: unknown): AddedAssets {
  if (!Array.isArray(raw)) {
    throw badRequest('assets must be an array');
  }
  const errors: string[] = [];
  const parsed: AssetInput[] = [];
  raw.forEach((item, index) => {
    const input = readAssetInput(createChecker(item, errors, `assets[${index}].`));
    if (input !== undefined) {
      parsed.push(input);
    }
  });
  if (errors.length > 0) {
    throw new ApiError(400, errors);
  }
  const items = parsed.map((input) => {
    const { asset, created } = upsertAsset(ctx, input);
    return { id: asset.id, path: asset.originalPath, created };
  });
  const created = items.filter((item) => item.created).length;
  return { created, updated: items.length - created, items };
}

function reconcileAssetPaths(
  ctx: MockContext,
  rows: readonly BackingAssetPath[],
  associations?: BackingAssociations,
): void {
  const { data } = ctx;
  if (rows.length !== data.assets.size) {
    throw new Error('backing snapshot must contain every mock asset exactly once');
  }
  const ids = new Set<string>();
  const paths = new Set<string>();
  for (const row of rows) {
    const asset = data.assets.get(row.id);
    if (
      !asset ||
      ids.has(row.id) ||
      asset.ownerId !== row.ownerId ||
      asset.libraryId !== row.libraryId
    ) {
      throw new Error(`backing asset identity mismatch: ${row.id}`);
    }
    if (
      !row.originalPath.startsWith('/') ||
      row.originalPath.endsWith('/') ||
      row.originalFileName !== row.originalPath.slice(row.originalPath.lastIndexOf('/') + 1) ||
      !row.checksum ||
      !Number.isFinite(Date.parse(row.updatedAt)) ||
      typeof row.isOffline !== 'boolean' ||
      typeof row.isFavorite !== 'boolean' ||
      (row.rating !== null &&
        (!Number.isInteger(row.rating) || row.rating < -1 || row.rating > 5)) ||
      typeof row.description !== 'string' ||
      !['archive', 'timeline', 'hidden', 'locked'].includes(row.visibility) ||
      (row.deletedAt !== null &&
        (typeof row.deletedAt !== 'string' || !Number.isFinite(Date.parse(row.deletedAt)))) ||
      paths.has(row.originalPath)
    ) {
      throw new Error(`invalid or duplicate backing asset path: ${row.originalPath}`);
    }
    ids.add(row.id);
    paths.add(row.originalPath);
  }

  if (associations) {
    for (const [kind, expected, actual] of [
      ['album', data.albums, associations.albums],
      ['tag', data.tags, associations.tags],
    ] as const) {
      if (actual.length !== expected.size) {
        throw new Error(`backing snapshot must contain every mock ${kind}`);
      }
      const seen = new Set<string>();
      for (const row of actual) {
        const entry = expected.get(row.id);
        const name = kind === 'tag' ? data.tags.get(row.id)?.value : entry?.name;
        if (
          !entry ||
          seen.has(row.id) ||
          row.name !== name ||
          !Array.isArray(row.assetIds) ||
          new Set(row.assetIds).size !== row.assetIds.length ||
          row.assetIds.some((id) => !ids.has(id))
        ) {
          throw new Error(`backing ${kind} mismatch: ${row.id}`);
        }
        seen.add(row.id);
      }
    }
  }

  data.assetsByPath.clear();
  for (const row of rows) {
    const asset = data.assets.get(row.id);
    if (!asset) throw new Error(`backing asset disappeared: ${row.id}`);
    asset.originalPath = row.originalPath;
    asset.originalFileName = row.originalFileName;
    asset.checksum = row.checksum;
    asset.updatedAt = Date.parse(row.updatedAt);
    asset.isOffline = row.isOffline;
    asset.isFavorite = row.isFavorite;
    asset.rating = row.rating;
    asset.description = row.description;
    asset.visibility = row.visibility;
    asset.deletedAt = row.deletedAt === null ? null : Date.parse(row.deletedAt);
    data.assetsByPath.set(row.originalPath, asset);
  }
  if (associations) {
    for (const row of associations.albums) {
      const album = data.albums.get(row.id);
      if (!album) throw new Error(`backing album disappeared: ${row.id}`);
      album.assetIds = new Set(row.assetIds);
    }
    for (const row of associations.tags) {
      const tag = data.tags.get(row.id);
      if (!tag) throw new Error(`backing tag disappeared: ${row.id}`);
      tag.assetIds = new Set(row.assetIds);
    }
  }
}

export function createControl(ctx: MockContext): MockControl {
  const requireWritable = () => {
    if (ctx.readOnlyBacking?.()) {
      throw new Error('backing store does not support mock mutations');
    }
  };
  return {
    async scan(ref) {
      if (ctx.readOnlyBacking?.() && !ctx.beforeScanCommit) requireWritable();
      const queued = librariesFor(ctx, ref).map((library) => queueScan(ctx, library.id, 0));
      await Promise.all(queued.map(({ done }) => done));
      return queued.map(({ job }) => structuredClone(job));
    },
    fail: (rule) => ctx.faults.addRule(parseFailRule(rule)),
    failures: () => ctx.faults.rules(),
    clearFailures: () => ctx.faults.clearRules(),
    revokeKey(name) {
      let revoked = 0;
      for (const [id, key] of ctx.data.keys) {
        if (name === undefined || key.name === name) {
          ctx.data.keys.delete(id);
          revoked += 1;
        }
      }
      return revoked;
    },
    reset: () => {
      requireWritable();
      resetState(ctx);
    },
    gate(match, options = {}) {
      const c = createChecker({ times: options.times });
      const times = c.integer('times', { min: 1 });
      c.done();
      return ctx.faults.addGate(match, times);
    },
    releaseGate: () => ctx.faults.releaseGates(),
    waitForHeld: (count, timeoutMs = DEFAULT_HELD_TIMEOUT_MS) =>
      ctx.faults.waitForHeld(count, timeoutMs),
    latency(ms) {
      const c = createChecker({ ms });
      const value = c.integer('ms', { required: true, min: 0 });
      c.done();
      ctx.settings.latencyMs = value ?? 0;
    },
    generate: (options) => {
      requireWritable();
      return runGenerate(ctx, options);
    },
    addAssets: (inputs) => {
      requireWritable();
      return runAddAssets(ctx, inputs);
    },
    reconcileAssetPaths: (rows, associations) => reconcileAssetPaths(ctx, rows, associations),
  };
}
