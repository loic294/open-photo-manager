import { chmod, readFile } from 'node:fs/promises';
import { join, relative } from 'node:path';

// Only private generated copies are made readable; the original fixture tree is never chmod'ed.
export async function fixturePermissions(outDir: string, readable: boolean) {
  const paths = (await readFile(join(outDir, '.fixtures-unreadable'), 'utf8')).trim().split('\n');
  for (const path of paths.filter(Boolean)) {
    if (
      !path.startsWith('volumes/') ||
      path.split('/').includes('..') ||
      relative(outDir, join(outDir, path)).startsWith('..')
    )
      throw new Error(`Unsafe permission fixture ${path}`);
    try {
      await chmod(join(outDir, path), readable ? 0o700 : 0);
    } catch (error) {
      if (!readable || (error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
  }
}
