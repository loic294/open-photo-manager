import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

export interface FixturePath {
  volume: string;
  path: string;
}
export interface FixtureFile extends FixturePath {
  type: 'file' | 'symlink';
  kind: 'image' | 'raw' | 'video' | 'sidecar' | 'other' | 'junk';
  scenario: string;
  indexable: boolean;
  skipped: boolean;
  skip_reason: string | null;
  size: number | null;
  sha1: string | null;
  mtime: number | null;
  capture_ts_expected: string | null;
  capture_src_expected: string | null;
  camera_expected: string | null;
  rating_expected: number | null;
  flag_expected: 'selected' | 'rejected' | 'none' | null;
  tags_expected: string[] | null;
  width: number | null;
  height: number | null;
  sidecars: { path: string; link: 'exact' | 'stem' }[];
  unreadable: 'file' | 'dir' | null;
  capture_offset_min_expected: number | null;
}
interface Scenario {
  description: string;
  skipped: boolean;
  skip_reason: string | null;
}
interface DuplicateGroup {
  id: string;
  source: FixturePath | null;
  members: (FixturePath & {
    role: string;
    modes: number[];
    match: boolean;
    match_plain_prefix?: boolean;
  })[];
}
interface ManifestData {
  schema_version: number;
  volumes: string[];
  files: FixtureFile[];
  scenarios: Record<string, Scenario>;
  duplicate_groups: Record<string, DuplicateGroup[]>;
  empty_dirs: (FixturePath & { scenario: string })[];
}

export class Manifest {
  constructor(readonly data: ManifestData) {
    if (data.schema_version !== 1 || !Array.isArray(data.files) || !data.scenarios)
      throw new Error('Unsupported fixture manifest');
  }
  static async load(outDir: string) {
    return new Manifest(JSON.parse(await readFile(join(outDir, 'manifest.json'), 'utf8')));
  }
  filesOf(key: string) {
    this.scenario(key);
    return this.data.files.filter((file) => file.scenario === key && !file.skipped);
  }
  file(volume: string, path: string) {
    const file = this.data.files.find((file) => file.volume === volume && file.path === path);
    if (!file) throw new Error(`Missing fixture ${volume}/${path}`);
    return file;
  }
  scenario(key: string) {
    const scenario = this.data.scenarios[key];
    if (!scenario) throw new Error(`Unknown fixture scenario ${key}`);
    return scenario;
  }
  duplicateGroups(mode: number | 'multi' | 'none') {
    const groups = this.data.duplicate_groups[typeof mode === 'number' ? `mode${mode}` : mode];
    if (!groups) throw new Error(`Unknown duplicate mode ${mode}`);
    return groups;
  }
  emptyDirs() {
    return this.data.empty_dirs;
  }
}
