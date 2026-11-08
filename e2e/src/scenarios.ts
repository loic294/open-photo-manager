import { basename, dirname } from 'node:path';
import type { Manifest } from './manifest';
import { test } from './test';

export function fixture(manifest: Manifest, key: string) {
  const scenario = manifest.scenario(key);
  test.skip(scenario.skipped, scenario.skip_reason ?? 'Scenario unavailable on this machine');
  const file = manifest
    .filesOf(key)
    .find(
      (entry) =>
        entry.indexable &&
        (entry.kind === 'image' || entry.kind === 'raw' || entry.kind === 'video'),
    );
  if (!file) throw new Error(`No media in scenario ${key}`);
  return {
    ...file,
    name: basename(file.path),
    dir: dirname(file.path) === '.' ? '' : dirname(file.path),
  };
}
export function browseUrl(url: string, source: string, comparison = source) {
  const target = new URL('/browse', url);
  target.searchParams.set('src', source);
  target.searchParams.set('cmp', comparison);
  return target.href;
}
