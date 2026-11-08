import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  locations,
  type PaneLocation,
  ROOT_LOCATION,
  setLocation,
  syncFromLocation,
} from '@/stores/nav';
import { toolConfig } from '@/test/tool-config';
import { followSource } from '@/tools/framework/follow-source';
import { createToolContext, type ToolContext } from '@/tools/framework/tool-context';

const TRIP: PaneLocation = { volume: 'photos', dir: '2024/trip' };
const PARTY: PaneLocation = { volume: 'photos', dir: '2024/party' };
const OLD: PaneLocation = { volume: 'backup', dir: 'old' };
const PHOTOS: PaneLocation = { volume: 'photos', dir: '' };

const contexts: ToolContext[] = [];
const stops: (() => void)[] = [];

function goTo(url: string): void {
  window.history.replaceState(null, '', url);
  syncFromLocation();
}

beforeEach(() => goTo('/format'));

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  for (const ctx of contexts.splice(0)) ctx.dispose();
  vi.restoreAllMocks();
  goTo('/');
});

function context(): ToolContext {
  const ctx = createToolContext(toolConfig({ id: 'format' }), document.createElement('div'));
  contexts.push(ctx);
  return ctx;
}

/** Starts following with a context of its own; the cleanup runs after the test as well. */
function follow(options?: Parameters<typeof followSource>[1]): () => void {
  const stop = followSource(context(), options);
  stops.push(stop);
  return stop;
}

/** The follow lands one microtask after the move that caused it. */
const settled = () => new Promise<void>((resolve) => setTimeout(resolve));

const where = (pane: 'source' | 'comparison') => locations[pane].value;

describe('the first move', () => {
  it('puts the comparison on the source folder at once', () => {
    goTo('/format?src=photos/2024/trip');
    follow();
    expect(where('comparison')).toEqual(TRIP);
    expect(where('source')).toEqual(TRIP);
  });

  it('replaces a comparison from a deep link: the source wins', () => {
    goTo('/format?src=photos/2024/trip&cmp=backup/old');
    follow();
    expect(where('comparison')).toEqual(TRIP);
    expect(window.location.search).toBe('?src=photos/2024/trip&cmp=photos/2024/trip');
  });

  it('does it by replacing the history entry, not by adding one', () => {
    goTo('/format?src=photos/2024/trip&cmp=backup/old');
    const push = vi.spyOn(window.history, 'pushState');
    const replace = vi.spyOn(window.history, 'replaceState');
    follow();
    expect(push).not.toHaveBeenCalled();
    expect(replace).toHaveBeenCalledTimes(1);
  });

  it('does nothing while the source has no volume', () => {
    goTo('/format?cmp=backup/old');
    follow();
    expect(where('source')).toEqual(ROOT_LOCATION);
    expect(where('comparison')).toEqual(OLD);
  });

  it('does nothing when the comparison is there already', () => {
    goTo('/format?src=photos/2024/trip&cmp=photos/2024/trip');
    const replace = vi.spyOn(window.history, 'replaceState');
    follow();
    expect(replace).not.toHaveBeenCalled();
  });
});

describe('following', () => {
  it('moves the comparison to every new source folder', async () => {
    goTo('/format?src=photos/2024/trip');
    follow();
    setLocation('source', PARTY);
    await settled();
    expect(where('comparison')).toEqual(PARTY);
    setLocation('source', PHOTOS);
    await settled();
    expect(where('comparison')).toEqual(PHOTOS);
  });

  it('starts following at the first move when it began on the volumes root', async () => {
    follow();
    expect(where('comparison')).toEqual(ROOT_LOCATION);
    setLocation('source', TRIP);
    await settled();
    expect(where('comparison')).toEqual(TRIP);
  });

  it('follows a move of the source made by Back and Forward too', async () => {
    goTo('/format?src=photos/2024/trip&cmp=photos/2024/trip');
    follow();
    setLocation('source', PARTY);
    await settled();
    goTo('/format?src=photos/2024/trip&cmp=photos/2024/trip');
    await settled();
    expect(where('source')).toEqual(TRIP);
    expect(where('comparison')).toEqual(TRIP);
  });

  it('collects the moves of one turn into one', async () => {
    goTo('/format?src=photos/2024/trip');
    follow();
    const replace = vi.spyOn(window.history, 'replaceState');
    setLocation('source', PARTY);
    setLocation('source', PHOTOS);
    await settled();
    expect(where('comparison')).toEqual(PHOTOS);
    expect(replace).toHaveBeenCalledTimes(1);
  });

  it('stays where it is while the source goes to the volumes root', async () => {
    goTo('/format?src=photos/2024/trip');
    follow();
    setLocation('source', ROOT_LOCATION);
    await settled();
    expect(where('comparison')).toEqual(TRIP);
  });
});

describe('browsing each side on its own', () => {
  it('never moves the source when the comparison moves', async () => {
    goTo('/format?src=photos/2024/trip');
    follow();
    setLocation('comparison', OLD);
    await settled();
    expect(where('source')).toEqual(TRIP);
  });

  it('never moves the comparison back', async () => {
    goTo('/format?src=photos/2024/trip');
    follow();
    setLocation('comparison', OLD);
    await settled();
    await settled();
    expect(where('comparison')).toEqual(OLD);
  });

  it('follows the next move of the source again', async () => {
    goTo('/format?src=photos/2024/trip');
    follow();
    setLocation('comparison', OLD);
    await settled();
    setLocation('source', PARTY);
    await settled();
    expect(where('comparison')).toEqual(PARTY);
  });
});

describe('history', () => {
  it('leaves the entry of the source move to the move, and corrects it', async () => {
    goTo('/format?src=photos/2024/trip&cmp=photos/2024/trip');
    follow();
    const push = vi.spyOn(window.history, 'pushState');
    const replace = vi.spyOn(window.history, 'replaceState');
    setLocation('source', PARTY);
    await settled();
    expect(push).toHaveBeenCalledTimes(1);
    expect(replace).toHaveBeenCalledTimes(1);
    expect(window.location.search).toBe('?src=photos/2024/party&cmp=photos/2024/party');
  });

  it('adds no entry when the source move itself only replaces one', async () => {
    goTo('/format?src=photos/2024/trip&cmp=photos/2024/trip');
    follow();
    const push = vi.spyOn(window.history, 'pushState');
    setLocation('source', PARTY, { replace: true });
    await settled();
    expect(push).not.toHaveBeenCalled();
    expect(window.location.search).toBe('?src=photos/2024/party&cmp=photos/2024/party');
  });
});

describe('enabled', () => {
  it('is asked on every move, and a false answer stops the follow', async () => {
    goTo('/format?src=photos/2024/trip&cmp=backup/old');
    const enabled = vi.fn(() => false);
    follow({ enabled });
    expect(enabled).toHaveBeenCalledTimes(1);
    expect(where('comparison')).toEqual(OLD);
    setLocation('source', PARTY);
    await settled();
    expect(enabled).toHaveBeenCalledTimes(2);
    expect(where('comparison')).toEqual(OLD);
  });

  it('is not asked while the source has no volume', async () => {
    const enabled = vi.fn(() => true);
    follow({ enabled });
    await settled();
    expect(enabled).not.toHaveBeenCalled();
  });

  it('resumes at the next move of the source, not when it turns true', async () => {
    goTo('/format?src=photos/2024/trip&cmp=backup/old');
    let on = false;
    follow({ enabled: () => on });
    on = true;
    await settled();
    expect(where('comparison')).toEqual(OLD);
    setLocation('source', PARTY);
    await settled();
    expect(where('comparison')).toEqual(PARTY);
  });

  it('is read with the answer of the moment the follow lands', async () => {
    goTo('/format?src=photos/2024/trip&cmp=backup/old');
    let on = true;
    follow({ enabled: () => on });
    setLocation('comparison', OLD);
    setLocation('source', PARTY);
    on = false;
    await settled();
    expect(where('comparison')).toEqual(OLD);
  });
});

describe('the cleanup', () => {
  it('stops the follow', async () => {
    goTo('/format?src=photos/2024/trip');
    const stop = follow();
    stop();
    setLocation('source', PARTY);
    await settled();
    expect(where('comparison')).toEqual(TRIP);
  });

  it('cancels a follow that has not landed yet', async () => {
    goTo('/format?src=photos/2024/trip');
    const stop = follow();
    setLocation('source', PARTY);
    stop();
    await settled();
    expect(where('comparison')).toEqual(TRIP);
  });

  it('can be called twice', () => {
    const stop = follow();
    stop();
    expect(() => stop()).not.toThrow();
  });
});
