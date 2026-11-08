import { effect } from '@preact/signals-core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  currentPath,
  currentTool,
  formatLocation,
  linkTo,
  linkToLocation,
  locations,
  navigate,
  normalisePath,
  PANES,
  parseLocation,
  ROOT_LOCATION,
  setLocation,
  startNavSync,
  syncFromLocation,
} from '@/stores/nav';

function goToUrl(url: string) {
  window.history.replaceState(null, '', url);
  syncFromLocation();
}

beforeEach(() => {
  goToUrl('/');
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('parseLocation', () => {
  it.each([null, undefined, '', '/', '//', '/./'])('gives the root for %j', (value) => {
    expect(parseLocation(value)).toEqual({ volume: null, dir: '' });
  });

  it.each([
    ['photo', { volume: 'photo', dir: '' }],
    ['photo/2024/trip', { volume: 'photo', dir: '2024/trip' }],
    ['/photo//2024/./trip/', { volume: 'photo', dir: '2024/trip' }],
  ])('parses %j', (value, expected) => {
    expect(parseLocation(value)).toEqual(expected);
  });

  it.each(['photo/../etc', '..', '../photo', 'photo/\0x'])('rejects %j as the root', (value) => {
    expect(parseLocation(value)).toEqual({ volume: null, dir: '' });
  });

  it('keeps unicode exactly as given (NFD names from macOS must match the index)', () => {
    const nfd = 'e\u0301te\u0301';
    const parsed = parseLocation(`photo/${nfd}`);

    expect(parsed.dir).toBe(nfd);
    expect(parsed.dir).not.toBe(nfd.normalize('NFC'));
  });
});

describe('formatLocation', () => {
  it('is the inverse of parseLocation', () => {
    expect(formatLocation({ volume: null, dir: '' })).toBeNull();
    expect(formatLocation({ volume: 'photo', dir: '' })).toBe('photo');
    expect(formatLocation({ volume: 'photo', dir: '2024/trip' })).toBe('photo/2024/trip');
    expect(parseLocation(formatLocation({ volume: 'photo', dir: 'a/b' }))).toEqual({
      volume: 'photo',
      dir: 'a/b',
    });
  });
});

describe('normalisePath', () => {
  it('drops trailing slashes but keeps the root', () => {
    expect(normalisePath('/')).toBe('/');
    expect(normalisePath('/browse')).toBe('/browse');
    expect(normalisePath('/browse/')).toBe('/browse');
    expect(normalisePath('//')).toBe('/');
  });
});

describe('currentPath and currentTool', () => {
  it.each([
    ['/browse', 'browse'],
    ['/browse/', 'browse'],
    ['/duplicates', 'duplicates'],
    ['/move', 'move'],
    ['/format', 'format'],
    ['/cleanup', 'cleanup'],
    ['/trash', null],
    ['/settings', null],
    ['/', null],
    ['/nope', null],
  ])('%s is the %s tool', (url, tool) => {
    goToUrl(url);

    expect(currentTool.value).toBe(tool);
  });

  it('follows the address bar on sync', () => {
    goToUrl('/duplicates/');

    expect(currentPath.value).toBe('/duplicates');
  });
});

describe('locations', () => {
  it('reads both panes from the query string', () => {
    goToUrl('/browse?src=photo/2024&cmp=backup/old%20stuff');

    expect(locations.source.value).toEqual({ volume: 'photo', dir: '2024' });
    expect(locations.comparison.value).toEqual({ volume: 'backup', dir: 'old stuff' });
  });

  it('accepts encoded slashes and falls back to the root for bad values', () => {
    goToUrl('/browse?src=photo%2F2024&cmp=..%2Fx');

    expect(locations.source.value).toEqual({ volume: 'photo', dir: '2024' });
    expect(locations.comparison.value).toEqual(ROOT_LOCATION);
  });

  it('resets a pane whose key left the URL', () => {
    goToUrl('/browse?src=photo');
    goToUrl('/browse');

    expect(locations.source.value).toEqual(ROOT_LOCATION);
  });

  it('keeps the same object when the location did not change', () => {
    goToUrl('/browse?src=photo/2024');
    const before = locations.source.value;
    const runs = vi.fn();
    const stop = effect(() => runs(locations.source.value));

    syncFromLocation();
    stop();

    expect(locations.source.value).toBe(before);
    expect(runs).toHaveBeenCalledTimes(1);
  });
});

describe('setLocation', () => {
  it('updates the signal and the address bar, pushing a history entry', () => {
    goToUrl('/browse');
    const push = vi.spyOn(window.history, 'pushState');

    setLocation('source', { volume: 'photo', dir: '2024/trip' });

    expect(locations.source.value).toEqual({ volume: 'photo', dir: '2024/trip' });
    expect(window.location.pathname).toBe('/browse');
    expect(window.location.search).toBe('?src=photo/2024/trip');
    expect(push).toHaveBeenCalledTimes(1);
  });

  it('mirrors both panes and leaves other keys and the hash alone', () => {
    goToUrl('/duplicates?mode=exact&cmp=backup#top');

    setLocation('source', { volume: 'photo', dir: '' });

    const params = new URLSearchParams(window.location.search);
    expect(params.get('mode')).toBe('exact');
    expect(params.get('src')).toBe('photo');
    expect(params.get('cmp')).toBe('backup');
    expect(window.location.hash).toBe('#top');
  });

  it('removes the key when the pane has no volume', () => {
    goToUrl('/browse?src=photo&cmp=backup');

    setLocation('comparison', { volume: null, dir: 'ignored' });

    expect(locations.comparison.value).toEqual(ROOT_LOCATION);
    expect(window.location.search).toBe('?src=photo');
  });

  it('can replace the history entry instead of adding one', () => {
    goToUrl('/browse');
    const push = vi.spyOn(window.history, 'pushState');
    const replace = vi.spyOn(window.history, 'replaceState');

    setLocation('source', { volume: 'photo', dir: '' }, { replace: true });

    expect(push).not.toHaveBeenCalled();
    expect(replace).toHaveBeenCalledTimes(1);
    expect(window.location.search).toBe('?src=photo');
  });

  it('does nothing when the pane is already there', () => {
    goToUrl('/browse?src=photo/2024');
    const push = vi.spyOn(window.history, 'pushState');
    const replace = vi.spyOn(window.history, 'replaceState');

    setLocation('source', { volume: 'photo', dir: '2024' });

    expect(push).not.toHaveBeenCalled();
    expect(replace).not.toHaveBeenCalled();
  });

  it('treats an unsafe directory as the root', () => {
    goToUrl('/browse');

    setLocation('source', { volume: 'photo', dir: '../secret' });

    expect(locations.source.value).toEqual(ROOT_LOCATION);
    expect(window.location.search).toBe('');
  });

  it('writes names exactly as given and round-trips them through the URL', () => {
    goToUrl('/browse');
    const nfd = 'e\u0301te\u0301 2024';

    setLocation('source', { volume: 'photo', dir: nfd });
    const written = window.location.search;
    goToUrl(`/browse${written}`);

    expect(locations.source.value).toEqual({ volume: 'photo', dir: nfd });
  });

  it('keeps names with reserved characters intact', () => {
    goToUrl('/browse');
    const dir = 'a&b=c #1/100%';

    setLocation('source', { volume: 'photo', dir });
    goToUrl(`/browse${window.location.search}`);

    expect(locations.source.value).toEqual({ volume: 'photo', dir });
  });
});

describe('linkTo', () => {
  it('carries the panes onto another page', () => {
    goToUrl('/browse?src=photo/2024&cmp=backup');

    expect(linkTo('/duplicates')).toBe('/duplicates?src=photo/2024&cmp=backup');
  });

  it('is a plain path while no pane has a volume', () => {
    expect(linkTo('/settings')).toBe('/settings');
  });

  it('keeps the query and hash of the target', () => {
    goToUrl('/browse?src=photo');

    expect(linkTo('/move?x=1#a')).toBe('/move?x=1&src=photo#a');
  });

  it('follows the panes', () => {
    goToUrl('/browse');
    setLocation('comparison', { volume: 'backup', dir: 'old' });

    expect(linkTo('/move')).toBe('/move?cmp=backup/old');
  });
});

describe('linkToLocation', () => {
  it('moves one pane on the current page and leaves the other where it is', () => {
    goToUrl('/browse?src=photo/2024&cmp=backup');

    expect(linkToLocation('source', { volume: 'photo', dir: '2024/trip' })).toBe(
      '/browse?src=photo/2024/trip&cmp=backup',
    );
    expect(linkToLocation('comparison', { volume: 'cards', dir: '' })).toBe(
      '/browse?src=photo/2024&cmp=cards',
    );
  });

  it('is the address setLocation writes', () => {
    goToUrl('/duplicates?src=photo&cmp=backup/old');
    const link = linkToLocation('comparison', { volume: 'backup', dir: 'old/2023' });

    setLocation('comparison', { volume: 'backup', dir: 'old/2023' });

    expect(`${window.location.pathname}${window.location.search}`).toBe(link);
  });

  it('drops the key of a pane sent to the volumes root', () => {
    goToUrl('/browse?src=photo/2024&cmp=backup');

    expect(linkToLocation('source', { volume: null, dir: '' })).toBe('/browse?cmp=backup');
  });

  it('keeps names as they are on disk', () => {
    goToUrl('/browse');

    expect(linkToLocation('source', { volume: 'photo', dir: 'Été/ré sumé #1' })).toBe(
      '/browse?src=photo/%C3%89t%C3%A9/r%C3%A9+sum%C3%A9+%231',
    );
  });

  it('does not move the panes itself', () => {
    goToUrl('/browse?src=photo');

    linkToLocation('source', { volume: 'cards', dir: '' });

    expect(locations.source.value).toEqual({ volume: 'photo', dir: '' });
    expect(window.location.search).toBe('?src=photo');
  });
});

describe('navigate', () => {
  it('pushes the url and announces it as a popstate', () => {
    const seen = vi.fn();
    window.addEventListener('popstate', seen);

    navigate('/duplicates?src=photo');
    window.removeEventListener('popstate', seen);

    expect(window.location.pathname).toBe('/duplicates');
    expect(window.location.search).toBe('?src=photo');
    expect(seen).toHaveBeenCalledTimes(1);
  });

  it('can replace the current entry', () => {
    const push = vi.spyOn(window.history, 'pushState');
    const replace = vi.spyOn(window.history, 'replaceState');

    navigate('/trash', { replace: true });

    expect(push).not.toHaveBeenCalled();
    expect(replace).toHaveBeenCalledTimes(1);
    expect(window.location.pathname).toBe('/trash');
  });
});

describe('startNavSync', () => {
  it('syncs now and on every popstate until stopped', () => {
    window.history.replaceState(null, '', '/browse?src=photo');
    const stop = startNavSync();

    expect(currentPath.value).toBe('/browse');
    expect(locations.source.value.volume).toBe('photo');

    window.history.replaceState(null, '', '/move?src=backup/old');
    window.dispatchEvent(new PopStateEvent('popstate'));

    expect(currentPath.value).toBe('/move');
    expect(locations.source.value).toEqual({ volume: 'backup', dir: 'old' });

    stop();
    window.history.replaceState(null, '', '/trash');
    window.dispatchEvent(new PopStateEvent('popstate'));

    expect(currentPath.value).toBe('/move');
  });

  it('has one location per pane', () => {
    expect(Object.keys(locations)).toEqual([...PANES]);
  });
});
