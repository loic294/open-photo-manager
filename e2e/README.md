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
