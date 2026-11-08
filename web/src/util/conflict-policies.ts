import type { ConflictPolicy } from '@/api/generated/ConflictPolicy';

export const CONFLICT_POLICIES: readonly {
  value: ConflictPolicy;
  label: string;
  help: string;
}[] = [
  {
    value: 'smart',
    label: 'Smart',
    help: 'An identical file is skipped and reported. A different file is moved under a new name such as photo_1.jpg.',
  },
  {
    value: 'skip',
    label: 'Skip',
    help: 'The file stays where it is and is reported as skipped.',
  },
  {
    value: 'rename',
    label: 'Rename',
    help: 'The file is moved under a new name such as photo_1.jpg.',
  },
  {
    value: 'replace',
    label: 'Replace',
    help: 'The file that is already there goes to the trash and the moved file takes its name.',
  },
];

export function conflictPolicyLabel(value: ConflictPolicy): string {
  return CONFLICT_POLICIES.find((entry) => entry.value === value)?.label ?? value;
}

export function isConflictPolicy(value: unknown): value is ConflictPolicy {
  return CONFLICT_POLICIES.some((entry) => entry.value === value);
}
