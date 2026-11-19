# Real-browser end-to-end tests

This package drives the real `opm-server` binary and the production `web/dist` build in Chromium.
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
scan. Periodic rescans default to 360 minutes, trash retention to null and Sync Manager is disconnected.
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
| API/disk | `api` uses generated DTOs with matching Host/Origin and `x-opm-request: 1`; `sql(query)` is read-only SQLite; `applyFixtureMutations()` runs the shared script and parses TSV |
| Fixture catalog | `Manifest.load(outDir)`, `filesOf(key)`, `file(volume,path)`, `scenario(key)`, `duplicateGroups(mode)`, `emptyDirs()` |
| Disk helpers | `snapshotTree(dir,{hash?})`, `diffTrees`, `expectTree`, `trashEntries`, `writeFile`, `touch`, `removeFile`, `makeDir`, `makeSparseFile` (maximum 4 GiB) |
| Read-only assertions | `captureReadOnly(app)`, `assertReadOnly(app,before)` compare hashed volume trees and complete files/dirs/cameras/tags/sidecar index tables |
| Browser helpers | `expectStaysFalse(condition,{forMs})`, `expectNoSeriousA11yViolations(page,{include?,exclude?})`, `captureScreens` |
| Test fixtures | `app` (test-scoped), `sharedApp` (worker-scoped, read-only), `manifest`, page objects, `allowedErrors`, `appOptions` |

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
validation. Compare pre-existing `.opm-part` paths because a deliberate junk fixture already has
that suffix: copy/cancel/recovery must introduce no leftover parts.

The copy responsiveness check submits 200 logical browse/facet requests over at most 16 live
keep-alive connections, not 200 simultaneous TCP connects. Same-transport host controls observed
283 `ECONNRESET` read failures with unlimited Node HTTP sockets versus none in 1,200 requests
with 16 sockets; default Fetch separately produced connect resets. This is empirical connection-burst
sensitivity, not proof of kernel backlog overflow. Requests retain absolute 15-second deadlines,
must return HTTP 200 and valid JSON, and all settle before agent cleanup; no retries hide failures.

`specs/system/cross-device.spec.ts` tests discovery, shutdown/detach and busy-mount fail-closed
cleanup. These are host-binary checks, not Docker or actual Windows evidence. This APFS helper
requires macOS `hdiutil`; tests must explicitly report unsupported platforms without claiming copy
coverage. Never add a production EXDEV knob, symlink volume or force-detach workaround.

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
Host policy tests exercise the real binary with `OPM_WATCH_MODE=poll|auto|watch`, invalid values,
health/index warnings, capability badges and supported WAL startup. They do not simulate an
unsafe mount and are not evidence from an actual Windows machine or Docker container.

The metadata oracle accounts for two known generator discrepancies without altering fixtures:
failed zero-byte/unreadable extraction leaves capture fields null rather than assigning file time,
and successful UTC file-time/video extraction records offset `0` rather than the manifest's null.
The extractor's failure contract is documented in `indexer/extract.rs`; these discrepancies remain
fixture follow-up work, not changed product behavior.

## Shared real-copy fs-lane boundary

`src/fs-lane.ts` shares the Tools move mechanism with queued cancellation/restart suites:

- `startFileMove(app, page, source, movePage, confirmDialog, options)` starts a **UI-confirmed**
  single-file move and returns `{ id, planId }`. Options are `sourceVolume`, `sourceDir`,
  `fileName`, `destinationVolume` and `fileBytes`. It checks the server selection count/bytes
  and confirmation before executing; it does not create files, mount devices or hide errors.
- `observeRunningCopy(app, id)` returns `{ task, observedAt }` only after observing a running
  task with positive, incomplete byte progress. Use only a real cross-device copy as the blocker.
- `expectQueuedBehindCopy(app, copy, queuedId)` checks, in the **same task-list response**,
  that the blocker is still running with positive, incomplete progress and the caller is queued.
  Checking the queued task alone is not evidence that the copy occupied the lane.
- `waitForSustainedCopy(app, copy)` requires the task still to be running at least 15 seconds
  after observation (45-second polling budget). A copy finishing early fails; no fake occupancy
  or production throttling is used.

The caller owns one `CrossDevice.create(app, copyBytes)` lifecycle, writes its private source
with `makeSparseFile`, rescans, then calls `startFileMove` toward `device.volume`. Use one worker
and at most 4 GiB. Keep `await device.close()` in `finally`; its existing fail-closed shutdown/
detach and `beforeCleanup` hook remain authoritative. Do not mount a second fixture to queue
another task. Tools retains its own visible progress/speed/ETA, UI cancellation, source integrity,
successful copy and SIGKILL recovery assertions; this helper is not a substitute for those checks.
This extraction is static preparation; runtime proof and landing are still required before reuse
as acceptance evidence.

## Adding coverage

`MovePage.previewAfter(action)` observes the triggered plan and its scoped source/destination
folder and first-page item responses before the action runs. It requires successful completed
bodies and that plan's rendered pane/overlay state before another mode or pattern edit.
`tools/move-preview-boundary.spec.ts` gates real backend responses to protect this ordering.

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
   credentials, `.env` files, original fixture mutations and real services/media are out of scope.
