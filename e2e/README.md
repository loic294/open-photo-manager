# Real-browser end-to-end tests

This package drives the real `apm-server` binary and the production `web/dist` build in Chromium.
It has its own dependencies; tests do not replace the product with a browser/API fake.

## Running and debugging

Use Node 24+, Rust and the media tools listed in [fixtures](../fixtures/README.md). Run
`make e2e-install` once, then `CARGO_BUILD_JOBS=2 E2E_WORKERS=2 make e2e`.
`make check-e2e` checks formatting, types and the fixed-sleep guard; `make check-tools` includes it.
`make e2e-smoke` runs the five quickest acceptance cases. `E2E_ARGS` passes Playwright selectors,
for example `make e2e E2E_ARGS="specs/selection.spec.ts"`.

Use `make e2e-headed` to watch Chromium or `make e2e-report` to inspect failure screenshots,
videos, traces and backend logs. `E2E_KEEP=1` preserves private instances under `e2e/.cache/instance-*`;
restore permission-fixture modes before manually removing a preserved tree. `E2E_PROFILE=release`
selects the release binary. Bundled Chromium does not decode H.264: the video-element test still
runs, but playback runs only with `E2E_CHANNEL=chrome` and an installed Chrome.
`captureScreens(page, name)` writes light/dark viewport PNGs in `test-results/screens`;
these are review artifacts, not committed snapshots or pixel comparisons.

## Isolation and golden copy

Global setup creates one stopped, indexed golden copy in `.cache/golden`. Its signature includes the
fixture manifest, backend binary modification time/size and the content hashes of the web build.
Unchanged rebuilds of the web bundle reuse it. Remove only `.cache/golden` to force a cold golden run.
Every app copies the complete `out` directory and the stopped SQLite data directory, preserves mtimes,
and keeps symlinks as symlinks. The index uses volume-relative paths: copies work at different absolute
locations. A new process must complete a new startup task ID greater than the pre-spawn database's
maximum ID. A momentarily empty queue and terminal tasks copied from golden are not readiness.

The generator includes mode-000 permission fixtures, which cannot be copied as an ordinary user.
Instead of chmod'ing `fixtures/out` or dropping these scenarios, setup regenerates a private seed.
Only the private seed/golden storage is readable for copying; each running instance restores mode 000.
Snapshots record inaccessible entries and modes without opening them. The golden DB was indexed with
those modes in force. All generated media remains private, including the `outside` symlink targets.

Baseline settings disable `index.watch_enabled`; writing test files must not trigger a racing native
scan. Periodic rescans default to 360 minutes, trash retention to null and Immich is disconnected.
`watcher: true` explicitly enables native watching and waits for the generated watcher status.
`cold: true` uses an empty data directory and returns after health/settings, without awaiting startup.
`settings` transforms the complete generated settings document; `env` is passed to an isolated process.
Paths, port, timezone and production web directory cannot be overridden through `env`.

## Harness API

| Surface | API |
|---|---|
| Real process | `AppInstance.start({ env?, settings?, watcher?, cold? })`, `url`, `outDir`, `volumesDir`, `dataDir`, `logs()` |
| Readiness/indexing | `waitForIdle()`, `rescan(targets?: Scope[])` returns the generated `TaskDto`; `waitForTasksIdle`, `waitForFile`, `waitForIndexed` |
| Recovery | `suspend()` / `resume()` retain private directories and port; `kill()` sends SIGKILL; `restart({ newPort? })`; `stop()` always tears down |
| Mounted private fixtures | `CrossDevice.create(app, copyBytes)` (macOS, at most 4 GiB) stops the backend, mounts/discovers a new writable `.volume`, and proves distinct device IDs plus `EXDEV`; `.evidence`, `.mount`, `.image`, `close()` |
| API/disk | `api` uses generated DTOs with matching Host/Origin and `x-apm-request: 1`; `sql(query)` is read-only SQLite; `applyFixtureMutations()` runs the shared script and parses TSV |
| Fixture catalog | `Manifest.load(outDir)`, `filesOf(key)`, `file(volume,path)`, `scenario(key)`, `duplicateGroups(mode)`, `emptyDirs()` |
| Disk helpers | `snapshotTree(dir,{hash?})`, `diffTrees`, `expectTree`, `trashEntries`, `writeFile`, `touch`, `removeFile`, `makeDir`, `makeSparseFile` (maximum 4 GiB) |
| Read-only assertions | `captureReadOnly(app)`, `assertReadOnly(app,before)` compare hashed volume trees and complete files/dirs/cameras/tags/sidecar index tables |
| Browser helpers | `expectStaysFalse(condition,{forMs})`, `expectNoSeriousA11yViolations(page,{include?,exclude?})`, `captureScreens` |
| Test fixtures | `app` (test-scoped), `sharedApp` (worker-scoped, read-only), `manifest`, `mockImmich(options?)`, page objects, `allowedErrors`, `appOptions` |

`mockImmich()` starts the shared programmatic mock, uses the `fixtures` scenario and mounts
`/mnt/photos` and `/mnt/archive` on that test's private volumes. It exposes `url`, `control` and
`state`; teardown stops it and asserts that its violation list is empty. Only this external
server is fake. The connect acceptance case goes through the real Settings UI, then verifies the API.
`onApiKeyCreated(secret)` is a programmatic mock option for tests that must verify a generated
test key is absent from application logs and plaintext storage. It is not retained in mock state
or request logs and is never exposed through the mock control API.

## Private separate-device fixtures

Ordinary `photos` and `archive` fixture directories share a filesystem on macOS; logical-volume
moves there rename rather than copy. The shared `CrossDevice` helper creates a **private sparse APFS
image** under the app's directory and mounts it inside that app's private volumes before discovery.
It preflights host free space (copy size plus 1 GiB), verifies actual `st_dev` and an `EXDEV` rename,
and exposes the discovered destination volume name. Use it before creating the source/plan.
Always `await device.close()` in `finally`: stop/quiesce the app before detach.
The app remains stopped after `close()`; call `await app.restart()` before further UI/API use. The
`AppInstance.beforeCleanup()` hook detaches after process shutdown and **before any permission walk
or recursive cleanup**. A busy/failed detach throws with the exact mount/image/directory paths and
preserves them; never force detach or remove a still-mounted tree.

Calibration on this contended macOS host measured 1 GiB at 12 seconds (below acceptance), and
3 GiB at 34 then 35 seconds for the actual file task. Copy tests must retain a measured
**at least 15 seconds** gate, not a sleep or forced throttle. Use one worker for copy/mount
validation. Compare pre-existing `.apm-part` paths because a deliberate junk fixture already has
that suffix: copy/cancel/recovery must introduce no leftover parts.

`specs/system/cross-device.spec.ts` tests discovery, shutdown/detach and busy-mount fail-closed
cleanup. These are host-binary checks, not Docker or actual Windows evidence. This APFS helper
requires macOS `hdiutil`; tests must explicitly report unsupported platforms without claiming copy
coverage. Never add a production EXDEV knob, symlink volume or force-detach workaround.

The mock exposes `control.reconcileAssetPaths(rows, associations?)` as a backing-state seam. Each
asset row includes its stable identity, path, checksum, capture and local dates, update timestamp,
offline/favorite state, rating, description, visibility, and deletion timestamp. Supply a complete
snapshot from the disposable PostgreSQL database; mismatched identities, missing rows, duplicate
paths, invalid rows, or inconsistent associations fail before changing mock state.
`ImmichPostgres.start()` creates a task-owned Compose project from pinned Immich v3.3.3 images and
migrations, with a disposable TLS secret and auto-assigned loopback port. `seedMock()` writes mock
identities and assets into that schema; `refreshMock()` reads asset and album/tag state from a
single SQL snapshot. The `backedImmich` fixture owns the database, app, mock and teardown, and
requires one Playwright worker. Its focused real-schema test and the backed browser scenarios must
be run to verify SQL coherence before claiming database-backed acceptance.

The host-verified Immich browser coverage is 10 specs / 18 cases: `immich-a11y`,
`limited-mode`, `sync-nonblocking`, `connect`, `connect-errors`, `connection-boundaries`, `mapping`,
`reconnect`, `safety` and `tls`. The `backedImmich` Playwright fixture in `src/immich-test.ts`
owns PostgreSQL, the APM process and fixture-mounted mock, and requires `E2E_WORKERS=1` to avoid
overlapping Compose stacks. Its real-PostgreSQL coverage (`backed-move-outside`, `backed-rename`,
`backed-sync`, `mirror-delete`, `mirror-move` and `src/immich-postgres.test.ts`) is not
runtime-verified. A run below the 3.00 GiB Docker VM startup floor is explicitly skipped and is
not a pass; all six must run with sufficient capacity before database-backed acceptance is claimed.
The fixture fails closed if its pinned PostgreSQL image is not already local, measures Docker VM
free space without pulling, and tears down its task-owned stack if initialization leaves less than
1.50 GiB free. Its Docker-free guard tests run with `node --test src/docker-capacity.test.ts`
from `e2e/`.

## System coverage

`specs/system` covers metadata/hash/sidecar accuracy, cold startup, manual re-indexing, native
watching, host capability diagnostics, task progress/cancellation/recovery, General settings,
trash lifecycle and HTTP/media boundaries. The HTTP route inventory comes from the merged Rust
`tests/api_routes.rs` contract and is compared with the actual API routers. `src/http.ts` preserves
raw paths and duplicate headers for boundary checks that Fetch would normalize.

Run bounded selectors before the full suite. Mount/copy tests require **one worker**, even when
ordinary browser coverage uses two; a ≥15-second real copy is not interchangeable with the
64 KiB separate-device lifecycle probe. Keep exact commands, HEAD, logs and results for each run,
and distinguish skipped or unrun scenarios from passed runtime coverage.

The Settings contract protects dirty drafts with native `beforeunload` and preserves them across
Settings tabs; it does not veto in-app navigation. Enter in a text field does not submit.
Host policy tests exercise the real binary with `APM_WATCH_MODE=poll|auto|watch`, invalid values,
health/index warnings, capability badges and supported WAL startup. They do not simulate an
unsafe mount and are not evidence from an actual Windows machine or Docker container.

The metadata oracle accounts for two known generator discrepancies without altering fixtures:
failed zero-byte/unreadable extraction leaves capture fields null rather than assigning file time,
and successful UTC file-time/video extraction records offset `0` rather than the manifest's null.
The extractor's failure contract is documented in `indexer/extract.rs`; these discrepancies remain
fixture follow-up work, not changed product behavior.

## Adding coverage

Import `test` and `expect` from `src/test.ts`, not a second fixture implementation. Use `sharedApp`
only for read-only tests: its volume snapshot and logical file-index rows must stay unchanged.
External mutation helpers reject paths outside private volumes and existing symlink ancestors.
`waitForIndexed` tolerates only the expected `folder_not_found` response while a new directory is
not indexed yet; other API failures remain errors.
Use `app` for mutations and `test.use({ appOptions: ... })` for startup options.
Choose media by manifest scenario key with `fixture(manifest,key)`; its recorded skip reason is
used only when the generator could not build that scenario.

Page objects live one per file in `src/pages/`, without a barrel. Add their fixture entries to
`src/test.ts` alphabetically. Specs call actions and read state through these objects. Prefer roles,
labels and text; existing product `data-name`, `data-date` and `data-pane-body` hooks identify
otherwise unnamed subparts. **No new `data-testid` attributes were added.** Borders are compared
with computed DaisyUI theme-token colours, not hard-coded RGB values. The viewer object waits
for its observable double-click guard to clear before a pointer Close action.

Every browser page fails on an uncaught error, console warning/error, failed request or error API
response. `allowedErrors` accepts exact message fragments or regular expressions. Explain each
allow-list at its spec: reload closes SSE, history navigation cancels pending thumbnails, and
changing/closing the viewer cancels preloads. Allow
only those named URLs with `net::ERR_ABORTED`, never general 4xx/5xx answers or connection failures.
Wrap an array override as `allowedErrors: [[pattern1, pattern2], { scope: 'test' }]`:
Playwright otherwise interprets a two-element array as a fixture tuple.

## Acceptance rules for this and subsequent E2E work

1. Start pristine or use a guarded read-only shared app; tests have no ordering dependencies.
2. Assert UI first, then API/disk for mutations. Both views must agree.
3. Wait for observable state; fixed sleeps and test retries are forbidden. `check-e2e` rejects
   `waitForTimeout` outside `src/waits.ts`. Negative time windows need a comment explaining why
   no positive event can be observed.
4. Keep failing product assertions. Fix small local product bugs (about 30 lines maximum) with
   a regression in the product's own suite and report them. Larger issues remain explicit
   findings for the coordinator; a `fixme` is not final acceptance evidence.
5. Use disposable generated fixture copies and free local ports. Ports 8080, 18080 and 15173,
   credentials, `.env` files, original fixture mutations and real Immich/media are out of scope.
