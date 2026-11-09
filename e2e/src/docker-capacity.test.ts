import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  capacitySkip,
  DockerCapacityAbortError,
  DockerCapacityBlockedError,
  parseDockerDfAvailableKiB,
  requirePostStartCapacity,
  requireStartCapacity,
} from './docker-capacity.ts';

test('parses available KiB from df output for the Docker VM root filesystem', () => {
  assert.equal(
    parseDockerDfAvailableKiB(
      'Filesystem 1024-blocks Used Available Capacity Mounted on\n' +
        'overlay 61202244 55647560 2413360 96% /\n',
    ),
    2_413_360,
  );
  assert.throws(() => parseDockerDfAvailableKiB('not df output'), /Cannot parse/);
});

test('the injected startup measurer enforces the inclusive 3 GiB floor', async () => {
  let calls = 0;
  const measure = async () => {
    calls += 1;
    return 3_145_728;
  };
  assert.equal(await requireStartCapacity(measure), 3_145_728);
  assert.equal(calls, 1);
  await assert.rejects(
    () => requireStartCapacity(async () => 3_145_727),
    (error: unknown) =>
      error instanceof DockerCapacityBlockedError &&
      /3\.00 GiB is required/.test(error.message) &&
      /Enlarge the Docker Desktop disk/.test(error.message) &&
      /with the user's approval/.test(error.message),
  );
});

test('the injected post-start measurer aborts below 1.5 GiB and reports the delta', async () => {
  assert.equal(await requirePostStartCapacity(4_000_000, async () => 1_572_864), 1_572_864);
  await assert.rejects(
    () => requirePostStartCapacity(3_500_000, async () => 1_000_000),
    (error: unknown) =>
      error instanceof DockerCapacityAbortError &&
      /1\.50 GiB abort floor/.test(error.message) &&
      /-2\.38 GiB delta/.test(error.message),
  );
});

test('only a typed startup capacity refusal converts to an explicit blocked skip', () => {
  const error = new DockerCapacityBlockedError(2_413_360);
  assert.deepEqual(capacitySkip(error), { type: 'blocked', description: error.message });
  assert.equal(capacitySkip(new DockerCapacityAbortError(3_500_000, 1_000_000)), undefined);
  assert.equal(capacitySkip(new Error(error.message)), undefined);
});
