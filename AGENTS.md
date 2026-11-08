# Advanced Photo Manager (APM): rules for coding agents

A photo-management web app for a Synology NAS: a Rust/axum backend with a SQLite index (browse,
filter, duplicates, move/format, empty-folder cleanup, trash, Immich sync) and a Lit/DaisyUI
frontend, shipped as ONE Docker image. Frontend rules live in `web/AGENTS.md`.

## Layout
- `crates/apm-core/`: pure domain logic and EVERY API type (`src/dto/`). No filesystem, network,
  database or tokio code of its own (`hash` only wraps readers it is handed).
- `crates/apm-server/`: axum, sqlx, indexer, tasks, fsops, plans, immich. Library in `src/lib.rs`,
  thin binary in `src/main.rs`, SQL in `migrations/`, integration tests in `tests/`.
- `web/`: Vite + TypeScript + Lit + DaisyUI. `web/src/api/generated/` is written by ts-rs.
- `docker/`, `fixtures/` (generator, output git-ignored), `docs/`: added by the todos that own them.
- `scripts/`: coordinator tooling, leave alone. `Makefile`: the commands below.

## Commands (repo root)
- `make check` = `check-rust` + `check-web`. It must be green before you finish.
- `make check-rust`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. One test: `cargo test -p apm-server --test api_routes`.
- `make check-web`: installs `web/node_modules` when missing, then `npm --prefix web run check`.
- `make fmt` (rustfmt + Biome), `make gen-api` (regenerate every TypeScript DTO), `make build`
  (web build + release binary), `make clean` (`cargo clean`).
- `make dev-server`: backend on :8080 over `fixtures/out/volumes` and `.data` (every `APM_*` variable
  is overridable from the caller). `make dev-web`: Vite on :5173, proxies `/api`.
- Run tests with `cargo test`, not nextest: the ts-rs export tests rely on in-process locking.

## One implementation per concern (reuse, never duplicate)
| Building block | Reused by |
|---|---|
| `FileQuery` = `Scope` + `Filter` + sort to SQL (`sqlx::QueryBuilder`) | folders, items, ids, counts, facets, plans, selections, re-index scope |
| `Scope { volume_id, dir, recursive }` | every query and plan |
| Plan engine (build, overlay, execute) | duplicates, move, format, cleanup |
| `FsOps` executor (move, trash, rmdir; sidecar-aware, journaled) | every destructive task, Immich outbox hooks |
| `TaskEngine` (lanes, progress, speed/ETA, cancel, SSE) | indexing, FS ops, plan execution, Immich sync |
| `RelPath` / `VolumeRoot` | every filesystem access |
| `PatternFormatter` | move planner AND `POST /api/format/preview` (the UI never reimplements it) |
| `SidecarMatcher` | scanner, move, delete |
| `apm_core::hash` (`sha1_reader`, `HashingReader`, `Sha1Digest`) | indexer hashing, copy verification in `fsops`, Immich |

A second caller of a concern uses or extends the existing building block, never a parallel copy.

## Backend conventions
- SQL: runtime `sqlx::query` / `query_as` / `QueryBuilder` ONLY, never the compile-time `query!` /
  `query_as!` macros (the `macros` feature is for `FromRow` and `migrate!`). Bind values with
  `push_bind`; never format user data into SQL. SQLite allows 32766 binds per statement: chunk id
  lists (30_000 max, see `tests/sqlx_smoke.rs`).
- Subtree idiom ("this directory and everything below it", `Scope { dir, recursive }`): never write
  the `dir_path` range or a `LIKE` by hand. Push `db::subtree::Subtree::push_predicate` after
  `volume_id = ?`: it is an index range over `files_volume_dir_name` that stays correct for sibling
  names such as `2019.old` (reasons in the `db` module docs).
- Handlers are thin: extract with `ApiJson` / `ApiPath` / `ApiQuery` (`api/extract.rs`, rejections
  become the JSON `ErrorBody`), call a service, return DTOs. Logic lives in services and `apm-core`.
- All errors are `AppError` (`error.rs`): `bad_request`, `forbidden`, `not_found`, `conflict`
  (code + message), `internal(cause)` (logs the cause, never leaks it), `not_implemented(todo_id)`.
  `?` converts `sqlx`, `std::io` and `anyhow` errors. Alias: `AppResult`.
- No `unwrap` / `expect` in non-test code (clippy enforces it; tests are exempt). No `unsafe`
  (denied workspace-wide). Log with `tracing`; secrets (Immich key, tokens) are never logged.
- No shell for external tools (exiftool, ffmpeg): pass arguments after `--` or as `./path`.
- Hashing: `apm_core::hash` is the only SHA-1 code (`sha1_reader` with a cancel/progress callback,
  `HashingReader` where the bytes are read anyway, hex helpers). Never write a second digest loop;
  `apm-server` deliberately has no `sha1` dependency.
- Only `fsops` changes user files: move, copy, trash, restore, delete and rmdir of volume paths all go
  through `FsOps` (journaled, sidecar-aware, with hooks for the index and Immich). The scanner,
  watcher, plan executors, trash and Immich code never call `std::fs` / `tokio::fs` mutators on a
  volume path.
- Index work (scan, extract, hash, thumbnails) is started by production code only through
  `IndexJobs::submit(ScanRequest, IndexOrigin)` (`indexer/jobs.rs`), which owns de-duplication,
  concurrency and the `index` task and runs the startup and periodic rescans. `Indexer::run` stays the
  entry point for tests.
- Every foreign key column needs an index: `db/schema_tests.rs` fails the gate when a migration adds
  an unindexed one.
- Services are `Clone` structs held by `AppState` (`state.rs`). Background workers and SSE streams
  must stop on `AppState::begin_shutdown`, or shutdown waits for the 10 s grace timeout.
- Shared wiring (`lib.rs`, `state.rs`, `app.rs`, `api/mod.rs`, root `Cargo.toml`) is one item per
  line, alphabetical: touch only your own line. Every module you own is already declared, so fill
  in your files instead of adding modules. New dependency: add it to `[workspace.dependencies]`
  (alphabetical, full `x.y.z` version) and use `workspace = true` in the crate.
- Tests: unit tests beside the code; integration tests in `crates/apm-server/tests/` with
  `tests/common` (`TestApp::new().await`, `app.call(method, path, body)` adds `Host` and
  `x-apm-request`; `scratch_dir()` lives under `target/`, never use `/tmp`).
- macOS is case-insensitive and stores NFD names: do not rely on either in tests.

## API conventions
- JSON is `snake_case`, ids are integers, timestamps are epoch seconds (capture times are
  naive-local). Errors are `{"error":{"code","message"}}`.
- Every non-GET request must carry header `x-apm-request: 1` (the web client always sends it;
  GET and SSE are exempt). The security layer (todo `securing-api-baseline`) enforces it.
- CONTRACT DEVIATION from plan 5.12: browse and facets are POST with JSON bodies, not GET with
  query strings. `POST /api/browse/folders` (`BrowseRequest` -> `FoldersResponse`),
  `/api/browse/items` (`ItemsRequest` -> `ItemsResponse`), `/api/browse/ids` (`ItemsRequest` ->
  `IdsResponse`, offset/limit ignored), `/api/facets` (`BrowseRequest` -> `Facets`).
- Unknown `/api/*` paths answer JSON 404 (`route_not_found`), never the SPA; a wrong method answers
  JSON 405 (`method_not_allowed`); bad bodies, paths and queries answer `invalid_json`,
  `invalid_path` and `invalid_query`. Unimplemented endpoints answer 501 `not_implemented`.
- SSE (`GET /api/events`): every message is a single `data:` line holding one `ServerEvent` JSON
  object (tagged by `type`: `task`, `fs_change`) and NO `event:` field, so a client uses one
  `onmessage` and dispatches on `type`. The server sends `retry: 3000` once, a `: keep-alive`
  comment every 15 s, never replays missed events (after a reconnect the client refetches what it
  shows) and ends the stream when `AppState::begin_shutdown` fires.

## Browse and media contracts
Backend and frontend agents build against these in parallel; change them only in a coordinator commit.
- Thumbnails: `GET /api/files/{id}/thumb?size=sm|lg&v={mtime}`. `sm` is a JPEG with a longest edge of
  320 px, `lg` of 800 px, never upscaled. `v` is `FileItem.mtime` and only busts the browser cache (the
  server derives its own cache key). `GET /api/files/{id}/preview` is the viewer rendition (the original
  when a browser can show it, else a JPEG of at most 2560 px) and `GET /api/files/{id}/content` the
  original, with Range. All three are plain URLs for `<img>`, `<video>` and links; the API client never
  calls them. A ghost item's `id` is the id of the real file it stands for, so the same URLs work.
- Browsing answers from the index and never touches the filesystem. A pane location is
  `(volume_id, dir)`; `volume_id = null, dir = ""` is the virtual top level that lists the volumes as
  folders. A directory that does not exist (any more) answers 404 `folder_not_found`, so the UI walks up
  to the nearest ancestor that does.
- Listing order: real files first, then ghost files (`overlay.planned`, from the plan overlay); folders,
  real and planned, are merged and sorted by `names::normalize_name`. `ids` (select all) holds real files
  only, in the order of `items`; a ghost never has a selectable id and is never counted. The `Overlay`
  docs say what each flag means on a file row and on a folder row.
- `dir` and file names are the exact on-disk bytes: never normalise, lowercase or trim one you pass back
  to the server.

## Adding a DTO
1. Define it in the `apm-core/src/dto/<area>.rs` of your area (all API types live there only).
2. Derive `Debug, Clone, Serialize, Deserialize, TS` (plus `PartialEq, Eq, Default` where natural)
   and add `#[ts(export)]`. Enums use `#[serde(rename_all = "snake_case")]`. Prefer flat structs;
   with `#[serde(flatten)]` add `#[ts(flatten)]` and a JSON round-trip test. No intra-doc links in
   doc comments (they become JSDoc). Add a serde round-trip test.
3. Run `make gen-api` (or `cargo test`) and commit the generated `web/src/api/generated/Name.ts`.
   TypeScript imports it as `'@/api/generated/Name'`; never hand-write a duplicate.
4. `i64`/`u64` export as `number` (`TS_RS_LARGE_INT` in `.cargo/config.toml`). `#[serde(default)]`
   fields stay required in TypeScript, so clients send complete objects.

## Adding an endpoint
1. DTOs first (above). 2. Every plan 5.12 route already exists as a stub in `api/<file>.rs`:
replace `not_implemented` with a handler that delegates to your service. A new route goes into that
file's `routes()` (axum 0.8 syntax: `/files/{id}`). 3. Add an integration test through `TestApp`.
`tests/api_routes.rs` only needs a new `ROUTES` row for a path that is not in plan 5.12.

## Migrations
`crates/apm-server/migrations/`, run with `sqlx::migrate!`. Every todo owns a number block (rule 6 of
the parallel work protocol at the end of this file). Never edit a merged migration: add a new one.
`build.rs` makes cargo rebuild when files are added.

## Ownership (module = owning todo; edit only what you own)
- `designing-database-schema`: `db/**`, `settings/**`, `api/settings.rs`, `migrations/0001*`.
- `implementing-core-domain` agent A: apm-core `pattern`, `conflict`, `datetime`.
- `implementing-core-domain` agent B: apm-core `filter`, `sidecar`, `matching`, `junk`, `progress`,
  `kind`.
- apm-core `paths` (`RelPath`, ids), `names`, `consts` and `hash` are COMPLETE on `main`: reuse them, extend
  them only additively and with tests. Wire enums used by several modules (`FileKind`,
  `CaptureSource`, `FlagFilter`, `ConflictPolicy`, ...) live in `dto/common.rs`; never define a parallel enum.
- `implementing-scanner`: `indexer/mod.rs`, `indexer/scanner.rs`.
- `extracting-metadata`: `indexer/extract.rs`, `indexer/xmp.rs`. `hashing-files`: `indexer/hash.rs`.
  `watching-filesystem`: `indexer/watcher.rs`.
- `building-filter-query-engine`: `query/**`.
- `building-browse-api`: `api/browse.rs`, `api/files.rs`, `api/volumes.rs`.
- `serving-media`: `media/**`, `api/media.rs`. `securing-api-baseline`: `security/**`.
- `building-task-engine`: `tasks/**`, `api/tasks.rs`, `api/events.rs`.
- `implementing-safe-file-ops`: `fsops/**`. `building-index-controls`: `api/index.rs`, `indexer/jobs.rs`.
- `building-trash-management`: `trash/**`, `api/trash.rs`.
- `building-plan-engine`: `plans/{engine,store,overlay,selections}.rs`, `api/plans.rs`,
  `api/selections.rs`. `implementing-duplicate-matching`: `plans/duplicates.rs`.
  `implementing-move-planner`: `plans/moves.rs`, `api/format.rs`.
  `implementing-empty-folder-cleanup`: `plans/cleanup.rs`.
- `building-immich-client`: `immich/{client,crypto,models}.rs`, `api/immich.rs`.
  `syncing-immich-assets`: `immich/sync.rs`. `reflecting-file-ops-in-immich`: `immich/outbox.rs`.
- DTO files (`apm-core/src/dto/`): each owner edits only the file of their area; `common.rs` is
  shared, change it additively. The `plans.rs` and `immich.rs` files have one marked section per todo.
- Server paths are under `crates/apm-server/src/`, core paths under `crates/apm-core/src/`.

## Dev loop and runtime
1. Edit, then iterate with a narrow `cargo test -p <crate> <name>`; finish with `make check`.
2. UI against the real backend: `make dev-server` plus `make dev-web`, open http://localhost:5173.
   Agents in worktrees use their own ports instead (see `web/AGENTS.md`, Commands), and a
   destructive check runs on a COPY of `fixtures/out/volumes` (`APM_VOLUMES_DIR`, with a throwaway
   `APM_DATA_DIR`), never on the shared fixture output.
3. Config comes from env (`config.rs`): `APM_VOLUMES_DIR` (/volumes), `APM_DATA_DIR` (/data),
   `APM_LISTEN` (0.0.0.0:8080), `APM_ALLOWED_HOSTS`, `APM_INDEX_CONCURRENCY`, `APM_LOG` (info),
   `APM_WEB_DIR` (`./web/dist`; the Docker image sets `APM_WEB_DIR=/app/web`).

## Parallel work protocol (MANDATORY)

Many agents build this repo concurrently, each in its own git worktree, and land onto `main`
through a serialized, gated script. `main` must always build and pass `make check`.
The design plan (source of truth for behaviour) is
`<coordinator-session>/plan.md` (outside the repo).

1. **Work only in your assigned worktree** (given in your brief: `<worktrees-dir>/{todo-id}`,
   branch `feat/{todo-id}`). Every file you read, write or build must be under it. Never edit, build or run
   git commands in the main checkout `<main-checkout>`. Never delete/reset other
   worktrees, other branches or `main`. Only the coordinator manages `origin`, creates the private GitHub
   repository, and pushes branches or release tags. Agents commit and land locally; never `git push`.
2. **Stay inside your ownership boundary** (ownership map above + your brief). Shared wiring files
   (`Cargo.toml`, `crates/*/src/lib.rs`, other owners' `mod.rs`, `state.rs`, `app.rs`, `api/mod.rs`,
   `tests/common/mod.rs`, `Makefile`, `AGENTS.md`) may only receive minimal, line-oriented additions: one item
   per line, keep alphabetical order, never reformat or reorder lines you do not own. Put notes in your
   module's `//!` doc instead of AGENTS.md.
3. **Reuse, don't duplicate.** Before writing a helper, search for an existing one (see "one implementation
   per concern"). If you need a change in someone else's module, make the smallest additive change and
   mention it in your report.
4. **Commit in your worktree** as you go: `git add -A && git commit -m "feat(scope): message"`; the commit
   message must end with the trailer line
   `Co-authored-by: Copilot App <223556219+Copilot@users.noreply.github.com>`.
   Nothing is on `main` until you run the land script.
5. **Land with `scripts/wt-land.sh`** (from your worktree, everything committed). It queues behind other
   agents, rebases onto `main`, runs `make check-rust` and/or `make check-web` for what changed, then
   fast-forwards `main` and prints `LANDED: ...`. Run it synchronously with the longest wait; it may queue for
   many minutes. Never start a second instance; if your tool call times out, keep reading its output until it
   ends. Outcomes:
   - `LANDED` - done.
   - `REBASE_CONFLICT` - resolve (instructions are printed), `git rebase --continue`, re-run.
   - `GATE_FAILED` - fix the failure, commit, re-run. Never weaken a gate, skip or ignore a test, or add
     `#[allow]` to pass without a comment explaining why.
   - `GATE_DIRTY` - the gates regenerated files (usually TypeScript types in `web/src/api/generated`);
     commit them and re-run.
   - exit 5 (main checkout dirty) or 6 - wait a minute and re-run; report if it persists.
6. **Quality bar:** tests for everything you add (they are part of the todo, not optional); clippy clean
   (`-D warnings`); no `unwrap`/`expect`/`todo!`/`unimplemented!` in non-test code; Biome and `tsc` clean; no
   secrets in code or logs; follow the conventions above.
   **Migrations** live in `crates/apm-server/migrations/` and are append-only once on `main` (never edit a landed
   migration, add a new one). Use ONLY the number block of your todo so parallel agents never collide:
   `0001` core schema; `0010-0019` scanner; `0020-0029` metadata, hashing, watcher; `0030-0039` query engine,
   browse API, media; `0040-0049` task engine, file ops, trash, index controls; `0100-0199` Immich;
   `0200-0299` plan engine, planners, cleanup. Name files `{number}_{snake_case}.sql`.
7. **Do not touch the SQL todo database**; the coordinator tracks status from your report.
8. **Do not ask the user questions**; make the best decision consistent with `plan.md` and report it.
   Never read credential files (e.g. `<other-project>/.env`, `login.txt`).
9. **Final report** (at most ~250 words): what you built, test results, deviations from the plan or brief,
   anything later agents must know, and the landed commit hash. Do not paste file contents.

## Mock Immich (`tools/mock-immich/`)

ONE shared, zero-dependency Node/TypeScript mock of the Immich calls our client makes, for the real
binary, browser checks, end-to-end and load tests. Never write a throwaway Immich server. Reference:
`tools/mock-immich/README.md`.
- Run: `make mock-immich` (default scenario, port 12283; options `MOCK_IMMICH_PORT`,
  `MOCK_IMMICH_SCENARIO`, `MOCK_IMMICH_MOUNTS=/mnt/photos=DIR,/mnt/archive=DIR2`) or
  `node tools/mock-immich/server.ts --port 0 --scenario FILE --mount PREFIX=DIR` (repeatable). One
  stdout line `{"listening":"http://127.0.0.1:PORT","tls":false}` says it is ready. Use your own port,
  never 8080, 18080 or 15173.
- Scenarios (`tools/mock-immich/scenarios/`): `default` (libraries Photos `/mnt/photo`, Videos
  `/mnt/video`), `fixtures` (Photos `/mnt/photos`, Archive `/mnt/archive`: the volumes of
  `make fixtures`, so mount them on the copy of `fixtures/out/volumes` the backend serves) and
  `limited` (regular user, v2.5.1). Accounts: `admin@example.test` / `admin-password`,
  `user@example.test` / `user-password`. They seed no assets: scanning the mounts creates them.
- Programmatic: `startMockImmich({ scenario, mounts, scanDelayMs, port, tls })` from
  `tools/mock-immich/index.ts`; `scenario` is a parsed object or a file path (`scenarioFiles.fixtures`),
  `mounts` an object `{ '/mnt/photos': '<real dir>' }`. It returns `{ url, stop(), state, control }`:
  `state.assets()|assetByPath()|albumsOf()|violations()|requests()`,
  `control.scan()|fail()|gate()|revokeKey()|reset()|addAssets()|generate()`.
- Control API, never under `/api` and without the `x-apm-request` header (that one is our backend's):
  `/__mock/state|log|reset|scan|assets|generate|latency|revoke-key|fail|gate`.
- Safety: a forced delete of an online asset removes the real file and is recorded in `violations()`.
  Every test that drives our backend asserts it is empty.
- The in-process Rust fake (`crates/apm-server/tests/immich_support/mod.rs`) serves the Rust tests;
  the mock serves everything with a real HTTP server over real directories. They model the same
  API: change a shape, a modelled behaviour or `spec-permissions.txt` in BOTH, and keep
  `make mock-immich-smoke` passing: it drives the real backend against the mock, over a copy of the
  fixture volumes `photos` and `archive` with the `fixtures` scenario.
- `make check-tools` (Biome, `tsc`, `node --test`; `scripts/wt-land.sh` runs it when a path under
  `tools/` or `e2e/` changes). Other todos add their own `check-*` prerequisite to it.

## Trash (`trash/`, `api/trash.rs`; reference: the module docs of `trash`)

User-facing trash management over what `FsOps` puts in `<volume root>/.apm-trash/<batch>/<original
relative path>`: one `trash_items` row per trashed file or per whole trashed folder. A batch
(`YYYYMMDD-HHMMSS-t<task id>`, `-r<hex>` ad hoc, `-x<journal id>` for the source a copy-and-delete
move keeps) is one deletion on one volume. `reason` is `user`, `replaced`, `junk`, `moved` or
`NULL` (found on the disk, so it restores like a new file).
- `AppState.trash: TrashService` (`new(db, settings, tasks, volumes, fsops)`, `with_clock` ages the
  trash in a test): `list(TrashListQuery)`, `batches(TrashBatchesQuery)`, `summary()`,
  `restore(TrashRestoreRequest)` and `empty(TrashEmptyRequest)` (each answers a `TaskId`; a refusal
  is a `TrashError`), `sweep_retention()`, `start_background(shutdown)` (`main.rs` starts it;
  `start_background_every` serves tests) and `sync()`.
- Routes: `GET /api/trash` (`TrashListQuery` as a query string, `TrashListResponse`),
  `GET /api/trash/batches` (`TrashBatchesQuery`, `TrashBatchesResponse`) and the 202 + `TaskDto`
  POSTs `/api/trash/restore`, `/api/trash/empty`, `/api/trash/sync` (no body). Both lists embed
  `summary` (`TrashSummary`: the GLOBAL items, bytes, `max_id`, `retention_days`, the
  `conflict_policy` a restore uses and `volumes`, only those that hold trash, each with
  `online`/`writable`). A bad query or body answers one 422 `validation_failed` listing every
  field; `batch` needs `volume_id`; an unknown volume or deletion answers 404 (`batch_not_found`);
  a restore whose items all sit on one offline volume, and an empty aimed at an offline volume,
  answer 409 `volume_offline`.
- Restore is ONE `fs_move` task through `FsOps::restore`; a name that is taken again follows the
  request's `conflict_policy` (`null`: the `default_conflict_policy` setting), a skipped item stays
  in the trash and a restored item gets a new id. Items are never merged into a restored folder.
- Empty is a `trash_empty` task that deletes `id <= up_to_id`, the `max_id` the user was looking
  at: what was trashed afterwards survives (ids are `AUTOINCREMENT`, migration 0047, never reused;
  `409 trash_already_empty` when nothing matches). "Empty trash" and the retention sweeper
  (`trash_retention_days`, off by default; first look 2 minutes after boot, then hourly and at once
  when the setting changes) are the ONLY permanent deletes (A15), always through `FsOps::purge`.
- `sync` is an `fs_trash` task on the `fs` lane, NOT an `index` task (`IndexJobs` counts those as
  scans). It adopts untracked files, drops the rows whose file is gone (it keeps them when 50 or
  more, and over a fifth of the rows, vanish together: an unmounted share) and repairs the totals.
- Reads answer from the index and never touch the disk. The summary is the running totals of
  `trash_volume_totals`, kept by three triggers on `trash_items` (migration 0048): never
  `INSERT OR REPLACE` into `trash_items` (SQLite does not fire the delete trigger for the replaced
  row, so the totals would drift); delete and insert instead. On a million items (release,
  `tests/trash_perf.rs`, ignored by default): summary 0.06 ms, first page 0.23 ms, deletions
  228 ms. Not indexed on purpose: sorting by size, path or name, a search and deep offsets.
- Task outcomes: the title says what happened, a run with a failure ends `failed` and its `error`
  names the first three items with their `FsOpError` code, and a skip never fails a task. A
  client derives its toast from the title and the error (`TaskDto` has no result).
