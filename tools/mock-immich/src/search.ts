import { likeToRegExp } from './glob.ts';
import { badRequest } from './http.ts';
import type { MockContext } from './mock.ts';
import type { Asset, User, Visibility } from './types.ts';
import { VISIBILITIES } from './types.ts';
import { type Checker, createChecker } from './validate.ts';
import { supportsTypedSearch } from './version.ts';

type Predicate = (asset: Asset) => boolean;
type Direction = 'asc' | 'desc';

const DIRECTIONS: readonly Direction[] = ['asc', 'desc'];
const DEFAULT_SIZE = 250;
const MAX_SIZE = 1000;
const TYPED_KEYS = ['filter', 'orderBy', 'cursor'] as const;
const TYPED_ONLY_BODY = ['size', 'orderBy', 'filter', 'withExif', 'cursor'];
const FLAT_BODY = [
  'size',
  'page',
  'order',
  'libraryId',
  'updatedAfter',
  'withDeleted',
  'isOffline',
  'visibility',
  'originalFileName',
  'originalPath',
  'withExif',
];

export interface SearchOutcome {
  items: Asset[];
  withExif: boolean;
  nextPage: string | null;
  nextCursor: string | null;
}

interface Parsed {
  style: 'typed' | 'flat';
  size: number;
  direction: Direction;
  withExif: boolean;
  predicate: Predicate;
  page: number;
  cursor: string | undefined;
}

const byKey = (a: Asset, b: Asset): number => a.fileCreatedAt - b.fileCreatedAt || a.seq - b.seq;

/** Every asset ordered by (capture time, creation order); rebuilt after a change of the set. */
function sortedAssets(ctx: MockContext): Asset[] {
  if (ctx.data.order === null) {
    ctx.data.order = [...ctx.data.assets.values()].sort(byKey);
  }
  return ctx.data.order;
}

/** Runs `POST /search/metadata` for `user` (the universe is the caller's own, unlocked assets). */
export function searchMetadata(ctx: MockContext, user: User, body: unknown): SearchOutcome {
  const parsed = parseSearch(ctx, body);
  const universe: Predicate = (asset) => asset.ownerId === user.id && asset.visibility !== 'locked';
  const matches: Predicate = (asset) => universe(asset) && parsed.predicate(asset);
  const sorted = sortedAssets(ctx);
  const descending = parsed.direction === 'desc';

  let start = descending ? sorted.length - 1 : 0;
  let skip = 0;
  if (parsed.style === 'typed' && parsed.cursor !== undefined) {
    start = positionAfter(sorted, decodeCursor(parsed.cursor, parsed.direction), descending);
  } else if (parsed.style === 'flat') {
    skip = (parsed.page - 1) * parsed.size;
  }

  const items: Asset[] = [];
  let more = false;
  const step = descending ? -1 : 1;
  for (let i = start; i >= 0 && i < sorted.length; i += step) {
    const asset = sorted[i];
    if (asset === undefined || !matches(asset)) {
      continue;
    }
    if (skip > 0) {
      skip -= 1;
      continue;
    }
    if (items.length === parsed.size) {
      more = true;
      break;
    }
    items.push(asset);
  }

  const last = items.at(-1);
  return {
    items,
    withExif: parsed.withExif,
    nextPage: parsed.style === 'flat' && more ? String(parsed.page + 1) : null,
    nextCursor:
      parsed.style === 'typed' && more && last !== undefined
        ? encodeCursor(last, parsed.direction)
        : null,
  };
}

/** Whether the typed keys are present, which decides the style of the body. */
const isTypedBody = (body: unknown): boolean =>
  typeof body === 'object' &&
  body !== null &&
  TYPED_KEYS.some((key) => (body as Record<string, unknown>)[key] !== undefined);

function parseSearch(ctx: MockContext, body: unknown): Parsed {
  const typed = isTypedBody(body);
  if (typed && !supportsTypedSearch(ctx.version)) {
    if (ctx.settings.legacyUnknownFields === 'reject') {
      throw badRequest(
        TYPED_KEYS.filter((key) => (body as Record<string, unknown>)[key] !== undefined).map(
          (key) => `property ${key} should not exist`,
        ),
      );
    }
    return parseFlat(createChecker(stripTyped(body)));
  }
  return typed ? parseTyped(createChecker(body)) : parseFlat(createChecker(body));
}

/** An older server that ignores unknown fields drops the typed keys and reads the rest. */
function stripTyped(body: unknown): Record<string, unknown> {
  const rest: Record<string, unknown> = { ...(body as Record<string, unknown>) };
  for (const key of TYPED_KEYS) {
    delete rest[key];
  }
  return rest;
}

function parseTyped(c: Checker): Parsed {
  c.only(TYPED_ONLY_BODY);
  const size = c.integer('size', { min: 1, max: MAX_SIZE });
  const withExif = c.boolean('withExif');
  const cursor = c.string('cursor');
  const order = c.child('orderBy');
  let direction: Direction = 'desc';
  if (order !== undefined) {
    order.only(['field', 'direction']);
    order.oneOf('field', ['fileCreatedAt', 'localDateTime'], { required: true });
    direction = order.oneOf('direction', DIRECTIONS) ?? 'desc';
  }
  const predicate = typedFilter(c.child('filter'));
  c.done();
  return {
    style: 'typed',
    size: size ?? DEFAULT_SIZE,
    direction,
    withExif: withExif ?? false,
    predicate,
    page: 1,
    cursor,
  };
}

function parseFlat(c: Checker): Parsed {
  c.only(FLAT_BODY);
  const size = c.integer('size', { min: 1, max: MAX_SIZE });
  const page = c.integer('page', { min: 1 });
  const direction = c.oneOf('order', DIRECTIONS) ?? 'desc';
  const libraryId = c.uuid('libraryId');
  const updatedAfter = c.date('updatedAfter');
  const withDeleted = c.boolean('withDeleted');
  const offline = c.boolean('isOffline');
  const visibility = c.oneOf('visibility', VISIBILITIES);
  const name = c.string('originalFileName');
  const path = c.string('originalPath');
  const withExif = c.boolean('withExif');
  c.done();

  const showTrashed = withDeleted === true || offline === true;
  const after = updatedAfter === undefined ? undefined : Date.parse(updatedAfter);
  const lowerName = name?.toLowerCase();
  const lowerPath = path?.toLowerCase();
  const predicate: Predicate = (asset) =>
    (showTrashed || asset.deletedAt === null) &&
    (libraryId === undefined || asset.libraryId === libraryId) &&
    (after === undefined || asset.updatedAt > after) &&
    (offline === undefined || asset.isOffline === offline) &&
    (visibility === undefined
      ? asset.visibility === 'archive' || asset.visibility === 'timeline'
      : asset.visibility === visibility) &&
    (lowerName === undefined || asset.originalFileName.toLowerCase().includes(lowerName)) &&
    (lowerPath === undefined || asset.originalPath.toLowerCase().includes(lowerPath));
  return {
    style: 'flat',
    size: size ?? DEFAULT_SIZE,
    direction,
    withExif: withExif ?? false,
    predicate,
    page: page ?? 1,
    cursor: undefined,
  };
}

const NUMBER_OPERATORS = ['gt', 'gte', 'lt', 'lte'] as const;
const STRING_OPERATORS = ['eq', 'ne', 'in', 'notIn', 'startsWith', 'endsWith', 'like', 'notLike'];
const FILTER_KEYS = [
  'libraryId',
  'trashedAt',
  'isOffline',
  'updatedAt',
  'visibility',
  'originalFileName',
  'originalPath',
];

/** The typed `filter` object as one predicate; every key narrows the result. */
function typedFilter(c: Checker | undefined): Predicate {
  if (c === undefined) {
    return () => true;
  }
  for (const key of Object.keys(c.obj)) {
    if (!FILTER_KEYS.includes(key)) {
      c.fail(key, `is not supported by the mock; the filter keys are ${FILTER_KEYS.join(', ')}`);
    }
  }
  const predicates: Predicate[] = [
    ...libraryFilter(c.child('libraryId')),
    ...trashFilter(c.child('trashedAt')),
    ...offlineFilter(c.child('isOffline')),
    ...updatedFilter(c.child('updatedAt')),
    ...visibilityFilter(c.child('visibility')),
    ...stringFilter(c.child('originalFileName'), (asset) => asset.originalFileName),
    ...stringFilter(c.child('originalPath'), (asset) => asset.originalPath),
  ];
  return (asset) => predicates.every((predicate) => predicate(asset));
}

function libraryFilter(c: Checker | undefined): Predicate[] {
  if (c === undefined) {
    return [];
  }
  c.only(['eq', 'ne']);
  const eq = c.nullableUuid('eq');
  const ne = c.nullableUuid('ne');
  const predicates: Predicate[] = [];
  if (eq !== undefined) {
    predicates.push((asset) => asset.libraryId === eq);
  }
  if (ne !== undefined) {
    predicates.push((asset) => asset.libraryId !== ne);
  }
  return predicates;
}

function trashFilter(c: Checker | undefined): Predicate[] {
  if (c === undefined) {
    return [];
  }
  c.only(['eq', 'ne', ...NUMBER_OPERATORS]);
  const predicates: Predicate[] = [];
  const eq = c.nullableDate('eq');
  const ne = c.nullableDate('ne');
  if (eq !== undefined) {
    const at = eq === null ? null : Date.parse(eq);
    predicates.push((asset) => asset.deletedAt === at);
  }
  if (ne !== undefined) {
    const at = ne === null ? null : Date.parse(ne);
    predicates.push((asset) => asset.deletedAt !== at);
  }
  predicates.push(...rangePredicates(c, (asset) => asset.deletedAt));
  return predicates;
}

function updatedFilter(c: Checker | undefined): Predicate[] {
  if (c === undefined) {
    return [];
  }
  c.only(['eq', 'ne', ...NUMBER_OPERATORS]);
  const predicates: Predicate[] = [];
  const eq = c.date('eq');
  const ne = c.date('ne');
  if (eq !== undefined) {
    predicates.push((asset) => asset.updatedAt === Date.parse(eq));
  }
  if (ne !== undefined) {
    predicates.push((asset) => asset.updatedAt !== Date.parse(ne));
  }
  predicates.push(...rangePredicates(c, (asset) => asset.updatedAt));
  return predicates;
}

/** `gt`, `gte`, `lt` and `lte` of a date field; an absent value (`null`) never matches. */
function rangePredicates(c: Checker, read: (asset: Asset) => number | null): Predicate[] {
  const predicates: Predicate[] = [];
  const bounds: Array<
    [(typeof NUMBER_OPERATORS)[number], (value: number, bound: number) => boolean]
  > = [
    ['gt', (value, bound) => value > bound],
    ['gte', (value, bound) => value >= bound],
    ['lt', (value, bound) => value < bound],
    ['lte', (value, bound) => value <= bound],
  ];
  for (const [operator, compare] of bounds) {
    const raw = c.date(operator);
    if (raw !== undefined) {
      const bound = Date.parse(raw);
      predicates.push((asset) => {
        const value = read(asset);
        return value !== null && compare(value, bound);
      });
    }
  }
  return predicates;
}

function offlineFilter(c: Checker | undefined): Predicate[] {
  if (c === undefined) {
    return [];
  }
  c.only(['eq']);
  const eq = c.boolean('eq', { required: true });
  return eq === undefined ? [] : [(asset) => asset.isOffline === eq];
}

function visibilityFilter(c: Checker | undefined): Predicate[] {
  if (c === undefined) {
    return [];
  }
  c.only(['eq', 'ne', 'in', 'notIn']);
  const predicates: Predicate[] = [];
  const eq = c.oneOf('eq', VISIBILITIES);
  const ne = c.oneOf('ne', VISIBILITIES);
  const include = c.oneOfList<Visibility>('in', VISIBILITIES);
  const exclude = c.oneOfList<Visibility>('notIn', VISIBILITIES);
  if (eq !== undefined) {
    predicates.push((asset) => asset.visibility === eq);
  }
  if (ne !== undefined) {
    predicates.push((asset) => asset.visibility !== ne);
  }
  if (include !== undefined) {
    predicates.push((asset) => include.includes(asset.visibility));
  }
  if (exclude !== undefined) {
    predicates.push((asset) => !exclude.includes(asset.visibility));
  }
  return predicates;
}

/** The string operators of `originalFileName` and `originalPath`; `like` follows SQL's wildcards. */
function stringFilter(c: Checker | undefined, read: (asset: Asset) => string): Predicate[] {
  if (c === undefined) {
    return [];
  }
  c.only(STRING_OPERATORS);
  const predicates: Predicate[] = [];
  const eq = c.string('eq');
  const ne = c.string('ne');
  const include = c.stringList('in');
  const exclude = c.stringList('notIn');
  const startsWith = c.string('startsWith');
  const endsWith = c.string('endsWith');
  const like = c.string('like');
  const notLike = c.string('notLike');
  if (eq !== undefined) {
    predicates.push((asset) => read(asset) === eq);
  }
  if (ne !== undefined) {
    predicates.push((asset) => read(asset) !== ne);
  }
  if (include !== undefined) {
    predicates.push((asset) => include.includes(read(asset)));
  }
  if (exclude !== undefined) {
    predicates.push((asset) => !exclude.includes(read(asset)));
  }
  if (startsWith !== undefined) {
    predicates.push((asset) => read(asset).startsWith(startsWith));
  }
  if (endsWith !== undefined) {
    predicates.push((asset) => read(asset).endsWith(endsWith));
  }
  if (like !== undefined) {
    const regex = likeToRegExp(like, 'whole');
    predicates.push((asset) => regex.test(read(asset)));
  }
  if (notLike !== undefined) {
    const regex = likeToRegExp(notLike, 'whole');
    predicates.push((asset) => !regex.test(read(asset)));
  }
  return predicates;
}

/** The keyset of the last item of a page: an opaque string for the client. */
function encodeCursor(asset: Asset, direction: Direction): string {
  return Buffer.from(JSON.stringify([asset.fileCreatedAt, asset.seq, direction])).toString(
    'base64url',
  );
}

interface Cursor {
  time: number;
  seq: number;
}

function decodeCursor(text: string, direction: Direction): Cursor {
  try {
    const parsed: unknown = JSON.parse(Buffer.from(text, 'base64url').toString('utf8'));
    if (
      Array.isArray(parsed) &&
      parsed.length === 3 &&
      typeof parsed[0] === 'number' &&
      typeof parsed[1] === 'number' &&
      parsed[2] === direction
    ) {
      return { time: parsed[0], seq: parsed[1] };
    }
  } catch {
    // falls through to the error below
  }
  throw badRequest('Invalid cursor');
}

/** The index to continue from, strictly after `cursor` in the walking direction. */
function positionAfter(sorted: Asset[], cursor: Cursor, descending: boolean): number {
  const before = (asset: Asset) =>
    asset.fileCreatedAt < cursor.time ||
    (asset.fileCreatedAt === cursor.time && asset.seq < cursor.seq);
  const beforeOrAt = (asset: Asset) =>
    asset.fileCreatedAt < cursor.time ||
    (asset.fileCreatedAt === cursor.time && asset.seq <= cursor.seq);
  const test = descending ? before : beforeOrAt;
  let low = 0;
  let high = sorted.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    const asset = sorted[middle];
    if (asset !== undefined && test(asset)) {
      low = middle + 1;
    } else {
      high = middle;
    }
  }
  return descending ? low - 1 : low;
}
