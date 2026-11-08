import { execFile } from 'node:child_process';
import { randomBytes, randomUUID } from 'node:crypto';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import type { BackingAssetPath, MockImmich } from '../../tools/mock-immich/index.ts';
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

  static async start(): Promise<ImmichPostgres> {
    await mkdir(cache, { recursive: true });
    const directory = await mkdtemp(join(cache, 'immich-postgres-'));
    const database = new ImmichPostgres(
      directory,
      `apm-e2e-${randomBytes(12).toString('hex')}`,
      randomBytes(24).toString('hex'),
    );
    try {
      await database.initialise();
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
          (id, "ownerId", "libraryId", "isExternal", type, "originalPath", "originalFileName",
           checksum, "checksumAlgorithm", "fileCreatedAt", "fileModifiedAt", "localDateTime")
        SELECT id::uuid, "ownerId"::uuid, "libraryId"::uuid, true,
          (jsonb_populate_record(NULL::public.asset, jsonb_build_object('type', type))).type,
          "originalPath", "originalFileName", decode(checksum, 'base64'), 'sha1-path',
          "fileCreatedAt"::timestamptz, "fileModifiedAt"::timestamptz, "fileCreatedAt"::timestamp
        FROM jsonb_to_recordset(${sqlJson(assets.slice(offset, offset + 40))})
          AS x(id text, "ownerId" text, "libraryId" text, type text, "originalPath" text,
               "originalFileName" text, checksum text, "fileCreatedAt" text, "fileModifiedAt" text)
      `);
    }
    await this.refreshMock(mock);
  }

  /** Refresh mock paths from SQL after a direct write and before observing the API. */
  async refreshMock(mock: MockImmich) {
    const json = await this.sql(`SELECT COALESCE(json_agg(row_to_json(a)), '[]'::json)
      FROM (SELECT id::text AS "id", "ownerId"::text AS "ownerId",
      "libraryId"::text AS "libraryId", "originalPath", "originalFileName",
      encode(checksum, 'base64') AS checksum, "updatedAt" AS "updatedAt"
      FROM public.asset ORDER BY id) a`);
    mock.control.reconcileAssetPaths(JSON.parse(json) as BackingAssetPath[]);
  }

  async stop() {
    if (this.started) {
      await this.compose(['down', '--volumes', '--remove-orphans']);
      this.started = false;
    }
    await rm(this.directory, { recursive: true, force: true });
  }
}
