import { readdir, readFile } from 'node:fs/promises';

async function check(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory()) await check(path);
    else if (path.endsWith('.ts') && path !== 'src/waits.ts') {
      if (/\bwaitForTimeout\s*\(/.test(await readFile(path, 'utf8'))) {
        throw new Error(`Fixed sleeps are forbidden: ${path}`);
      }
    }
  }
}
await check('src');
await check('specs');
