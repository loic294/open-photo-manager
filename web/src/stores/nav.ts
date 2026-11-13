import { batch, computed, type ReadonlySignal, type Signal, signal } from '@preact/signals-core';

/** The tools of the main menu. The first path segment of a tool's route is its id. */
export const TOOLS = ['browse', 'duplicates', 'move', 'format', 'cleanup', 'rename'] as const;
export type ToolId = (typeof TOOLS)[number];

/** Every tool has a source pane; tools that compare also use the comparison pane. */
export const PANES = ['source', 'comparison'] as const;
export type PaneId = (typeof PANES)[number];

/**
 * What a pane shows. `volume` is the volume NAME (the directory under the volumes root, never an
 * id; resolve it with `volumeByName` from `@/stores/volumes`), `null` while none is chosen.
 * `dir` is relative to the volume root, `/`-separated, `''` for the root.
 */
export interface PaneLocation {
  volume: string | null;
  dir: string;
}

export const ROOT_LOCATION: Readonly<PaneLocation> = Object.freeze({ volume: null, dir: '' });

/** Query keys of the panes: `?src=photo/2024/trip&cmp=backup`. Other keys are left alone. */
export const PANE_QUERY_KEYS: Readonly<Record<PaneId, string>> = {
  source: 'src',
  comparison: 'cmp',
};

/** `/browse/` and `/browse` are the same page. */
export function normalisePath(pathname: string): string {
  return pathname.length > 1 ? pathname.replace(/\/+$/, '') || '/' : pathname;
}

/**
 * Parses a `volume[/dir...]` string. Empty and `.` segments are dropped and anything unsafe
 * (`..`, NUL) gives the root. Unicode is deliberately NOT normalised: the index keeps the on-disk
 * bytes (NFD names from macOS) and must be matched exactly.
 */
export function parseLocation(value: string | null | undefined): PaneLocation {
  if (!value || value.includes('\0')) return ROOT_LOCATION;
  const segments: string[] = [];
  for (const segment of value.split('/')) {
    if (segment === '..') return ROOT_LOCATION;
    if (segment !== '' && segment !== '.') segments.push(segment);
  }
  const [volume, ...rest] = segments;
  return volume === undefined ? ROOT_LOCATION : { volume, dir: rest.join('/') };
}

/** The inverse of `parseLocation`: `null` when no volume is chosen (the key is then omitted). */
export function formatLocation(location: PaneLocation): string | null {
  if (location.volume === null) return null;
  return location.dir ? `${location.volume}/${location.dir}` : location.volume;
}

export function sameLocation(a: PaneLocation, b: PaneLocation): boolean {
  return a.volume === b.volume && a.dir === b.dir;
}

function locationsFromUrl(): Record<PaneId, PaneLocation> {
  const params = new URLSearchParams(window.location.search);
  return {
    source: parseLocation(params.get(PANE_QUERY_KEYS.source)),
    comparison: parseLocation(params.get(PANE_QUERY_KEYS.comparison)),
  };
}

const initial = locationsFromUrl();

/** The pathname of the current page, without a trailing slash. */
export const currentPath: Signal<string> = signal(normalisePath(window.location.pathname));

/** The active tool, or `null` on pages that are not tools (trash, settings, unknown). */
export const currentTool: ReadonlySignal<ToolId | null> = computed(() => {
  const first = currentPath.value.split('/')[1] ?? '';
  return (TOOLS as readonly string[]).includes(first) ? (first as ToolId) : null;
});

/** The independent location of each pane. Change it with `setLocation`, never by assignment. */
export const locations: Readonly<Record<PaneId, Signal<PaneLocation>>> = {
  source: signal(initial.source),
  comparison: signal(initial.comparison),
};

/** Query strings keep `/` readable: `src=photo/2024` instead of `src=photo%2F2024`. */
function readable(search: string): string {
  return search.replaceAll('%2F', '/');
}

function withPaneParams(
  url: URL,
  override?: { pane: PaneId; location: PaneLocation; sync?: boolean },
): string {
  for (const pane of PANES) {
    const location =
      override && (override.sync || override.pane === pane)
        ? override.location
        : locations[pane].value;
    const text = formatLocation(location);
    if (text === null) url.searchParams.delete(PANE_QUERY_KEYS[pane]);
    else url.searchParams.set(PANE_QUERY_KEYS[pane], text);
  }
  return `${url.pathname}${readable(url.search)}${url.hash}`;
}

/**
 * Moves a pane and mirrors both panes into the URL (`?src=..&cmp=..`), so the address bar is
 * always a deep link. Pushes a history entry (Back returns to the previous folder); pass
 * `replace: true` for corrections that should not add one.
 */
export function setLocation(
  pane: PaneId,
  next: PaneLocation,
  options: { replace?: boolean; sync?: boolean } = {},
): void {
  const normalised = parseLocation(formatLocation(next));
  batch(() => {
    for (const id of options.sync ? PANES : [pane]) {
      if (!sameLocation(locations[id].peek(), normalised)) locations[id].value = normalised;
    }
  });

  const inUrl = locationsFromUrl();
  if (PANES.every((id) => sameLocation(inUrl[id], locations[id].peek()))) return;

  const target = withPaneParams(new URL(window.location.href));
  if (options.replace) window.history.replaceState(window.history.state, '', target);
  else window.history.pushState(null, '', target);
}

/**
 * The href for a link to `path` that keeps both panes where they are. Reads the pane signals, so
 * call it inside `render()` and the link stays current.
 */
export function linkTo(path: string): string {
  return withPaneParams(new URL(path, window.location.origin));
}

/**
 * The href of the CURRENT page with `pane` moved to `location` and the other pane where it is: the
 * real link behind a folder row or a breadcrumb, so middle-click and "open in a new tab" work. It
 * is the address `setLocation(pane, location)` would put in the address bar. Reads the signals, so
 * call it inside `render()` and the link stays current.
 */
export function linkToLocation(pane: PaneId, location: PaneLocation, sync = false): string {
  const url = new URL(currentPath.value, window.location.origin);
  return withPaneParams(url, { pane, location: parseLocation(formatLocation(location)), sync });
}

/**
 * Navigates the app to `path` (route changes included) as if the user followed a link. Use
 * `navigate(linkTo('/duplicates'))` to carry the panes along. Plain `<a href>` clicks are handled
 * by the router already; this is for code.
 */
export function navigate(path: string, options: { replace?: boolean } = {}): void {
  if (options.replace) window.history.replaceState(window.history.state, '', path);
  else window.history.pushState(null, '', path);
  window.dispatchEvent(new PopStateEvent('popstate'));
}

/** Reads the path and the pane locations from the address bar into the signals. */
export function syncFromLocation(): void {
  const fromUrl = locationsFromUrl();
  batch(() => {
    currentPath.value = normalisePath(window.location.pathname);
    for (const pane of PANES) {
      if (!sameLocation(locations[pane].peek(), fromUrl[pane]))
        locations[pane].value = fromUrl[pane];
    }
  });
}

/** Keeps the signals in step with Back/Forward. Returns the stop function. */
export function startNavSync(): () => void {
  window.addEventListener('popstate', syncFromLocation);
  syncFromLocation();
  return () => window.removeEventListener('popstate', syncFromLocation);
}
