import type { PaneLocation } from '@/stores/nav';

/** One step of the path of a pane: what to show and where a click goes. */
export interface Crumb {
  label: string;
  location: PaneLocation;
}

/** The root crumb: the virtual top level whose folders are the volumes. */
export const ROOT_CRUMB_LABEL = 'Volumes';

/**
 * The path of a location as breadcrumbs, root first: "Volumes", the volume name, then one crumb
 * per directory. The last one is the current page. Segments are the names as they are on disk:
 * never normalised, never trimmed.
 */
export function crumbsFor(location: PaneLocation): Crumb[] {
  const crumbs: Crumb[] = [{ label: ROOT_CRUMB_LABEL, location: { volume: null, dir: '' } }];
  const { volume } = location;
  if (volume === null) return crumbs;
  crumbs.push({ label: volume, location: { volume, dir: '' } });
  let dir = '';
  for (const segment of location.dir.split('/')) {
    if (segment === '') continue;
    dir = dir === '' ? segment : `${dir}/${segment}`;
    crumbs.push({ label: segment, location: { volume, dir } });
  }
  return crumbs;
}

/**
 * The folder above a location: the parent directory, the volume root from a first-level
 * directory, the volumes root from a volume root, and the volumes root again from itself.
 */
export function parentLocation(location: PaneLocation): PaneLocation {
  const { volume, dir } = location;
  if (volume === null || dir === '') return { volume: null, dir: '' };
  const cut = dir.lastIndexOf('/');
  return { volume, dir: cut === -1 ? '' : dir.slice(0, cut) };
}

/** The last path segment of a location: the volume name for a volume root, `''` at the root. */
export function locationName(location: PaneLocation): string {
  if (location.volume === null) return '';
  if (location.dir === '') return location.volume;
  return location.dir.slice(location.dir.lastIndexOf('/') + 1);
}
