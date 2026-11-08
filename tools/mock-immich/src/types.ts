export type Visibility = 'archive' | 'timeline' | 'hidden' | 'locked';
export type AssetType = 'IMAGE' | 'VIDEO';

export const VISIBILITIES: readonly Visibility[] = ['archive', 'timeline', 'hidden', 'locked'];

export interface User {
  id: string;
  email: string;
  password: string;
  name: string;
  isAdmin: boolean;
  createdAt: number;
}

export interface Session {
  token: string;
  userId: string;
  createdAt: number;
}

export interface ApiKey {
  id: string;
  name: string;
  permissions: string[];
  /** Only the hash of the secret is kept, like Immich does. */
  secretHash: string;
  userId: string;
  createdAt: number;
  updatedAt: number;
}

export interface Library {
  id: string;
  name: string;
  ownerId: string;
  importPaths: string[];
  exclusionPatterns: string[];
  createdAt: number;
  updatedAt: number;
  refreshedAt: number | null;
}

/** All times are epoch milliseconds; they are written as ISO strings on the wire. */
export interface Asset {
  id: string;
  /** Creation order: breaks ties between equal capture times. */
  seq: number;
  libraryId: string;
  ownerId: string;
  originalPath: string;
  originalFileName: string;
  type: AssetType;
  checksum: string;
  fileCreatedAt: number;
  fileModifiedAt: number;
  localDateTime: number;
  createdAt: number;
  updatedAt: number;
  /** Set while the asset is in Immich's trash. */
  deletedAt: number | null;
  isOffline: boolean;
  isFavorite: boolean;
  /** 1 to 5, -1 rejected, null unrated (`exifInfo.rating`). */
  rating: number | null;
  description: string;
  visibility: Visibility;
  make: string | null;
  model: string | null;
  /** The file's modification time (ms) at the last scan, to notice a changed file. */
  mtimeMs: number | null;
}

export interface Album {
  id: string;
  name: string;
  ownerId: string;
  createdAt: number;
  updatedAt: number;
  assetIds: Set<string>;
}

export interface Tag {
  id: string;
  name: string;
  /** The full path of the tag (`Parent/Child`); `name` is its last part. */
  value: string;
  createdAt: number;
  updatedAt: number;
  assetIds: Set<string>;
}

export interface ScanSummary {
  libraryId: string;
  imported: number;
  updated: number;
  restored: number;
  offlined: number;
  unchanged: number;
  /** Files found under the import paths. */
  seen: number;
  /** Import paths that lie under no mount: not scanned. */
  unmounted: string[];
}

export type JobStatus = 'queued' | 'running' | 'completed' | 'failed' | 'cancelled';

export interface Job {
  id: string;
  kind: 'library-scan';
  libraryId: string;
  status: JobStatus;
  queuedAt: string;
  startedAt: string | null;
  finishedAt: string | null;
  summary: ScanSummary | null;
  error: string | null;
}

/** A call that would have damaged a real Immich library: see `violations` in the README. */
export interface Violation {
  kind: 'online-asset-force-deleted';
  assetId: string;
  path: string;
  libraryId: string;
  /** The original file was removed from disk (its path lay under a mount). */
  fileRemoved: boolean;
  at: string;
}

/** A directory of this machine that stands for a path of the Immich server (`/mnt/photos`). */
export interface Mount {
  /** Absolute, without a trailing slash. */
  prefix: string;
  /** Absolute and existing. */
  dir: string;
}

/** The mutable model; emptied and seeded again by a reset. */
export interface Data {
  users: Map<string, User>;
  sessions: Map<string, Session>;
  keys: Map<string, ApiKey>;
  libraries: Map<string, Library>;
  assets: Map<string, Asset>;
  assetsByPath: Map<string, Asset>;
  albums: Map<string, Album>;
  tags: Map<string, Tag>;
  jobs: Job[];
  violations: Violation[];
  nextSeq: number;
  /** Bumped by every reset, so a scan queued before it never touches the new data. */
  generation: number;
  /** Every asset by (capture time, seq): rebuilt when needed, see `search.ts`. */
  order: Asset[] | null;
}

export interface ServerVersion {
  major: number;
  minor: number;
  patch: number;
  /** As the server writes it in `/server/about` (`v3.4.0`, `v3.4.0-rc.0`). */
  text: string;
}

export type LegacyUnknownFields = 'reject' | 'ignore';

export interface ScenarioOptions {
  scanDelayMs?: number;
  latencyMs?: number;
  legacyUnknownFields?: LegacyUnknownFields;
  unknownAssetStatus?: 400 | 404;
}

export interface ScenarioUser {
  id: string;
  email: string;
  password: string;
  name: string;
  isAdmin: boolean;
}

export interface ScenarioLibrary {
  id: string;
  name: string;
  importPaths: string[];
  exclusionPatterns: string[];
  /** The email of the owner; the first administrator when absent. */
  owner?: string;
}

/** An asset to seed or to upsert (`POST /__mock/assets`); `library` is a name or an id. */
export interface AssetInput {
  path: string;
  library?: string;
  id?: string;
  type?: AssetType;
  fileCreatedAt?: string;
  fileModifiedAt?: string;
  isFavorite?: boolean;
  /** `null` clears the rating. */
  rating?: number | null;
  description?: string;
  visibility?: Visibility;
  isOffline?: boolean;
  trashed?: boolean;
  make?: string | null;
  model?: string | null;
  checksum?: string;
}

export interface GenerateInput {
  library: string;
  prefix: string;
  count: number;
  extension: string;
  startAt?: string;
  stepMs: number;
  /** How many consecutive assets share one capture time. */
  ties: number;
}

export interface ScenarioAlbum {
  id: string;
  name: string;
  /** Original paths of the member assets. */
  assets: string[];
}

export interface ScenarioTag {
  id: string;
  name: string;
  value: string;
}

export interface Scenario {
  description?: string;
  serverVersion?: string;
  options: ScenarioOptions;
  users: ScenarioUser[];
  libraries: ScenarioLibrary[];
  assets: AssetInput[];
  generate: GenerateInput[];
  albums: ScenarioAlbum[];
  tags: ScenarioTag[];
}
