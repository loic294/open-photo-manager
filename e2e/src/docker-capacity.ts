import { execFile } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { root } from './paths.ts';

const exec = promisify(execFile);
const START_FLOOR_KIB = 3_145_728;
const ABORT_FLOOR_KIB = 1_572_864;
const GIB_KIB = 1_048_576;

export type CapacityMeasurer = () => Promise<number>;

export class DockerCapacityBlockedError extends Error {
  readonly availableKiB: number;

  constructor(availableKiB: number) {
    super(
      `Docker VM has ${formatGiB(availableKiB)} GiB free; 3.00 GiB is required before starting ` +
        'Immich PostgreSQL tests. Enlarge the Docker Desktop disk, or free Docker VM space with ' +
        "the user's approval, then retry.",
    );
    this.name = 'DockerCapacityBlockedError';
    this.availableKiB = availableKiB;
  }
}

export class DockerCapacityAbortError extends Error {
  readonly initialKiB: number;
  readonly availableKiB: number;

  constructor(initialKiB: number, availableKiB: number) {
    const delta = availableKiB - initialKiB;
    super(
      `Docker VM free space fell below the 1.50 GiB abort floor after PostgreSQL initialization: ` +
        `${formatGiB(initialKiB)} GiB before, ${formatGiB(availableKiB)} GiB after ` +
        `(${delta >= 0 ? '+' : ''}${formatGiB(delta)} GiB delta).`,
    );
    this.name = 'DockerCapacityAbortError';
    this.initialKiB = initialKiB;
    this.availableKiB = availableKiB;
  }
}

function formatGiB(kib: number): string {
  return (kib / GIB_KIB).toFixed(2);
}

export function parseDockerDfAvailableKiB(output: string): number {
  const rootLine = output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .find((line) => /^\S+\s+\d+\s+\d+\s+\d+\s+\d+%\s+\/$/.test(line));
  const available = rootLine && /^\S+\s+\d+\s+\d+\s+(\d+)\s+\d+%\s+\/$/.exec(rootLine)?.[1];
  if (!available) throw new Error('Cannot parse Docker VM free space from `df -Pk /` output');
  const kib = Number(available);
  if (!Number.isSafeInteger(kib)) throw new Error('Docker VM free-space value is out of range');
  return kib;
}

export async function requireStartCapacity(measure: CapacityMeasurer): Promise<number> {
  const availableKiB = await measure();
  if (availableKiB < START_FLOOR_KIB) throw new DockerCapacityBlockedError(availableKiB);
  return availableKiB;
}

export async function requirePostStartCapacity(
  initialKiB: number,
  measure: CapacityMeasurer,
): Promise<number> {
  const availableKiB = await measure();
  if (availableKiB < ABORT_FLOOR_KIB) {
    throw new DockerCapacityAbortError(initialKiB, availableKiB);
  }
  return availableKiB;
}

export function capacitySkip(error: unknown): { type: 'blocked'; description: string } | undefined {
  if (!(error instanceof DockerCapacityBlockedError)) return undefined;
  return { type: 'blocked', description: error.message };
}

export async function measureDockerVmFreeKiB(): Promise<number> {
  const compose = await readFile(join(root, 'docker/immich-postgres-test.compose.yml'), 'utf8');
  const image = /^ {4}image:\s*(ghcr\.io\/immich-app\/postgres:\S+@sha256:[a-f\d]{64})\s*$/m.exec(
    compose,
  )?.[1];
  if (!image) {
    throw new Error('Cannot find the pinned PostgreSQL image in the Immich test Compose file');
  }

  try {
    await exec('docker', ['image', 'inspect', image], { cwd: root, timeout: 30_000 });
  } catch (error) {
    throw new Error(
      'Cannot measure Docker VM capacity: the pinned PostgreSQL image is not present locally; ' +
        'the capacity guard will not pull an image or start the test stack.',
      { cause: error },
    );
  }

  let output: string;
  try {
    ({ stdout: output } = await exec(
      'docker',
      ['run', '--rm', '--pull=never', '--network=none', '--entrypoint', 'df', image, '-Pk', '/'],
      { cwd: root, timeout: 30_000 },
    ));
  } catch (error) {
    throw new Error('Cannot measure Docker VM capacity using the pinned PostgreSQL image', {
      cause: error,
    });
  }
  return parseDockerDfAvailableKiB(output);
}
