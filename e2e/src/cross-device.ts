import { mkdir, rename, rm, stat, statfs, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { type AppInstance, run } from './app-instance';

/** A real, private separate filesystem, not a production EXDEV injection. */
export class CrossDevice {
  readonly volume = 'system-cross-device';
  readonly mount: string;
  readonly image: string;
  private attached = false;
  private deviceIds?: { source: number; destination: number };

  private constructor(readonly app: AppInstance) {
    this.mount = app.volumePath(this.volume);
    this.image = join(app.directory, 'system-cross-device.sparseimage');
  }

  get evidence() {
    if (!this.deviceIds) throw new Error('Cross-device fixture has not verified EXDEV');
    return { ...this.deviceIds, renameErrno: 'EXDEV' };
  }

  static async create(app: AppInstance, copyBytes: number) {
    if (process.platform !== 'darwin')
      throw new Error('Private APFS cross-device fixtures require macOS hdiutil');
    if (!Number.isSafeInteger(copyBytes) || copyBytes <= 0 || copyBytes > 4 * 1024 ** 3)
      throw new Error('Cross-device fixture copy must be at most 4 GiB');
    const available = await statfs(app.directory);
    if (available.bavail * available.bsize < copyBytes + 1024 ** 3)
      throw new Error(
        'Not enough host space for the private cross-device copy plus 1 GiB headroom',
      );
    const device = new CrossDevice(app);
    await app.suspend();
    await mkdir(device.mount);
    await run('hdiutil', [
      'create',
      '-size',
      '6g',
      '-type',
      'SPARSE',
      '-fs',
      'APFS',
      '-volname',
      'APM private E2E',
      device.image,
    ]);
    // Cleanup is registered before attaching: even a partially failed attach must not be traversed.
    app.beforeCleanup(() => device.detach());
    device.attached = true;
    try {
      await run('hdiutil', [
        'attach',
        '-nobrowse',
        '-noautoopen',
        '-mountpoint',
        device.mount,
        device.image,
      ]);
      const source = join(app.directory, 'system-exdev-probe');
      const destination = join(device.mount, 'system-exdev-probe');
      await writeFile(source, 'private EXDEV probe');
      const sourceDevice = (await stat(source)).dev;
      const destinationDevice = (await stat(device.mount)).dev;
      if (sourceDevice === destinationDevice)
        throw new Error('Private disk image did not provide a distinct device');
      try {
        await rename(source, destination);
        throw new Error('Rename unexpectedly succeeded across the private devices');
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'EXDEV') throw error;
      } finally {
        await rm(source, { force: true });
      }
      device.deviceIds = { source: sourceDevice, destination: destinationDevice };
      await app.resume();
      await app.waitForIdle();
      const volume = (await app.api.volumes()).find((volume) => volume.name === device.volume);
      if (!volume?.online || !volume.writable)
        throw new Error('Private cross-device volume was not discovered writable and online');
      return device;
    } catch (error) {
      await app.suspend();
      await device.detach();
      throw error;
    }
  }

  private async detach() {
    if (!this.attached) return;
    try {
      const mounted = (await stat(this.mount)).dev !== (await stat(this.app.volumesDir)).dev;
      if (mounted) await run('hdiutil', ['detach', this.mount]);
      if ((await stat(this.mount)).dev !== (await stat(this.app.volumesDir)).dev)
        throw new Error(`Device still mounted at ${this.mount}`);
    } catch (cause) {
      throw new Error(
        `Refusing recursive fixture cleanup; preserving mount ${this.mount}, image ${this.image}, directory ${this.app.directory}`,
        { cause },
      );
    }
    this.attached = false;
  }

  async close() {
    await this.app.suspend();
    await this.detach();
  }
}
