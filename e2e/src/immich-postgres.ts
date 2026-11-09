import { execFile } from 'node:child_process';
import { randomBytes, randomUUID } from 'node:crypto';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import type {
  BackingAssetPath,
  BackingAssociations,
  MockImmich,
  MockImmichOptions,
} from '../../tools/mock-immich/index.ts';
import {
  type CapacityMeasurer,
  measureDockerVmFreeKiB,
  requirePostStartCapacity,
  requireStartCapacity,
} from './docker-capacity.ts';
import { cache, root } from './paths.ts';

const exec = promisify(execFile);
const DATABASE = 'apm_immich_test_v333';
const MARKER = 'apm-immich-v3.3.3-disposable';
const LAST_MIGRATION = '1790693088454-AddPersonUserTableSharedBySharedWithConstraint';

function sqlJson(value: unknown): string {
  const encoded = Buffer.from(JSON.stringify(value)).toString('base64');
  return `convert_from(decode('${encoded}', 'base64'), 'UTF8')::jsonb`;
}

async function command(program: string, args: string[], env: NodeJS.ProcessEnv): Promise<string> {
  try {
    const result = await exec(program, args, {
      cwd: root,
      env,
      timeout: 180_000,
      maxBuffer: 32 * 1024 * 1024,
    });
    return result.stdout.trim();
  } catch (error) {
    throw new Error(`${program} failed while running ${args.slice(0, 3).join(' ')}`, {
      cause: error,
    });
  }
}

async function commandWithInput(
  program: string,
  args: string[],
  input: string,
  env: NodeJS.ProcessEnv,
): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = execFile(
      program,
      args,
      { cwd: root, env, timeout: 180_000, maxBuffer: 32 * 1024 * 1024 },
      (error, stdout) => {
        if (error) {
          reject(
            new Error(`${program} failed while running ${args.slice(0, 3).join(' ')}`, {
              cause: error,
            }),
          );
        } else {
          resolve(stdout.trim());
        }
      },
    );
    if (!child.stdin) {
      reject(new Error(`${program} did not provide standard input`));
      return;
    }
    child.stdin.on('error', reject);
    child.stdin.end(input);
  });
}

/**
 * One private Compose project per test. It uses the direct-writer gate's pinned
 * images and actual Immich migrations, never a hand-written substitute schema.
 */
export class ImmichPostgres {
  readonly directory: string;
  readonly secretFile: string;
  private readonly project: string;
  private readonly composeFile: string;
  private readonly env: NodeJS.ProcessEnv;
  private started = false;

  private constructor(directory: string, project: string, password: string) {
    this.directory = directory;
    this.project = project;
    this.secretFile = join(directory, 'database-url');
    this.composeFile = join(directory, 'compose.yml');
    this.env = {
      ...process.env,
      APM_IMMICH_TEST_DIR: directory,
      APM_IMMICH_TEST_PASSWORD: password,
    };
  }

  static async start(measure: CapacityMeasurer = measureDockerVmFreeKiB): Promise<ImmichPostgres> {
    const initialCapacity = await requireStartCapacity(measure);
    await mkdir(cache, { recursive: true });
    const directory = await mkdtemp(join(cache, 'immich-postgres-'));
    const database = new ImmichPostgres(
      directory,
      `apm-e2e-${randomBytes(12).toString('hex')}`,
      randomBytes(24).toString('hex'),
    );
    try {
      await database.initialise();
      await requirePostStartCapacity(initialCapacity, measure);
      return database;
    } catch (error) {
      try {
        await database.stop();
      } catch (cleanupError) {
        throw new AggregateError([error, cleanupError], 'PostgreSQL setup and teardown failed');
      }
      throw error;
    }
  }

  private compose(args: string[]) {
    return command(
      'docker',
      ['compose', '-p', this.project, '-f', this.composeFile, ...args],
      this.env,
    );
  }

  private async initialise() {
    const tls = join(this.directory, 'tls');
    await mkdir(tls);
    const ca = join(tls, 'ca.crt');
    const caKey = join(tls, 'ca.key');
    const key = join(tls, 'server.key');
    const csr = join(tls, 'server.csr');
    const cert = join(tls, 'server.crt');
    await command(
      'openssl',
      [
        'req',
        '-x509',
        '-newkey',
        'rsa:2048',
        '-nodes',
        '-days',
        '1',
        '-keyout',
        caKey,
        '-out',
        ca,
        '-subj',
        '/CN=APM disposable test CA',
        '-addext',
        'basicConstraints=critical,CA:TRUE',
      ],
      this.env,
    );
    await command(
      'openssl',
      [
        'req',
        '-new',
        '-newkey',
        'rsa:2048',
        '-nodes',
        '-keyout',
        key,
        '-out',
        csr,
        '-subj',
        '/CN=localhost',
        '-addext',
        'subjectAltName=DNS:localhost',
        '-addext',
        'basicConstraints=critical,CA:FALSE',
        '-addext',
        'extendedKeyUsage=serverAuth',
      ],
      this.env,
    );
    await command(
      'openssl',
      [
        'x509',
        '-req',
        '-in',
        csr,
        '-CA',
        ca,
        '-CAkey',
        caKey,
        '-CAcreateserial',
        '-days',
        '1',
        '-copy_extensions',
        'copy',
        '-out',
        cert,
      ],
      this.env,
    );
    await Promise.all([chmod(key, 0o600), chmod(caKey, 0o600)]);
    const template = await readFile(join(root, 'docker/immich-postgres-test.compose.yml'), 'utf8');
    const binding = '127.0.0.1:25432:5432';
    if (!template.includes(binding)) throw new Error('Pinned Compose port binding changed');
    await writeFile(this.composeFile, template.replace(binding, '127.0.0.1::5432'));
    this.started = true;
    await this.compose(['up', '-d', '--wait']);
    const version = await this.compose([
      'exec',
      '-T',
      'server',
      'node',
      '-p',
      'require("/usr/src/app/server/package.json").version',
    ]);
    if (version !== '3.3.3') throw new Error(`Unexpected Immich version: ${version}`);
    const deadline = Date.now() + 180_000;
    for (;;) {
      try {
        const latest = await this.rawSql(
          'SELECT name FROM public.kysely_migrations ORDER BY name DESC LIMIT 1',
        );
        if (latest === LAST_MIGRATION) break;
      } catch (error) {
        if (Date.now() >= deadline) throw error;
      }
      if (Date.now() >= deadline) throw new Error('Immich v3.3.3 migrations did not finish');
      await delay(500);
    }
    const contract = await readFile(
      join(root, 'crates/apm-server/src/immich/direct_db/contract.rs'),
      'utf8',
    );
    const expected = /const MIGRATIONS: &str = "\\\n([\s\S]*?)";/.exec(contract)?.[1];
    if (!expected) throw new Error('Cannot read the direct-writer migration contract');
    const actual = await this.rawSql('SELECT name FROM public.kysely_migrations ORDER BY name');
    if (actual !== expected) throw new Error('Immich migration list does not match v3.3.3');
    await this.compose(['stop', 'server', 'redis']);
    await this.rawSql(`COMMENT ON DATABASE ${DATABASE} IS '${MARKER}'`);
    const portText = await this.compose(['port', 'database', '5432']);
    const port = /^127\.0\.0\.1:(\d+)$/.exec(portText)?.[1];
    if (!port) throw new Error(`Unexpected disposable PostgreSQL binding: ${portText}`);
    const url = new URL(`postgresql://postgres@localhost:${port}/${DATABASE}`);
    url.password = this.env.APM_IMMICH_TEST_PASSWORD ?? '';
    url.searchParams.set('sslmode', 'verify-full');
    url.searchParams.set('sslrootcert', ca);
    await writeFile(this.secretFile, `${url.toString()}\n`, { mode: 0o600 });
  }

  private rawSql(sql: string, guard?: string) {
    return this.compose([
      'exec',
      '-T',
      'database',
      'psql',
      '-X',
      '-qAt',
      '-v',
      'ON_ERROR_STOP=1',
      '-U',
      'postgres',
      '-d',
      DATABASE,
      ...(guard ? ['-c', 'BEGIN', '-c', guard] : []),
      '-c',
      sql,
      ...(guard ? ['-c', 'COMMIT'] : []),
    ]);
  }

  /** Query only the marked, test-owned database. All SQL is supplied by the test. */
  async sql(sql: string) {
    if (sql.includes('\\')) throw new Error('psql meta-commands are not allowed');
    const guard = `DO $$ BEGIN IF current_database() <> '${DATABASE}' OR
      (SELECT shobj_description(oid, 'pg_database') FROM pg_database
       WHERE datname = current_database()) IS DISTINCT FROM '${MARKER}'
      THEN RAISE EXCEPTION 'disposable database marker missing'; END IF; END $$;`;
    return this.rawSql(sql, guard);
  }

  /** Seed mock identities and scanned assets into the upstream schema, with matching UUIDs. */
  async seedMock(mock: MockImmich) {
    const group = randomUUID();
    const users = mock.state.users();
    const libraries = mock.state.libraries();
    const assets = mock.state.assets();
    const albums = mock.state.albums();
    const tags = mock.state.tags();
    const owner = users.find((user) => user.isAdmin);
    if (!owner) throw new Error('The backed fixture needs an admin user for its tags');
    await this.sql(`
      INSERT INTO public.cluster_group (id) VALUES ('${group}'::uuid);
      INSERT INTO public."user" (id, email, "clusterGroupId")
      SELECT id::uuid, email, '${group}'::uuid
      FROM jsonb_to_recordset(${sqlJson(users)}) AS x(id text, email text);
      INSERT INTO public.library (id, name, "ownerId", "importPaths", "exclusionPatterns")
      SELECT id::uuid, name, "ownerId"::uuid,
        ARRAY(SELECT jsonb_array_elements_text("importPaths")),
        ARRAY(SELECT jsonb_array_elements_text("exclusionPatterns"))
      FROM jsonb_to_recordset(${sqlJson(libraries)})
        AS x(id text, name text, "ownerId" text, "importPaths" jsonb, "exclusionPatterns" jsonb)
    `);
    for (let offset = 0; offset < assets.length; offset += 40) {
      await this.sql(`
        INSERT INTO public.asset
          (id, "ownerId", "libraryId", "isExternal", "isOffline", "isFavorite", "deletedAt", type, visibility,
           "originalPath", "originalFileName", checksum, "checksumAlgorithm",
           "fileCreatedAt", "fileModifiedAt", "localDateTime")
        SELECT id::uuid, "ownerId"::uuid, "libraryId"::uuid, true,
          "isOffline", "isFavorite", "deletedAt"::timestamptz,
          (jsonb_populate_record(NULL::public.asset, jsonb_build_object('type', type))).type,
          (jsonb_populate_record(NULL::public.asset, jsonb_build_object('visibility', visibility))).visibility,
          "originalPath", "originalFileName", decode(checksum, 'base64'), 'sha1-path',
          "fileCreatedAt"::timestamptz, "fileModifiedAt"::timestamptz, "localDateTime"::timestamptz
        FROM jsonb_to_recordset(${sqlJson(assets.slice(offset, offset + 40))})
          AS x(id text, "ownerId" text, "libraryId" text, type text, "originalPath" text,
               "originalFileName" text, checksum text, "fileCreatedAt" text, "fileModifiedAt" text,
               "localDateTime" text, "isOffline" boolean, "isFavorite" boolean, "deletedAt" text,
               visibility text)
      `);
      await this.sql(`
        INSERT INTO public.asset_exif ("assetId", rating, description)
        SELECT id::uuid, rating, description
        FROM jsonb_to_recordset(${sqlJson(assets.slice(offset, offset + 40))})
          AS x(id text, rating integer, description text)
      `);
    }
    await this.sql(`
      INSERT INTO public.album (id, "albumName")
      SELECT id::uuid, name FROM jsonb_to_recordset(${sqlJson(albums)})
        AS x(id text, name text);
      INSERT INTO public.tag (id, "userId", value)
      SELECT id::uuid, '${owner.id}'::uuid, value FROM jsonb_to_recordset(${sqlJson(tags)})
        AS x(id text, value text);
      INSERT INTO public.album_asset ("albumId", "assetId")
      SELECT x.id::uuid, a.value::uuid
      FROM jsonb_to_recordset(${sqlJson(albums)}) AS x(id text, "assetIds" jsonb),
        jsonb_array_elements_text(x."assetIds") AS a(value);
      INSERT INTO public.tag_asset ("tagId", "assetId")
      SELECT x.id::uuid, a.value::uuid
      FROM jsonb_to_recordset(${sqlJson(tags)}) AS x(id text, "assetIds" jsonb),
        jsonb_array_elements_text(x."assetIds") AS a(value)
    `);
    await this.refreshMock(mock);
  }

  /** Refresh asset and association state in one SQL snapshot before observing the API. */
  async refreshMock(mock: MockImmich) {
    const json = await this.sql(`SELECT json_build_object(
      'assets', (SELECT COALESCE(json_agg(row_to_json(a)), '[]'::json)
        FROM (SELECT asset.id::text AS "id", asset."ownerId"::text AS "ownerId",
          asset."libraryId"::text AS "libraryId", asset."originalPath", asset."originalFileName",
          encode(asset.checksum, 'base64') AS checksum, asset."fileCreatedAt",
          asset."fileModifiedAt", asset."localDateTime", asset."updatedAt", asset."isOffline",
          asset."isFavorite", asset."deletedAt", asset.visibility::text AS visibility,
          exif.rating, COALESCE(exif.description, '') AS description
          FROM public.asset LEFT JOIN public.asset_exif exif ON exif."assetId" = asset.id
          ORDER BY asset.id) a),
      'albums', (SELECT COALESCE(json_agg(row_to_json(a)), '[]'::json)
        FROM (SELECT album.id::text AS id, album."albumName" AS name,
          COALESCE((SELECT json_agg(aa."assetId"::text ORDER BY aa."assetId")
            FROM public.album_asset aa WHERE aa."albumId" = album.id), '[]'::json) AS "assetIds"
          FROM public.album ORDER BY album.id) a),
      'tags', (SELECT COALESCE(json_agg(row_to_json(t)), '[]'::json)
        FROM (SELECT tag.id::text AS id, tag.value AS name,
          COALESCE((SELECT json_agg(ta."assetId"::text ORDER BY ta."assetId")
            FROM public.tag_asset ta WHERE ta."tagId" = tag.id), '[]'::json) AS "assetIds"
          FROM public.tag ORDER BY tag.id) t))`);
    const snapshot = JSON.parse(json) as BackingAssociations & { assets: BackingAssetPath[] };
    mock.control.reconcileAssetPaths(snapshot.assets, snapshot);
  }

  /** Persist an authenticated API deletion only if the database still has the same assets. */
  async deleteAssets(
    assets: Parameters<NonNullable<MockImmichOptions['beforeAssetDelete']>>[0],
    force: boolean,
    now: number,
  ) {
    const snapshot = assets.map(({ id, ownerId, libraryId, originalPath }) => ({
      id,
      ownerId,
      libraryId,
      originalPath,
    }));
    await this.sql(`
      CREATE TEMP TABLE expected_delete ON COMMIT DROP AS
        SELECT id::uuid, "ownerId"::uuid, "libraryId"::uuid, "originalPath"
        FROM jsonb_to_recordset(${sqlJson(snapshot)})
          AS x(id text, "ownerId" text, "libraryId" text, "originalPath" text);
      DO $$ DECLARE matched integer; BEGIN
        PERFORM a.id FROM public.asset a JOIN expected_delete e ON a.id = e.id FOR UPDATE OF a;
        SELECT count(*) INTO matched FROM public.asset a JOIN expected_delete e
          ON a.id = e.id AND a."ownerId" = e."ownerId" AND a."libraryId" = e."libraryId"
          AND a."originalPath" = e."originalPath";
        IF matched <> (SELECT count(*) FROM expected_delete) THEN
          RAISE EXCEPTION 'mock asset identities differ from database';
        END IF;
      END $$;
      ${
        force
          ? 'DELETE FROM public.asset WHERE id IN (SELECT id FROM expected_delete)'
          : `UPDATE public.asset SET "deletedAt" = COALESCE("deletedAt", to_timestamp(${now} / 1000.0)),
             "updatedAt" = to_timestamp(${now} / 1000.0)
             WHERE id IN (SELECT id FROM expected_delete)`
      }
    `);
  }

  /** Persist an entire proposed library scan as one guarded SQL transaction. */
  async commitScan(scan: Parameters<NonNullable<MockImmichOptions['beforeScanCommit']>>[0]) {
    const guard = `DO $$ BEGIN IF current_database() <> '${DATABASE}' OR
      (SELECT shobj_description(oid, 'pg_database') FROM pg_database
       WHERE datname = current_database()) IS DISTINCT FROM '${MARKER}'
      THEN RAISE EXCEPTION 'disposable database marker missing'; END IF; END $$;`;
    const record = `id text, "ownerId" text, "libraryId" text, "originalPath" text,
      "originalFileName" text, type text, checksum text, "fileCreatedAt" double precision,
      "fileModifiedAt" double precision, "localDateTime" double precision, "updatedAt" double precision,
      "deletedAt" double precision, "isOffline" boolean`;
    const library = sqlJson(scan.libraryId);
    const script = `
      BEGIN;
      ${guard}
      LOCK TABLE public.asset IN SHARE ROW EXCLUSIVE MODE;
      CREATE TEMP TABLE expected_scan ON COMMIT DROP AS
        SELECT * FROM jsonb_to_recordset(${sqlJson(scan.before)}) AS x(${record});
      CREATE TEMP TABLE proposed_scan ON COMMIT DROP AS
        SELECT * FROM jsonb_to_recordset(${sqlJson(scan.after)}) AS x(${record});
      ALTER TABLE expected_scan ADD PRIMARY KEY (id);
      ALTER TABLE proposed_scan ADD PRIMARY KEY (id);
      DO $$ DECLARE library_id uuid := (${library} #>> '{}')::uuid; BEGIN
        IF (SELECT count(*) FROM public.asset WHERE "libraryId" = library_id) <>
           (SELECT count(*) FROM expected_scan) OR EXISTS (
          SELECT 1 FROM expected_scan e LEFT JOIN public.asset a ON a.id = e.id::uuid
          WHERE a.id IS NULL OR a."libraryId" <> library_id
            OR a."ownerId" <> e."ownerId"::uuid OR a."originalPath" <> e."originalPath"
            OR a.checksum <> decode(e.checksum, 'base64')
            OR a."isOffline" IS DISTINCT FROM e."isOffline"
            OR a."deletedAt" IS DISTINCT FROM to_timestamp(e."deletedAt" / 1000.0)
        ) OR EXISTS (
          SELECT 1 FROM expected_scan e LEFT JOIN proposed_scan p ON p.id = e.id
          WHERE p.id IS NULL OR p."ownerId" <> e."ownerId" OR p."libraryId" <> e."libraryId"
            OR p."originalPath" <> e."originalPath"
        ) OR EXISTS (
          SELECT 1 FROM proposed_scan p LEFT JOIN expected_scan e ON e.id = p.id
          JOIN public.asset a ON a.id = p.id::uuid WHERE e.id IS NULL
        ) OR EXISTS (
          SELECT 1 FROM proposed_scan WHERE "libraryId"::uuid <> library_id
        ) THEN RAISE EXCEPTION 'mock scan differs from database'; END IF;
      END $$;
      INSERT INTO public.asset
        (id, "ownerId", "libraryId", "isExternal", "isOffline", "deletedAt", type,
         "originalPath", "originalFileName", checksum, "checksumAlgorithm",
         "fileCreatedAt", "fileModifiedAt", "localDateTime", "updatedAt")
      SELECT id::uuid, "ownerId"::uuid, "libraryId"::uuid, true, "isOffline",
        to_timestamp("deletedAt" / 1000.0),
        (jsonb_populate_record(NULL::public.asset, jsonb_build_object('type', type))).type,
        "originalPath", "originalFileName", decode(checksum, 'base64'), 'sha1-path',
        to_timestamp("fileCreatedAt" / 1000.0), to_timestamp("fileModifiedAt" / 1000.0),
        to_timestamp("localDateTime" / 1000.0), to_timestamp("updatedAt" / 1000.0)
      FROM proposed_scan
      ON CONFLICT (id) DO UPDATE SET
        "isOffline" = EXCLUDED."isOffline", "deletedAt" = EXCLUDED."deletedAt",
        "fileCreatedAt" = EXCLUDED."fileCreatedAt",
        "fileModifiedAt" = EXCLUDED."fileModifiedAt", "localDateTime" = EXCLUDED."localDateTime",
        "updatedAt" = EXCLUDED."updatedAt";
      COMMIT;
    `;
    await commandWithInput(
      'docker',
      [
        'compose',
        '-p',
        this.project,
        '-f',
        this.composeFile,
        'exec',
        '-T',
        'database',
        'psql',
        '-X',
        '-qAt',
        '-v',
        'ON_ERROR_STOP=1',
        '-U',
        'postgres',
        '-d',
        DATABASE,
      ],
      script,
      this.env,
    );
  }

  /** Simulate a database outage without touching any other Compose project. */
  async suspendDatabase() {
    await this.compose(['stop', 'database']);
  }

  async resumeDatabase() {
    await this.compose(['up', '-d', '--wait', 'database']);
  }

  async stop() {
    if (this.started) {
      await this.compose(['down', '--volumes', '--remove-orphans']);
      this.started = false;
    }
    await rm(this.directory, { recursive: true, force: true });
  }
}
