import { describe, expect, it } from 'vitest';
import { crumbsFor, locationName, parentLocation } from '@/util/breadcrumbs';

describe('crumbsFor', () => {
  it('is only the root at the volumes root', () => {
    expect(crumbsFor({ volume: null, dir: '' })).toEqual([
      { label: 'Volumes', location: { volume: null, dir: '' } },
    ]);
  });

  it('adds the volume name', () => {
    expect(crumbsFor({ volume: 'photo', dir: '' })).toEqual([
      { label: 'Volumes', location: { volume: null, dir: '' } },
      { label: 'photo', location: { volume: 'photo', dir: '' } },
    ]);
  });

  it('adds one crumb per directory, each going to its own path', () => {
    expect(crumbsFor({ volume: 'photo', dir: '2024/trip/day 1' })).toEqual([
      { label: 'Volumes', location: { volume: null, dir: '' } },
      { label: 'photo', location: { volume: 'photo', dir: '' } },
      { label: '2024', location: { volume: 'photo', dir: '2024' } },
      { label: 'trip', location: { volume: 'photo', dir: '2024/trip' } },
      { label: 'day 1', location: { volume: 'photo', dir: '2024/trip/day 1' } },
    ]);
  });

  it('shows names verbatim: non-ASCII, dotted, spaced and decomposed', () => {
    const nfd = 'Cre\u0301me';
    const crumbs = crumbsFor({ volume: 'Été', dir: `.hidden/a.b/${nfd}/ 日本語 ` });

    expect(crumbs.map((crumb) => crumb.label)).toEqual([
      'Volumes',
      'Été',
      '.hidden',
      'a.b',
      nfd,
      ' 日本語 ',
    ]);
    expect(crumbs.at(-1)?.location).toEqual({
      volume: 'Été',
      dir: `.hidden/a.b/${nfd}/ 日本語 `,
    });
    expect(crumbs.at(-2)?.label).not.toBe(nfd.normalize('NFC'));
  });

  it('skips an empty segment instead of inventing a crumb', () => {
    expect(crumbsFor({ volume: 'photo', dir: 'a//b/' }).map((crumb) => crumb.label)).toEqual([
      'Volumes',
      'photo',
      'a',
      'b',
    ]);
  });

  it('gives every crumb its own location object', () => {
    const [first, second] = crumbsFor({ volume: 'photo', dir: '' });

    expect(first?.location).not.toBe(second?.location);
  });
});

describe('parentLocation', () => {
  it.each([
    [
      { volume: 'photo', dir: '2024/trip/day' },
      { volume: 'photo', dir: '2024/trip' },
    ],
    [
      { volume: 'photo', dir: '2024/trip' },
      { volume: 'photo', dir: '2024' },
    ],
    [
      { volume: 'photo', dir: '2024' },
      { volume: 'photo', dir: '' },
    ],
    [
      { volume: 'photo', dir: '' },
      { volume: null, dir: '' },
    ],
    [
      { volume: null, dir: '' },
      { volume: null, dir: '' },
    ],
  ])('%j -> %j', (location, expected) => {
    expect(parentLocation(location)).toEqual(expected);
  });
});

describe('locationName', () => {
  it.each([
    [{ volume: null, dir: '' }, ''],
    [{ volume: 'photo', dir: '' }, 'photo'],
    [{ volume: 'photo', dir: '2024' }, '2024'],
    [{ volume: 'photo', dir: '2024/trip.old' }, 'trip.old'],
  ])('%j is named %j', (location, expected) => {
    expect(locationName(location)).toBe(expected);
  });
});
