/**
 * The glob dialect of Immich's exclusion patterns, reduced to what is used: `**` (any number of
 * path segments, also none), `*` (anything but `/`) and `?` (one character but `/`). Everything
 * else is literal, so braces and character classes are not interpreted. A pattern is matched
 * against the whole absolute path as Immich sees it (`/mnt/photo/2019/@eaDir/x.jpg`).
 */
export function globToRegExp(pattern: string): RegExp {
  let source = '';
  let i = 0;
  while (i < pattern.length) {
    const char = pattern.charAt(i);
    if (pattern.startsWith('/**', i) && i + 3 === pattern.length) {
      source += '(?:/.*)?';
      break;
    }
    if (pattern.startsWith('**/', i) && (i === 0 || pattern.charAt(i - 1) === '/')) {
      source += '(?:.*/)?';
      i += 3;
    } else if (pattern.startsWith('**', i)) {
      source += '.*';
      i += 2;
    } else if (char === '*') {
      source += '[^/]*';
      i += 1;
    } else if (char === '?') {
      source += '[^/]';
      i += 1;
    } else {
      source += char.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');
      i += 1;
    }
  }
  return new RegExp(`^${source}$`, 's');
}

export function compileExclusions(patterns: readonly string[]): RegExp[] {
  return patterns.map(globToRegExp);
}

/** Whether a file or a directory (`path` without a trailing slash) is excluded. */
export function isExcluded(matchers: readonly RegExp[], path: string): boolean {
  return matchers.some((matcher) => matcher.test(path));
}

/** A SQL `LIKE` pattern as a regular expression: `%` is any text, `_` any one character. */
export function likeToRegExp(pattern: string, mode: 'whole' | 'start' | 'contains'): RegExp {
  let source = '';
  for (const char of pattern) {
    if (char === '%') {
      source += '.*';
    } else if (char === '_') {
      source += '.';
    } else {
      source += char.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');
    }
  }
  const head = mode === 'contains' ? '' : '^';
  const tail = mode === 'whole' ? '$' : '';
  return new RegExp(`${head}${source}${tail}`, 's');
}
