import type { MoveType } from '@/api/generated/MoveType';

export const MOVE_TYPES = {
  keep_source: {
    label: 'Keep source file structure',
    title:
      'Each file keeps its folder path relative to the source folder, recreated under the destination folder.',
  },
  keep_destination: {
    label: 'Keep destination file structure',
    title:
      'Each file goes to the folder of its matching file in the destination; without a match it goes to the destination folder.',
  },
  flat: {
    label: 'Flat file transfer',
    title: 'Every file goes straight into the destination folder.',
  },
  custom: {
    label: 'Use custom file structure',
    title: "Folders are built from a pattern such as {YYYY}/{mm} using each file's capture date.",
  },
} satisfies Record<MoveType, { label: string; title: string }>;

export function isMoveType(value: unknown): value is MoveType {
  return typeof value === 'string' && Object.hasOwn(MOVE_TYPES, value);
}
