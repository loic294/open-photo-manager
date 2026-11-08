import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFile, stat } from 'node:fs/promises';
import { CrossDevice } from '../../src/cross-device';
import { writeFile } from '../../src/disk';
import { expect, test } from '../../src/test';

test.describe.configure({ mode: 'default' });

test('private cross-device fixture discovers a real writable volume and detaches after shutdown @slow', async ({
  app,
}) => {
  test.setTimeout(90_000);
  test.skip(process.platform !== 'darwin', 'Private APFS mount lifecycle requires macOS hdiutil');
  const device = await CrossDevice.create(app, 64 * 1024);
  try {
    expect(device.evidence.source).not.toBe(device.evidence.destination);
    expect(device.evidence.renameErrno).toBe('EXDEV');
    const volume = (await app.api.volumes()).find((volume) => volume.name === device.volume);
    expect(volume).toMatchObject({ online: true, writable: true });
    await writeFile(app, device.volume, 'private.txt', 'private mounted fixture bytes');
    expect(await readFile(app.volumePath(`${device.volume}/private.txt`), 'utf8')).toBe(
      'private mounted fixture bytes',
    );
  } finally {
    await device.close();
  }
  expect((await stat(device.mount)).dev).toBe((await stat(app.volumesDir)).dev);
  expect((await stat(device.image)).isFile()).toBe(true);
});

test('a busy private mount fails closed and preserves its image until successful detach @slow', async ({
  app,
}) => {
  test.setTimeout(90_000);
  test.skip(process.platform !== 'darwin', 'Private APFS mount teardown is a macOS harness seam');
  const device = await CrossDevice.create(app, 64 * 1024);
  await writeFile(
    app,
    device.volume,
    'preserved.txt',
    'private mounted bytes must survive failed detach',
  );
  const holder = spawn(
    process.execPath,
    ['-e', 'process.stdin.resume(); process.stdout.write("holding\\n");'],
    {
      cwd: device.mount,
      stdio: ['pipe', 'pipe', 'pipe'],
    },
  );
  try {
    await once(holder.stdout, 'data');
    await expect(app.stop()).rejects.toThrow(
      `preserving mount ${device.mount}, image ${device.image}, directory ${app.directory}`,
    );
    expect((await stat(app.directory)).isDirectory()).toBe(true);
    expect((await stat(device.image)).isFile()).toBe(true);
    expect(await readFile(app.volumePath(`${device.volume}/preserved.txt`), 'utf8')).toBe(
      'private mounted bytes must survive failed detach',
    );
  } finally {
    const exited = once(holder, 'exit');
    holder.kill('SIGTERM');
    await exited;
    await device.close();
  }
  expect((await stat(device.mount)).dev).toBe((await stat(app.volumesDir)).dev);
});
