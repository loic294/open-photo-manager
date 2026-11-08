import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type {
  AssetInput,
  GenerateInput,
  Scenario,
  ScenarioAlbum,
  ScenarioLibrary,
  ScenarioOptions,
  ScenarioTag,
  ScenarioUser,
} from './types.ts';
import { VISIBILITIES } from './types.ts';
import { isUnder, stableUuid, trimSlash } from './util.ts';
import { type Checker, createChecker } from './validate.ts';
import { parseServerVersion } from './version.ts';

export const DEFAULT_SCENARIO = new URL('../scenarios/default.json', import.meta.url);
export const DEFAULT_GENERATE_START = '2020-01-01T00:00:00.000Z';

/** Every problem of a scenario at once, each with its path (`scenario.users[1].email ...`). */
export class ScenarioError extends Error {
  errors: string[];

  constructor(errors: string[], origin: string) {
    super(`invalid scenario (${origin}):\n  - ${errors.join('\n  - ')}`);
    this.errors = errors;
  }
}

const TOP_KEYS = [
  'description',
  'serverVersion',
  'options',
  'users',
  'libraries',
  'assets',
  'generate',
  'albums',
  'tags',
];

const ASSET_KEYS = [
  'path',
  'library',
  'id',
  'type',
  'fileCreatedAt',
  'fileModifiedAt',
  'isFavorite',
  'rating',
  'description',
  'visibility',
  'isOffline',
  'trashed',
  'make',
  'model',
  'checksum',
];

function setIfDefined<T extends object, K extends keyof T>(
  target: T,
  key: K,
  value: T[K] | undefined,
) {
  if (value !== undefined) {
    target[key] = value;
  }
}

/** The fields of one asset to seed or upsert; shared with `POST /__mock/assets`. */
export function readAssetInput(c: Checker): AssetInput | undefined {
  c.only(ASSET_KEYS);
  const path = c.string('path', { required: true, nonEmpty: true });
  if (path !== undefined && !path.startsWith('/')) {
    c.fail('path', 'must be an absolute path');
  }
  const input: AssetInput = { path: path ?? '' };
  setIfDefined(input, 'library', c.string('library', { nonEmpty: true }));
  setIfDefined(input, 'id', c.uuid('id'));
  setIfDefined(input, 'type', c.oneOf('type', ['IMAGE', 'VIDEO']));
  setIfDefined(input, 'fileCreatedAt', c.date('fileCreatedAt'));
  setIfDefined(input, 'fileModifiedAt', c.date('fileModifiedAt'));
  setIfDefined(input, 'isFavorite', c.boolean('isFavorite'));
  setIfDefined(input, 'rating', c.nullableInteger('rating', { min: -1, max: 5 }));
  setIfDefined(input, 'description', c.string('description'));
  setIfDefined(input, 'visibility', c.oneOf('visibility', VISIBILITIES));
  setIfDefined(input, 'isOffline', c.boolean('isOffline'));
  setIfDefined(input, 'trashed', c.boolean('trashed'));
  setIfDefined(input, 'make', c.nullableString('make'));
  setIfDefined(input, 'model', c.nullableString('model'));
  setIfDefined(input, 'checksum', c.string('checksum', { nonEmpty: true }));
  return path === undefined ? undefined : input;
}

interface LibraryLike {
  id: string;
  name: string;
  importPaths: readonly string[];
}

/**
 * The library an asset belongs to: the one named (by name or id) or, without a name, the one with
 * the deepest import path above `path`.
 */
export function pickLibrary<T extends LibraryLike>(
  libraries: Iterable<T>,
  ref: string | undefined,
  path: string,
): T | undefined {
  let best: T | undefined;
  let bestDepth = -1;
  for (const library of libraries) {
    if (ref !== undefined) {
      if (library.name === ref || library.id === ref) {
        return library;
      }
      continue;
    }
    for (const root of library.importPaths) {
      if (isUnder(path, root) && trimSlash(root).length > bestDepth) {
        best = library;
        bestDepth = trimSlash(root).length;
      }
    }
  }
  return best;
}

const EXTENSION_RE = /^[a-z0-9]{2,5}$/i;

/**
 * The fields of `POST /__mock/generate` and of a scenario's `generate` entries. The library is
 * named by `libraryId` (an id or a name) or by its alias `library`.
 */
export function readGenerateInput(c: Checker): GenerateInput | undefined {
  c.only(['libraryId', 'library', 'prefix', 'count', 'extension', 'startAt', 'stepMs', 'ties']);
  const byId = c.string('libraryId', { nonEmpty: true });
  const byName = c.string('library', { nonEmpty: true });
  if (byId !== undefined && byName !== undefined && byId !== byName) {
    c.fail('libraryId', 'contradicts "library": give only one of them');
  }
  const library = byId ?? byName;
  if (!c.has('libraryId') && !c.has('library')) {
    c.fail('libraryId', 'must be a string: the id or the name of the library');
  }
  const prefix = c.string('prefix', { required: true, nonEmpty: true });
  if (prefix !== undefined && !prefix.startsWith('/')) {
    c.fail('prefix', 'must start with "/", for example /mnt/photo/generated/IMG_');
  }
  const count = c.integer('count', { required: true, min: 1, max: 1_000_000 });
  const extension = c.string('extension') ?? 'jpg';
  if (!EXTENSION_RE.test(extension)) {
    c.fail('extension', 'must be a file extension such as jpg');
  }
  const startAt = c.date('startAt');
  const stepMs = c.integer('stepMs', { min: 0 }) ?? 1000;
  const ties = c.integer('ties', { min: 1 }) ?? 1;
  if (library === undefined || prefix === undefined || count === undefined) {
    return undefined;
  }
  const input: GenerateInput = { library, prefix, count, extension, stepMs, ties };
  setIfDefined(input, 'startAt', startAt);
  return input;
}

function readOptions(c: Checker | undefined): ScenarioOptions {
  const options: ScenarioOptions = {};
  if (c === undefined) {
    return options;
  }
  c.only(['scanDelayMs', 'latencyMs', 'legacyUnknownFields', 'unknownAssetStatus']);
  setIfDefined(options, 'scanDelayMs', c.integer('scanDelayMs', { min: 0 }));
  setIfDefined(options, 'latencyMs', c.integer('latencyMs', { min: 0 }));
  setIfDefined(
    options,
    'legacyUnknownFields',
    c.oneOf('legacyUnknownFields', ['reject', 'ignore']),
  );
  const status = c.integer('unknownAssetStatus');
  if (status === 400 || status === 404) {
    options.unknownAssetStatus = status;
  } else if (status !== undefined) {
    c.fail('unknownAssetStatus', 'must be 400 or 404');
  }
  return options;
}

function readUsers(list: Checker[] | undefined): ScenarioUser[] {
  const users: ScenarioUser[] = [];
  list?.forEach((c, index) => {
    c.only(['id', 'email', 'password', 'name', 'isAdmin']);
    const email = c.string('email', { required: true, nonEmpty: true });
    const password = c.string('password', { required: true, nonEmpty: true });
    const name = c.string('name');
    const isAdmin = c.boolean('isAdmin');
    const id = c.uuid('id');
    const same = users.findIndex((user) => user.email === email);
    if (same >= 0) {
      c.fail('email', `is already used by users[${same}]`);
    }
    if (email !== undefined && password !== undefined) {
      users.push({
        id: id ?? stableUuid('user', email),
        email,
        password,
        name: name ?? email.split('@')[0] ?? email,
        isAdmin: isAdmin ?? false,
      });
    } else {
      users.push({ id: '', email: '', password: '', name: `users[${index}]`, isAdmin: false });
    }
  });
  return users;
}

function readLibraries(list: Checker[] | undefined, users: ScenarioUser[]): ScenarioLibrary[] {
  const libraries: ScenarioLibrary[] = [];
  list?.forEach((c) => {
    c.only(['id', 'name', 'importPaths', 'exclusionPatterns', 'owner']);
    const name = c.string('name', { required: true, nonEmpty: true });
    const importPaths = c.stringList('importPaths') ?? [];
    for (const path of importPaths) {
      if (!path.startsWith('/')) {
        c.fail('importPaths', `must hold absolute paths, got ${JSON.stringify(path)}`);
      }
    }
    const exclusionPatterns = c.stringList('exclusionPatterns') ?? [];
    const owner = c.string('owner');
    if (owner !== undefined && !users.some((user) => user.email === owner)) {
      c.fail('owner', `must be the email of a user, got ${JSON.stringify(owner)}`);
    }
    const id = c.uuid('id');
    const same = libraries.findIndex((library) => library.name === name);
    if (same >= 0) {
      c.fail('name', `is already used by libraries[${same}]`);
    }
    if (name !== undefined) {
      const library: ScenarioLibrary = {
        id: id ?? stableUuid('library', name),
        name,
        importPaths,
        exclusionPatterns,
      };
      setIfDefined(library, 'owner', owner);
      libraries.push(library);
    }
  });
  return libraries;
}

function readAlbums(list: Checker[] | undefined): ScenarioAlbum[] {
  const albums: ScenarioAlbum[] = [];
  list?.forEach((c) => {
    c.only(['id', 'name', 'assets']);
    const name = c.string('name', { required: true, nonEmpty: true });
    const assets = c.stringList('assets') ?? [];
    const id = c.uuid('id');
    const same = albums.findIndex((album) => album.name === name);
    if (same >= 0) {
      c.fail('name', `is already used by albums[${same}]`);
    }
    if (name !== undefined) {
      albums.push({ id: id ?? stableUuid('album', name), name, assets });
    }
  });
  return albums;
}

function readTags(list: Checker[] | undefined): ScenarioTag[] {
  const tags: ScenarioTag[] = [];
  list?.forEach((c) => {
    c.only(['id', 'name', 'value']);
    const name = c.string('name', { nonEmpty: true });
    const value = c.string('value', { nonEmpty: true });
    const id = c.uuid('id');
    if (name === undefined && value === undefined) {
      c.fail('value', 'or name is required');
      return;
    }
    const full = value ?? name ?? '';
    const same = tags.findIndex((tag) => tag.value === full);
    if (same >= 0) {
      c.fail('value', `is already used by tags[${same}]`);
    }
    tags.push({
      id: id ?? stableUuid('tag', full),
      name: name ?? full.split('/').at(-1) ?? full,
      value: full,
    });
  });
  return tags;
}

/** Checks a parsed scenario and fills in its defaults; throws one error listing every problem. */
export function parseScenario(raw: unknown, origin = 'scenario object'): Scenario {
  const errors: string[] = [];
  const c = createChecker(raw, errors, 'scenario.');
  c.only(TOP_KEYS);
  const description = c.string('description');
  const serverVersion = c.string('serverVersion');
  if (serverVersion !== undefined && parseServerVersion(serverVersion) === null) {
    c.fail('serverVersion', `must look like 3.4.0, got ${JSON.stringify(serverVersion)}`);
  }
  const options = readOptions(c.child('options'));
  const users = readUsers(c.list('users', { required: true }));
  if (users.length === 0) {
    c.fail('users', 'must contain at least 1 elements');
  }
  const libraries = readLibraries(c.list('libraries'), users);
  const assets: AssetInput[] = [];
  c.list('assets')?.forEach((item) => {
    const input = readAssetInput(item);
    if (input === undefined) {
      return;
    }
    if (pickLibrary(libraries, input.library, input.path) === undefined) {
      if (input.library === undefined) {
        item.fail('path', 'lies under no import path of a library: set "library"');
      } else {
        item.fail('library', `is not a library of this scenario: ${JSON.stringify(input.library)}`);
      }
    }
    assets.push(input);
  });
  const generate: GenerateInput[] = [];
  c.list('generate')?.forEach((item) => {
    const input = readGenerateInput(item);
    if (input === undefined) {
      return;
    }
    if (pickLibrary(libraries, input.library, input.prefix) === undefined) {
      item.fail('library', `is not a library of this scenario: ${JSON.stringify(input.library)}`);
    }
    generate.push(input);
  });
  const albums = readAlbums(c.list('albums'));
  const tags = readTags(c.list('tags'));

  if (errors.length > 0) {
    throw new ScenarioError(errors, origin);
  }
  const scenario: Scenario = { options, users, libraries, assets, generate, albums, tags };
  setIfDefined(scenario, 'description', description);
  setIfDefined(scenario, 'serverVersion', serverVersion);
  return scenario;
}

/** A scenario from a parsed object, a JSON file path, or (absent) the default scenario. */
export function loadScenario(source?: string | object): { scenario: Scenario; origin: string } {
  if (source !== undefined && typeof source !== 'string') {
    return { scenario: parseScenario(source), origin: 'scenario object' };
  }
  const file = source === undefined ? DEFAULT_SCENARIO : resolve(source);
  const origin = typeof file === 'string' ? file : file.pathname;
  let text: string;
  try {
    text = readFileSync(file, 'utf8');
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    throw new ScenarioError([`cannot read the file: ${reason}`], origin);
  }
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    throw new ScenarioError([`not valid JSON: ${reason}`], origin);
  }
  return { scenario: parseScenario(raw, origin), origin };
}
