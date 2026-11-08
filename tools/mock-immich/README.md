# Mock Immich

One shared mock Immich server for development, browser checks and end-to-end tests. It answers the
calls our Rust client makes (`crates/apm-server/src/immich/`) and nothing else, scans real mounted
directories the way an Immich external library does, fails on demand, and records a **violation**
when a call would remove the original file of an online asset (the one thing our backend must never
cause). State is in memory and a scenario file seeds it: no UI, no persistence, no Docker.

Zero npm dependencies. The source is TypeScript that Node 24 runs directly by type stripping (no
build step); `tsc --noEmit` and Biome from `web/node_modules` check it. The permission list is read
from `crates/apm-server/src/immich/spec-permissions.txt`, so the package works inside this repository.

## Run it

```sh
make mock-immich                         # default scenario on http://127.0.0.1:12283
node tools/mock-immich/server.ts --help  # every option

# The libraries of the generated fixtures library (make fixtures), on a free port:
make mock-immich MOCK_IMMICH_PORT=0 \
  MOCK_IMMICH_SCENARIO=tools/mock-immich/scenarios/fixtures.json \
  MOCK_IMMICH_MOUNTS=/mnt/photos=$PWD/fixtures/out/volumes/photos,/mnt/archive=$PWD/fixtures/out/volumes/archive
node tools/mock-immich/server.ts --port 0 --scenario tools/mock-immich/scenarios/fixtures.json \
  --mount /mnt/photos=$PWD/fixtures/out/volumes/photos --mount /mnt/archive=$PWD/fixtures/out/volumes/archive
```

The only line on stdout is `{"listening":"http://127.0.0.1:12283","tls":false}` once the server is
ready (everything else goes to stderr), so a harness waits for that line and reads the URL from it;
`--port 0` picks a free port. SIGINT and SIGTERM stop it (exit 0); a usage error exits 2, a startup
error (unreadable or invalid scenario, missing mount directory, `--tls` without `openssl`) exits 1.

| Option | Meaning |
|---|---|
| `--port N` | Port to listen on (default 12283; 0 picks a free one). Agents use their own port, never 8080, 18080 or 15173. |
| `--host ADDRESS` | Address to bind (default `127.0.0.1`: the mock has no business on a network). |
| `--scenario FILE` | Scenario JSON (default `scenarios/default.json`). |
| `--mount PREFIX=DIR` | The real directory `DIR` is what the server sees at `PREFIX`, e.g. `/mnt/photos=./volumes/photos`. Repeatable. |
| `--server-version V` | The version to report and to behave as (`3.4.0`, `2.5.1`); overrides the scenario. |
| `--scan-delay-ms N` | How long a library scan waits before it looks at the disk (default 250; 0 = done before the response). |
| `--tls` | Serve https with a self-signed certificate for `localhost` and `127.0.0.1` (needs `openssl`). |

`make mock-immich` reads `MOCK_IMMICH_MOUNTS` (comma-separated `PREFIX=DIR`, no commas or spaces in a
path), `MOCK_IMMICH_PORT` and `MOCK_IMMICH_SCENARIO`; they are all optional.

## Programmatic API

`index.ts` is the stable entry point (typed; import it by its path, `../tools/mock-immich/index.ts`).

```ts
import { scenarioFiles, startMockImmich, testUsers } from '../tools/mock-immich/index.ts';

const mock = await startMockImmich({
  scenario: scenarioFiles.fixtures, // a file path, or a parsed scenario object
  mounts: { '/mnt/photos': photosDir, '/mnt/archive': archiveDir }, // the same object as --mount
  scanDelayMs: 0,
});
try {
  // ... point the backend at mock.url, log in as testUsers.admin ...
  await mock.control.scan(); // like the "scan" button of every library
  const asset = mock.state.assetByPath('/mnt/photos/2024/IMG_0001.jpg');
  assert.deepEqual(mock.state.violations(), []); // the application never force-deleted a file
} finally {
  await mock.stop();
}
```

`startMockImmich(options)` resolves to `{ url, port, host, tls, certificate?, state, control, stop() }`;
`certificate` is the PEM of a `tls` mock, to trust it from a client; `stop()` is idempotent. Options
(unknown or invalid keys are rejected with every problem listed): `port` (default 0 = free), `host`
(default `127.0.0.1`), `scenario` (path or object, default `scenarios/default.json`), `mounts`
(`{ prefix: realDir }`, every directory must exist), `serverVersion`, `scanDelayMs` (default 250),
`latencyMs`, `tls`. An explicit option beats the scenario's `options`, which beat the defaults.

Other exports: `scenarioFiles` (`default`, `limited` and `fixtures` as absolute paths), `testUsers`
(`admin`, `user`: `email`, `password`, `name`), `ScenarioError` (`errors` lists every problem of an
invalid scenario) and the types `MockImmich`, `MockImmichOptions`, `MockState`, `MockControl`,
`StateSnapshot`, `StateQuery`, `UserView`, `AlbumView`, `TagView`, `KeyView`, `AssetOptions`,
`GenerateOptions`, `AddedAssets`, `FailRuleInput`, `FailRuleView`, `GateView`, `HeldView`,
`LogEntry`, `Settings`, `Job`, `Mount`, `Scenario`, `Violation`.

**`mock.state`** reads the model directly (no HTTP, nothing in the request log):

| Method | Returns |
|---|---|
| `assets()` | every asset: `id`, `libraryId`, `ownerId`, `originalPath`, `originalFileName`, `type`, `checksum`, the dates, `deletedAt` (set = trashed), `isOffline`, `isFavorite`, `rating`, `description`, `visibility` |
| `assetByPath(path)` | one asset or `undefined` |
| `albumsOf(assetId)` | `[{ id, name }]` |
| `violations()` | forced deletes of online assets (see below); expect `[]` |
| `requests(since?)` | `{ entries, next }`: the request log (`method`, `path`, `status`, `auth`, `keyName`, `unexpected`, `injected`, `bodySummary`) |
| `users()`, `libraries()`, `albums()`, `tags()`, `keys()`, `jobs()` | the rest of the model (no passwords, no secrets) |
| `snapshot(query?)` | what `GET /__mock/state` answers |

**`mock.control`** changes the mock while it runs; every method has an HTTP twin below:

| Method | Effect |
|---|---|
| `scan(library?)` | Scans one library (id or name) or all of them and settles when every scan is over; returns the jobs, including `failed` ones. |
| `fail(rule)`, `failures()`, `clearFailures()` | Failure injection. |
| `gate(match, { times? })`, `releaseGate()`, `waitForHeld(count, timeoutMs?)` | Hold matching requests open, let them go, wait until `count` are held. |
| `revokeKey(name?)` | Deletes the API keys (all, or those named `name`): the next call with one gets a 401; returns how many. |
| `latency(ms)` | Delays every `/api` request. |
| `addAssets(inputs)`, `generate(options)` | Create assets (or change the ones that exist, matched by path); `generate` bulk-creates numbered ones. |
| `reset()` | Seeds the scenario again; clears sessions, keys, failure rules, gates, latency and the log. |

## Control API (`/__mock/*`)

Never under `/api`: it bypasses authentication, the request log, latency, failure rules and gates, and
needs no `x-apm-request` header (that one is our backend's). Bodies are JSON; unknown fields are a 400.

| Request | Effect |
|---|---|
| `GET /__mock/state?path=&limit=&offset=` | Settings, mounts, users (no passwords), libraries, a page of assets (`path` filters by substring of the original path, default limit 100, max 5000, `total` counts every match), albums, tags, keys (no secrets), jobs, `violations`, failure rules, gates, held requests. |
| `GET /__mock/log?since=N` | `{ entries, next }`; `next` is the `since` of the following call. |
| `POST /__mock/reset` | Back to the scenario. |
| `POST /__mock/scan` | `{ libraryId? }` (id or name; send `{}` for all): scans synchronously and answers `{ jobs }`. |
| `POST /__mock/assets` | `{ assets: [{ path, library?, id?, type?, fileCreatedAt?, fileModifiedAt?, isFavorite?, rating?, description?, visibility?, isOffline?, trashed?, make?, model?, checksum? }] }`: creates the assets, or changes the fields given of those that exist. `{ created, updated, items }`. |
| `POST /__mock/generate` | `{ libraryId, prefix, count, extension?, startAt?, stepMs?, ties? }`: creates `count` assets `prefix000000.jpg`, `prefix000001.jpg`, ... (capture time moves `stepMs` every `ties` assets), none or all. 200,000 take seconds. |
| `POST /__mock/latency` | `{ ms }`. |
| `POST /__mock/revoke-key` | `{ name? }`: `{ revoked }`. |
| `POST\|GET\|DELETE /__mock/fail` | Add, list and clear failure rules. |
| `POST /__mock/gate`, `GET /__mock/gate`, `POST /__mock/gate/release` | Hold, inspect (`?waitHeld=N&timeoutMs=` answers once N are held, a 408 otherwise) and release. |

A failure rule is `{ match, status, times?, after?, delayMs? }`. `match` is `[METHOD ]/api/path`
where `*` stands for any text (`PUT /api/assets`, `/api/search/*`); `status` is an HTTP error
(400-599), `'drop'` (close the connection) or `'slow'` (answer late, normally; `delayMs`, default
5000); the first `after` matching requests pass, then the next `times` (all when absent) fail. A gate
(`{ match, times? }`) holds matching requests open until released: for a task that is cancelled or
disconnected midway. Both leave `injected` in the request log.

## Scenarios

`scenarios/*.json`, validated with every problem listed (`ScenarioError`). Ids are derived from names,
so they are the same on every run. Test accounts are obviously fake:

| Name | Contents |
|---|---|
| `default` | `admin@example.test` / `admin-password` (admin), `user@example.test` / `user-password` (regular). Libraries `Photos` (`/mnt/photo`, exclusion `**/@eaDir/**`) and `Videos` (`/mnt/video`). Albums Holidays, Family, To print; tags Travel, Family/Birthdays. No assets. |
| `fixtures` | The default users, albums and tags with the libraries of `make fixtures` (`fixtures/out/volumes`): `Photos` (`/mnt/photos`, exclusion `**/@eaDir/**`) and `Archive` (`/mnt/archive`). No assets: mount `/mnt/photos` and `/mnt/archive` and scan. |
| `limited` | Only the regular user, `user@example.test` / `user-password`, on a v2.5.1 server (flat search). The libraries `Photos` (`/mnt/photo`) and `Videos` (`/mnt/video`) exist but a non-admin gets a 403 on them: our client's limited mode. Album Holidays, tag Travel. |

A scenario has `description`, `serverVersion`, `options` (`scanDelayMs`, `latencyMs`,
`legacyUnknownFields`: `reject` (a pre-3.2 server that validates strictly answers 400 to the typed
search body) or `ignore`, `unknownAssetStatus`: 400 or 404), `users` (`email`, `password`, `name`,
`isAdmin`), `libraries` (`name`, `importPaths`, `exclusionPatterns`, `owner`), `assets` (the fields
of `POST /__mock/assets`), `generate` (the fields of `POST /__mock/generate`), `albums` (`name`,
`assets`: paths of seeded assets) and `tags` (`value` such as `Family/Birthdays`, `name`). Every
object may carry a fixed `id`. Assets normally come from scanning mounted directories instead.

## What it models

Under `/api`, with the shapes of the OpenAPI spec (v3.4.0-rc.0) and Immich's error body
`{ message, error, statusCode }`; anything else is a 404 that the request log marks `unexpected`.

- `POST /auth/login` (201), `POST /auth/logout`; `POST /api-keys` (201, the secret once; permissions
  outside `spec-permissions.txt` are a 400), `DELETE /api-keys/:id`. A session bearer token works on
  those routes only; every other route takes the `x-api-key` header, and an unknown, deleted or
  revoked key is a 401, a missing permission a 403 (the permission of a route is data, not code).
- `GET /server/ping|version|about`, `GET /users/me`.
- Libraries, admin only (a regular account gets a 403, which is what limited mode relies on):
  `GET /libraries[/:id]`, `PUT /libraries/:id` (name, import paths, exclusion patterns),
  `POST /libraries/:id/scan` (204; a job that ends after `scanDelayMs`; scans of one library never overlap).
  `assetCount` counts the library's non-trashed assets.
- `POST /search/metadata` in both generations, picked by the advertised version like the client does:
  the typed `filter`/`orderBy`/`cursor` body from 3.2, flat fields and `page` before, with real paging
  (`size` up to 1,000, `nextCursor` or `nextPage`). The generations differ on omitted fields as the
  client documents: the typed filter has no implicit trash or visibility filter, the flat form hides
  trashed assets (unless `withDeleted`, which `isOffline: true` implies) and everything but archive
  and timeline assets, and its name and path fields are substring matches. Locked assets are never
  returned.
- `GET /assets/:id`, `PUT /assets` (`ids`, `isFavorite`, `rating`, `visibility`, `description`),
  `DELETE /assets` (`ids`, `force`). An unknown, foreign or locked id is a 400 `Not found or no
  asset.read access` (404 with `unknownAssetStatus: 404`), and one such id refuses the whole call.
  Without `force` the asset is trashed (`deletedAt`); with `force` it is removed, see violations.
- `GET /albums?assetId=`, `PUT /albums/:id/assets` (a result per id, including `duplicate`),
  `GET /tags`, `PUT /tags/assets` (`{ count }`). There is no album or tag creation: the scenario seeds them.

**Scanning.** `--mount`/`mounts` say which real directory stands for which server path. A scan walks
each import path that lies under a mount and skips paths that match an exclusion pattern (`**`, `*`
and `?` globs), hidden entries (names starting with a dot) and files whose extension is not a known
photo or video one. A new file becomes an asset (path-based checksum, dates from `stat`); a changed
mtime refreshes the dates; a file that is gone makes its asset `isOffline` and trashed; a file that
returns restores it. A moved file is therefore one offline, trashed asset at the old path and one new
asset at the new path, with no album, favorite or tag, exactly what Immich shows. Paths under no
mount are never scanned, so assets seeded for them stay as they are. Symlinked files are followed and
symlinked directories are not; an unlistable nested directory is skipped, but an import path that
cannot be listed fails the job (`failed`) and takes nobody offline.

**Violations.** In Immich a forced delete of an asset whose original file is still online can remove
that file. The mock records `{ kind: 'online-asset-force-deleted', assetId, path, libraryId,
fileRemoved, at }` for every forced delete of a non-offline asset, and deletes the real file when a
mount says where it lives. An offline asset can be force-deleted without a violation (its file is
already gone). Every test or smoke that drives our backend asserts that `violations()` is empty.

## Simplifications

- EXIF is not read: capture time is the file's `mtime`, `make` and `model` come from the scenario or
  `POST /__mock/assets`. Deletion is synchronous (no trash retention, no empty-trash job).
- One asset per path. The glob dialect is `**`, `*` and `?`. Albums and tags are global (not
  per owner), and the album list honours the `assetId` filter only. Import paths are not checked
  for existence and cannot span several mounts.
- Sessions are accepted on login, logout and the API-key routes only; there is no user management.
- `deletedAt` is in `state` but not in the API's asset view (the spec has none): `isTrashed` tells.
- A scan skips nested directories it cannot list. `addAssets` is not atomic when an upsert fails.

## Tests and the smoke

```sh
make check-tools          # Biome, tsc --noEmit and node --test, about 10 s
make mock-immich-smoke    # the real backend against the mock
```

`test/*.test.ts` run over real HTTP on port 0 (and `--tls` when `openssl` exists); their scratch
directories live in `tools/mock-immich/.scratch` (git-ignored; `MOCK_IMMICH_SCRATCH_DIR` moves it).

`smoke.mjs` proves the mock agrees with the **real** `apm-server`: it runs `cargo build -p apm-server`
(a no-op when current), copies `fixtures/out/volumes` (volumes `photos` and `archive`; a tiny
generated tree of the same names when it is absent) into a scratch directory, starts the mock (the
`fixtures` scenario, `/mnt/photos` and `/mnt/archive` mounted on the copies) and the backend, and
drives the backend over HTTP with `fetch`: volumes, mock scan, connect (wrong password, regular
user in limited mode, admin in full mode), libraries with asset counts, mappings, exclude-trash,
status check, revoked key then `needs_reconnect`, reconnect, TLS mock (untrusted: unreachable, with
`allow_insecure_tls`: connected), disconnect revokes the key, and finally no `unexpected` request
and no violation. Steps whose backend feature is not on `main` yet (index rescan, detect-prefix,
sync) print `skip`; they have marked extension points for the sync and outbox flows. It prints one
line per step, and on a failure the mock's request log and the output of both processes; it removes
its processes and directories either way. `APM_SERVER_BIN` uses a given backend binary as it is.

## Who starts it, and how

- **Settings UI and manual checks** (a browser against `make dev-server` or a backend of your own):
  `make fixtures`, then run the `fixtures` command from "Run it" with a port of your own. In the
  Immich settings use the printed URL, `admin@example.test` and `admin-password` (or the regular
  user for limited mode). `curl -X POST http://127.0.0.1:PORT/__mock/scan -H 'content-type:
  application/json' -d '{}'` makes the libraries show what is on disk. Start it with `--tls` to try
  "Allow a self-signed certificate".
- **Sync and outbox**: their Rust tests use the in-process fake. To drive a real backend over real
  directories use `startMockImmich` or the CLI with the mounts of the volumes the backend serves;
  call `control.scan()` after the backend moves or trashes files, `control.fail` and `control.gate`
  for failure and cancellation paths, and `generate` for load (`POST /__mock/generate`, e.g. 200,000
  assets under `/mnt/photos/gen/IMG_`).
- **End-to-end suite**: start it from the global setup with
  `startMockImmich({ scenario: scenarioFiles.fixtures, mounts: { '/mnt/photos': <copy>/photos, '/mnt/archive': <copy>/archive }, scanDelayMs: 0 })`
  where `<copy>` is the copy of `fixtures/out/volumes` the backend serves (never the shared output),
  `control.reset()` between tests, and assert `state.violations()` is empty at the end.

## The Rust fake and this mock

`crates/apm-server/tests/immich_support/mod.rs` is the in-process Rust fake: fast, no process, for
Rust integration tests. This mock serves everything that needs a real HTTP server over real
directories: the real binary, browser checks, end-to-end tests and load tests. They model the same API,
so a change to a request or response shape, a modelled behaviour or the permission list is made in
BOTH (the permission list is shared through `spec-permissions.txt`), and `make mock-immich-smoke`
is the check that the real backend works against this mock.
