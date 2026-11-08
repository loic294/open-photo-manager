import type { MatchMode } from '@/api/generated/MatchMode';

const MODE_DETAILS: Readonly<Record<MatchMode, { label: string; title: string }>> = {
  hash: {
    label: 'Hash exact match',
    title: 'Match the same file content, whatever its name.',
  },
  name_and_time: {
    label: 'Filename and time exact match',
    title: 'Match the same filename and capture date, to the second.',
  },
  name_different_time: {
    label: 'Filename exact match, different time',
    title: 'Match the same filename with another capture time.',
  },
  name_prefix: {
    label: 'Filename prefix match',
    title:
      'The comparison name starts with the source name: IMG_0001 finds IMG_0001_edit, but not IMG_00012 unless Strict prefix is on.',
  },
};

export const MATCH_MODES: readonly { value: MatchMode; label: string; title: string }[] = (
  ['hash', 'name_and_time', 'name_different_time', 'name_prefix'] as const
).map((value) => ({
  value,
  ...MODE_DETAILS[value],
}));

export function isMatchMode(value: unknown): value is MatchMode {
  return typeof value === 'string' && Object.hasOwn(MODE_DETAILS, value);
}

export function matchModeLabel(mode: MatchMode): string {
  return MODE_DETAILS[mode].label;
}
