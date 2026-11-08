import type { MockContext } from './mock.ts';
import { ScenarioError } from './scenario.ts';
import { generateAssets, upsertAsset } from './seeding.ts';
import type { Album, Data, Tag, User } from './types.ts';

export function createData(): Data {
  return {
    users: new Map(),
    sessions: new Map(),
    keys: new Map(),
    libraries: new Map(),
    assets: new Map(),
    assetsByPath: new Map(),
    albums: new Map(),
    tags: new Map(),
    jobs: [],
    violations: [],
    nextSeq: 1,
    generation: 0,
    order: null,
  };
}

/** Fills the (empty) model from the scenario of the context. */
export function seedData(ctx: MockContext): void {
  const { data, scenario } = ctx;
  const now = ctx.clock.now();

  for (const seed of scenario.users) {
    data.users.set(seed.id, { ...seed, createdAt: now });
  }
  const users = [...data.users.values()];
  const fallbackOwner: User | undefined = users.find((user) => user.isAdmin) ?? users[0];
  if (fallbackOwner === undefined) {
    throw new ScenarioError(['scenario.users must contain at least 1 elements'], 'seeding');
  }

  for (const seed of scenario.libraries) {
    const owner = users.find((user) => user.email === seed.owner) ?? fallbackOwner;
    data.libraries.set(seed.id, {
      id: seed.id,
      name: seed.name,
      ownerId: owner.id,
      importPaths: [...seed.importPaths],
      exclusionPatterns: [...seed.exclusionPatterns],
      createdAt: now,
      updatedAt: now,
      refreshedAt: null,
    });
  }

  for (const input of scenario.assets) {
    upsertAsset(ctx, input, now);
  }
  for (const input of scenario.generate) {
    generateAssets(ctx, input);
  }

  const errors: string[] = [];
  scenario.albums.forEach((seed, albumIndex) => {
    const album: Album = {
      id: seed.id,
      name: seed.name,
      ownerId: fallbackOwner.id,
      createdAt: now,
      updatedAt: now,
      assetIds: new Set(),
    };
    seed.assets.forEach((path, assetIndex) => {
      const asset = data.assetsByPath.get(path);
      if (asset === undefined) {
        errors.push(`scenario.albums[${albumIndex}].assets[${assetIndex}] has no asset at ${path}`);
      } else {
        album.assetIds.add(asset.id);
      }
    });
    data.albums.set(album.id, album);
  });
  if (errors.length > 0) {
    throw new ScenarioError(errors, 'seeding');
  }

  for (const seed of scenario.tags) {
    const tag: Tag = {
      id: seed.id,
      name: seed.name,
      value: seed.value,
      createdAt: now,
      updatedAt: now,
      assetIds: new Set(),
    };
    data.tags.set(tag.id, tag);
  }
}

/**
 * Puts everything back as at start-up: the model is seeded again from the scenario (same ids),
 * sessions and keys are gone, and the failure rules, gates, latency and request log are cleared.
 * The model object stays the same, so the handlers need no new reference.
 */
export function resetState(ctx: MockContext): void {
  const generation = ctx.data.generation + 1;
  ctx.faults.reset();
  ctx.scans.clear();
  Object.assign(ctx.data, createData());
  ctx.data.generation = generation;
  Object.assign(ctx.settings, ctx.baseSettings);
  ctx.log.clear();
  seedData(ctx);
}
