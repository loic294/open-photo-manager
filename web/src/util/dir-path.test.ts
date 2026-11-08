import { expect, it } from 'vitest';
import { isSameOrBelow } from './dir-path';

it.each([
  ['', '', true],
  ['a/b', '', true],
  ['a/b', 'a/b', true],
  ['a/b/c', 'a/b', true],
  ['a/bc', 'a/b', false],
  ['', 'a', false],
  ['é', 'e\u0301', false],
])('compares %s against %s without changing path bytes', (dir, base, expected) => {
  expect(isSameOrBelow(dir, base)).toBe(expected);
});
