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
