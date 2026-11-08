import { cp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { AppInstance, run } from './app-instance';
import { fixturePermissions } from './fixture-copy';
import { binary, cache, golden, root } from './paths';

export default async function setup() {
  await mkdir(cache, { recursive: true });
  const fixtures = join(root, 'fixtures/out');
  try {
    await stat(join(fixtures, 'manifest.json'));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    await run('make', ['fixtures']);
  }
  const signature = JSON.stringify(
    await Promise.all(
      [join(fixtures, 'manifest.json'), binary, join(root, 'web/dist/index.html')].map(
        async (path) => ({
          path,
          mtime: (await stat(path)).mtimeMs,
          size: (await stat(path)).size,
        }),
      ),
    ),
  );
  try {
    if ((await readFile(join(golden, 'stamp'), 'utf8')) === signature) {
      console.log('E2E: reusing golden indexed copy');
      return;
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
  }
  const seed = join(cache, 'seed');
  await rm(seed, { recursive: true, force: true });
  await mkdir(seed, { recursive: true });
  // Mode-000 scenarios cannot be read to copy them. Generate a private seed instead of
  // temporarily changing permissions of fixtures/out or dropping those scenarios.
  await run('bash', ['fixtures/generate.sh', '--out', join(seed, 'out')]);
  await fixturePermissions(join(seed, 'out'), true);
  const app = await AppInstance.start({ cold: true }, seed);
  try {
    await app.waitForIdle();
    await app.suspend();
    await fixturePermissions(app.outDir, true);
    await rm(golden, { recursive: true, force: true });
    await mkdir(golden, { recursive: true });
    await cp(app.outDir, join(golden, 'out'), {
      recursive: true,
      preserveTimestamps: true,
      verbatimSymlinks: true,
    });
    await cp(app.dataDir, join(golden, 'data'), {
      recursive: true,
      preserveTimestamps: true,
      verbatimSymlinks: true,
    });
    await writeFile(join(golden, 'stamp'), signature);
    console.log('E2E: built golden indexed copy');
  } finally {
    await app.stop();
    await rm(seed, { recursive: true, force: true });
  }
}
