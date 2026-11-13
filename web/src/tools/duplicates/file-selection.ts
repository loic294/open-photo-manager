import type { DuplicateFile } from '@/api/generated/DuplicateFile';
import type { DuplicateFileGroup } from '@/api/generated/DuplicateFileGroup';
import type { Scope } from '@/api/generated/Scope';

export const RULE_LABELS = {
  capture: 'Capture date',
  modified: 'Modified date',
  created: 'Creation date',
  rating: 'Has ratings',
  description: 'Has description',
  flag: 'Has flags',
  size: 'File size',
  tags: 'Has tags',
  tag: 'Has specific tag',
  folder: 'Is in specific folder',
  subtree: 'Is in folder or subfolder',
} as const;

export type RuleKind = keyof typeof RULE_LABELS;

export function differingFields(group: DuplicateFileGroup) {
  return {
    name: new Set(group.files.map((file) => file.item.name)).size > 1,
    size: new Set(group.files.map((file) => file.item.size)).size > 1,
    capture_ts: new Set(group.files.map((file) => file.item.capture_ts)).size > 1,
  };
}

export interface SelectionRule {
  kind: RuleKind;
  enabled: boolean;
  direction: 'asc' | 'desc';
  tag: string;
  folders: Scope[];
}

export function defaultRules(): SelectionRule[] {
  return (Object.keys(RULE_LABELS) as RuleKind[]).map((kind) => ({
    kind,
    enabled: false,
    direction: 'asc',
    tag: '',
    folders: [],
  }));
}

export function inFolder(file: DuplicateFile, scope: Scope, recursive: boolean): boolean {
  return (
    (scope.volume_id === null || file.item.volume_id === scope.volume_id) &&
    (file.item.dir === scope.dir ||
      (recursive && (scope.dir === '' || file.item.dir.startsWith(`${scope.dir}/`))))
  );
}

function value(file: DuplicateFile, rule: SelectionRule): number | null {
  switch (rule.kind) {
    case 'capture':
      return file.item.capture_ts;
    case 'modified':
      return file.item.mtime;
    case 'created':
      return file.btime;
    case 'size':
      return file.item.size;
    case 'rating':
      return Number(file.item.rating > 0);
    case 'description':
      return Number(file.has_description);
    case 'flag':
      return Number(file.item.flag !== 0);
    case 'tags':
      return Number(file.tags.length > 0);
    case 'tag':
      return Number(file.tags.includes(rule.tag));
    case 'folder':
    case 'subtree':
      return Number(rule.folders.some((folder) => inFolder(file, folder, rule.kind === 'subtree')));
  }
}

export function compareFiles(
  a: DuplicateFile,
  b: DuplicateFile,
  rules: readonly SelectionRule[],
): number {
  for (const rule of rules) {
    if (!rule.enabled) continue;
    const av = value(a, rule);
    const bv = value(b, rule);
    if (av === bv) continue;
    if (av === null) return 1;
    if (bv === null) return -1;
    const ascending =
      ['capture', 'modified', 'created', 'size'].includes(rule.kind) && rule.direction === 'asc';
    const order = av < bv ? -1 : 1;
    return ascending ? order : -order;
  }
  return a.item.id - b.item.id;
}

/** Greedy direct-match coverage, not transitive deletion. A chain may need several keepers. */
export function selectKeepers(
  group: DuplicateFileGroup,
  rules: readonly SelectionRule[],
  preferred?: number,
): Set<number> {
  const neighbours = new Map<number, Set<number>>();
  for (const [a, b] of group.pairs) {
    if (!neighbours.has(a)) neighbours.set(a, new Set());
    if (!neighbours.has(b)) neighbours.set(b, new Set());
    neighbours.get(a)?.add(b);
    neighbours.get(b)?.add(a);
  }
  const ordered = [...group.files].sort((a, b) => {
    if (a.item.id === preferred) return -1;
    if (b.item.id === preferred) return 1;
    return compareFiles(a, b, rules);
  });
  const covered = new Set<number>();
  const keep = new Set<number>();
  for (const file of ordered) {
    const id = file.item.id;
    if (covered.has(id)) continue;
    keep.add(id);
    covered.add(id);
    for (const other of neighbours.get(id) ?? []) covered.add(other);
  }
  return keep;
}

export function parentFolders(file: DuplicateFile): Scope[] {
  const folders: Scope[] = [];
  let dir = file.item.dir;
  while (true) {
    folders.push({ volume_id: file.item.volume_id, dir, recursive: true });
    if (dir === '') break;
    dir = dir.includes('/') ? dir.slice(0, dir.lastIndexOf('/')) : '';
  }
  return folders;
}
