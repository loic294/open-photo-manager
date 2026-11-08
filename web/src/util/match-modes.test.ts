import { expect, it } from 'vitest';
import { isMatchMode, MATCH_MODES, matchModeLabel } from '@/util/match-modes';

it('offers the four generated modes in user order with labels and explanations', () => {
  expect(MATCH_MODES.map(({ value }) => value)).toEqual([
    'hash',
    'name_and_time',
    'name_different_time',
    'name_prefix',
  ]);
  expect(MATCH_MODES.map(({ label }) => label)).toEqual([
    'Hash exact match',
    'Filename and time exact match',
    'Filename exact match, different time',
    'Filename prefix match',
  ]);
  for (const { value, label, title } of MATCH_MODES) {
    expect(isMatchMode(value)).toBe(true);
    expect(matchModeLabel(value)).toBe(label);
    expect(title.length).toBeGreaterThan(20);
  }
  expect(MATCH_MODES[1]?.title).toContain('to the second');
  expect(MATCH_MODES[3]?.title).toContain('IMG_00012');
});

it.each([null, undefined, 1, {}, 'toString', 'HASH', 'unknown'])('rejects %s', (value) => {
  expect(isMatchMode(value)).toBe(false);
});
