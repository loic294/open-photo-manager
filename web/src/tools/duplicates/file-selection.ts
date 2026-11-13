import type { DuplicateFile } from '@/api/generated/DuplicateFile';
import type { DuplicateFileGroup } from '@/api/generated/DuplicateFileGroup';
import type { Scope } from '@/api/generated/Scope';

export const RULE_LABELS = {
  folder: 'Is in specific folder',
  subtree: 'Is in folder or subfolder',
  regex: 'Name matches regex',
  name_length: 'Name length',
  capture: 'Capture date',
  modified: 'Modified date',
  created: 'Creation date',
  rating: 'Has ratings',
  description: 'Has description',
  flag: 'Has flags',
  size: 'File size',
  tags: 'Has tags',
  tag: 'Has specific tag',
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
  pattern: string;
  folders: Scope[];
}

export function defaultRules(): SelectionRule[] {
  return (Object.keys(RULE_LABELS) as RuleKind[]).map((kind) => ({
    kind,
    enabled: false,
    direction: 'asc',
    tag: '',
    pattern: '',
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

export function regexIssue(pattern: string): string | null {
  if (!pattern) return 'Enter a filename regular expression.';
  try {
    new RegExp(pattern);
    return null;
  } catch (error) {
    return error instanceof Error ? error.message : 'Invalid regular expression.';
  }
}

function value(file: DuplicateFile, rule: SelectionRule, regex?: RegExp): number | null {
  switch (rule.kind) {
    case 'regex':
      return Number(regex?.test(file.item.name));
    case 'name_length':
      return [...file.item.name].length;
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
  return comparison(rules)(a, b).order;
}

function comparison(rules: readonly SelectionRule[]) {
  const enabled = rules.filter((rule) => rule.enabled);
  const patterns = new Map(
    enabled.filter((rule) => rule.kind === 'regex').map((rule) => [rule, new RegExp(rule.pattern)]),
  );
  return (a: DuplicateFile, b: DuplicateFile): { order: number; reason: string } => {
    for (const rule of enabled) {
      const av = value(a, rule, patterns.get(rule));
      const bv = value(b, rule, patterns.get(rule));
      if (av === bv) continue;
      if (av === null) return { order: 1, reason: `${RULE_LABELS[rule.kind]}: known dates first` };
      if (bv === null) return { order: -1, reason: `${RULE_LABELS[rule.kind]}: known dates first` };
      const ascending =
        ['capture', 'modified', 'created', 'size', 'name_length'].includes(rule.kind) &&
        rule.direction === 'asc';
      const order = av < bv ? -1 : 1;
      const detail =
        rule.kind === 'regex'
          ? `: /${rule.pattern}/`
          : rule.kind === 'name_length'
            ? `: ${ascending ? 'shorter' : 'longer'} first`
            : ['capture', 'modified', 'created', 'size'].includes(rule.kind)
              ? `: ${ascending ? 'ascending' : 'descending'}`
              : rule.kind === 'tag'
                ? `: ${rule.tag}`
                : '';
      return { order: ascending ? order : -order, reason: `${RULE_LABELS[rule.kind]}${detail}` };
    }
    return { order: a.item.id - b.item.id, reason: 'Rules tied: lowest file ID' };
  };
}

/** Greedy direct-match coverage, not transitive deletion. A chain may need several keepers. */
export function selectKeepers(
  group: DuplicateFileGroup,
  rules: readonly SelectionRule[],
  preferred?: number,
): Set<number> {
  return selectWithReasons(group, rules, preferred).keep;
}

export function selectWithReasons(
  group: DuplicateFileGroup,
  rules: readonly SelectionRule[],
  preferred?: number,
): { keep: Set<number>; reasons: Map<number, string> } {
  const neighbours = new Map<number, Set<number>>();
  for (const [a, b] of group.pairs) {
    if (!neighbours.has(a)) neighbours.set(a, new Set());
    if (!neighbours.has(b)) neighbours.set(b, new Set());
    neighbours.get(a)?.add(b);
    neighbours.get(b)?.add(a);
  }
  const compare = comparison(rules);
  const ordered = [...group.files].sort((a, b) => {
    if (a.item.id === preferred) return -1;
    if (b.item.id === preferred) return 1;
    return compare(a, b).order;
  });
  const covered = new Set<number>();
  const positions = new Map(ordered.map((file, index) => [file.item.id, index]));
  const keep = new Set<number>();
  const reasons = new Map<number, string>();
  for (const file of ordered) {
    const id = file.item.id;
    if (covered.has(id)) continue;
    let other: DuplicateFile | undefined;
    let position = ordered.length;
    for (const neighbour of neighbours.get(id) ?? []) {
      const index = positions.get(neighbour);
      if (!covered.has(neighbour) && index !== undefined && index < position) {
        position = index;
        other = ordered[index];
      }
    }
    const reason =
      id === preferred
        ? 'Manual selection'
        : other
          ? compare(file, other).reason
          : 'Additional keeper required for direct-match coverage';
    reasons.set(id, `Keep: ${reason}`);
    keep.add(id);
    covered.add(id);
    for (const other of neighbours.get(id) ?? []) {
      if (!covered.has(other)) {
        covered.add(other);
        reasons.set(other, `Remove: directly matches ${file.item.name}. Keeper rule: ${reason}`);
      }
    }
  }
  return { keep, reasons };
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
