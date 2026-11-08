import { badRequest, noContent, ok } from '../http.ts';
import type { MockContext } from '../mock.ts';
import { type PrivateInput, type Route, route } from '../router.ts';
import { queueScan } from '../scan.ts';
import type { Library } from '../types.ts';
import { iso, isoOrNull } from '../util.ts';
import { createChecker } from '../validate.ts';

const MAX_LIST = 128;

/** `LibraryResponseDto`; `assetCount` leaves out what is in the trash (offline assets are). */
export function libraryView(ctx: MockContext, library: Library) {
  let assetCount = 0;
  for (const asset of ctx.data.assets.values()) {
    if (asset.libraryId === library.id && asset.deletedAt === null) {
      assetCount += 1;
    }
  }
  return {
    id: library.id,
    name: library.name,
    ownerId: library.ownerId,
    importPaths: library.importPaths,
    exclusionPatterns: library.exclusionPatterns,
    assetCount,
    createdAt: iso(library.createdAt),
    updatedAt: iso(library.updatedAt),
    refreshedAt: isoOrNull(library.refreshedAt),
  };
}

/** Immich checks the owner on a single library, and says "not found" to anybody else. */
function ownLibrary({ ctx, user, params }: PrivateInput, access: string): Library {
  const library = ctx.data.libraries.get(params.id ?? '');
  if (library === undefined || library.ownerId !== user.id) {
    throw badRequest(`Not found or no ${access} access`);
  }
  return library;
}

export const libraryRoutes: Route[] = [
  route('GET', '/libraries', { permission: 'library.read', admin: true }, ({ ctx }) =>
    ok([...ctx.data.libraries.values()].map((library) => libraryView(ctx, library))),
  ),

  route('GET', '/libraries/:id', { permission: 'library.read', admin: true }, (input) =>
    ok(libraryView(input.ctx, ownLibrary(input, 'library.read'))),
  ),

  route('PUT', '/libraries/:id', { permission: 'library.update', admin: true }, (input) => {
    const { ctx, body } = input;
    const c = createChecker(body);
    c.only(['name', 'importPaths', 'exclusionPatterns']);
    const name = c.string('name', { nonEmpty: true });
    const importPaths = c.stringList('importPaths');
    const exclusionPatterns = c.stringList('exclusionPatterns');
    for (const [key, list] of [
      ['importPaths', importPaths],
      ['exclusionPatterns', exclusionPatterns],
    ] as const) {
      if (list !== undefined && list.length > MAX_LIST) {
        c.fail(key, `must contain no more than ${MAX_LIST} elements`);
      }
    }
    c.done();
    const library = ownLibrary(input, 'library.update');
    library.name = name ?? library.name;
    library.importPaths = importPaths ?? library.importPaths;
    library.exclusionPatterns = exclusionPatterns ?? library.exclusionPatterns;
    library.updatedAt = ctx.clock.now();
    return ok(libraryView(ctx, library));
  }),

  route(
    'POST',
    '/libraries/:id/scan',
    { permission: 'library.update', admin: true },
    async (input) => {
      const { ctx } = input;
      const library = ownLibrary(input, 'library.update');
      const { done } = queueScan(ctx, library.id, ctx.settings.scanDelayMs);
      if (ctx.settings.scanDelayMs === 0) {
        await done;
      }
      return noContent();
    },
  ),
];
