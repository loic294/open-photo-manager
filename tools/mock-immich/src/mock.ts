import type { Faults } from './faults.ts';
import type { RequestLog } from './requestlog.ts';
import type { Asset, Data, LegacyUnknownFields, Mount, Scenario, ServerVersion } from './types.ts';
import type { Clock } from './util.ts';

export type BackingDeleteAsset = Readonly<
  Pick<Asset, 'id' | 'ownerId' | 'libraryId' | 'originalPath'>
>;

export interface BackingScan {
  libraryId: string;
  before: readonly Readonly<Asset>[];
  after: readonly Readonly<Asset>[];
}

export function withBackingLock<T>(ctx: MockContext, run: () => Promise<T>): Promise<T> {
  const operation = ctx.pendingBacking.then(run);
  ctx.pendingBacking = operation.then(
    () => undefined,
    () => undefined,
  );
  return operation;
}

/** What a scenario's `options` or the programmatic options turn into; some change at runtime. */
export interface Settings {
  /** How long a library scan waits before it looks at the disk. */
  scanDelayMs: number;
  /** Added to every `/api` request. */
  latencyMs: number;
  /** How a server older than 3.2 treats the typed search keys. */
  legacyUnknownFields: LegacyUnknownFields;
  /** The status of `GET /assets/{id}` for an unknown asset. */
  unknownAssetStatus: 400 | 404;
}

/** Everything the handlers share. Built once by `startMockImmich`. */
export interface MockContext {
  data: Data;
  clock: Clock;
  scenario: Scenario;
  version: ServerVersion;
  settings: Settings;
  /** The settings at start-up; a reset goes back to them. */
  baseSettings: Readonly<Settings>;
  mounts: Mount[];
  faults: Faults;
  log: RequestLog;
  /** The scan queue of each library: scans of one library never overlap. */
  scans: Map<string, Promise<void>>;
  /** Aborted by `stop()`: cuts the waits of scans. */
  stopSignal: AbortSignal;
  beforeAssetRead?: () => Promise<void>;
  readOnlyBacking?: () => boolean;
  beforeAssetDelete?: (
    assets: readonly BackingDeleteAsset[],
    force: boolean,
    now: number,
  ) => Promise<void>;
  beforeScanCommit?: (scan: BackingScan) => Promise<void>;
  pendingBacking: Promise<void>;
}
