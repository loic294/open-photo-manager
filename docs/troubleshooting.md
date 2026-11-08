# Diagnose a deployment

For an owner checking a failed start, missing files or delayed Immich changes.
These tables describe source-verified behavior; DSM procedures and the full local walkthrough are
not verified on a DSM device. Tool-specific failures will be added after the pending UIs land.

1. Find the matching symptom below before changing permissions, data or mappings.

   You should see: a specific cause to check, not a reason to run the container as root.

2. Collect the [diagnostics](#diagnostics) for unresolved failures.

   You should see: health, index status and bounded log output; redact private data before sharing.

## Startup and access

| Symptom | Cause | Fix |
|---|---|---|
| Container exits; data-directory writability error | Selected UID/GID cannot write `/data`, or it is mounted `:ro` | Check numeric ownership and ACLs on app storage; see [permissions](synology.md#prepare-the-nas). Named volumes also need correct ownership. Do not run as root to mask it. |
| `No space left on device` | Storage used for data, scratch or Docker image layers is full | Check that specific filesystem/engine's free space. Host free space does not prove a Docker Desktop VM has room; DSM storage is a separate environment. Do not prune unrelated containers/volumes. |
| SQLite refuses app data | `/data` is on NFS/SMB, corrupted, or contains newer migrations | Use local storage. An old binary cannot open a newer schema; restore a matching backup for downgrade, never delete the database casually. |
| `host_not_allowed` (403) | Browser hostname is not accepted | Set `APM_ALLOWED_HOSTS` and recreate the container; behind a proxy forward the original `Host`. |
| `csrf_rejected` (403) | Request header missing, cross-site request, or proxy changes `Host` | The web client sends `x-apm-request: 1`; check same-origin routing. API scripts must send that header for non-GET requests. Do not weaken the guard. |
| Private image pull denied | Missing login, package read access or SSO authorisation; login used a different OS user | Follow [GHCR installation](../README.md#install-on-synology), using the same `sudo` prefix for login and Compose. |
| Wrong architecture / executable will not start | Unsupported 32-bit NAS or offline image for another platform | Compare `uname -m` with [image architectures](../docker/README.md#build). |
| Running but `unhealthy` | Server is not answering the configured health-check port | Read logs and verify `APM_LISTEN` and published port; see [health checks](../docker/README.md#health-check). |

## Indexing

| Symptom | Cause | Fix |
|---|---|---|
| No mounted volumes | Share is not an immediate child mount below `/volumes` | Mount each share at `/volumes/NAME`; check permissions as the chosen container user. |
| Expected files absent | Index still running, ignored name/prefix, symlink, offline mount or active filter | Wait for the task; check filters, mount access and [scanner exclusions](synology.md#first-start). Do not change junk rules blindly. |
| Held-removal warning | Many previously indexed files vanished; mount may be disconnected | Restore/check the mount, then **Rescan**. **Remove from the index** confirms index removal only after you verify the mount; it does not delete on-disk files. |
| Watcher `degraded` | Watch budget/native error or incomplete coverage | Inspect `watcher` in `/api/index/status`, keep periodic rescans enabled, fix host limits/mount access; see [watcher fallback](synology.md#watcher-and-rescan-fallback). |
| External edits appear late | Quiet-period batching, missed events or partial watch coverage | Allow the event batch to settle, then rescan; periodic scans repair gaps. |
| Dates wrong by hours/day | Wrong `TZ` for filesystem/UTC-video fallback, or wrong metadata | Correct `TZ`, recreate and re-index. Embedded wall-clock dates are not simply shifted; see [date semantics](synology.md#names-and-dates). |
| Companion exists but metadata is absent | Non-XMP companion, unsupported supplemental JSON name or unreadable/malformed XMP | Verify same-stem/full-filename linking and XMP contents; non-XMP linking does not imply metadata parsing. |
| Task stays failed/interrupted | History deliberately persists | Inspect failure details, correct the cause and explicitly request work again; dismissing history is not a retry. |

## Browsing and media

| Symptom | Cause | Fix |
|---|---|---|
| `folder_not_found` (404) | Folder no longer exists in the index | Navigate to an existing ancestor and refresh after indexing. |
| HEIC/TIFF/RAW has no viewer image | JPEG rendering failed or media tooling lacks support for that file | Check logs and [Docker media-tool guidance](../docker/README.md#troubleshooting). Originals remain unchanged. |
| Video thumbnail works but playback fails | Browser does not support the original container/codec | Download/open the original in a compatible player. APM does not transcode video for playback. |
| Sidecar/unknown file has no preview | No supported viewer rendition for that kind | Use the original download; thumbnails are not a universal preview guarantee. |

## Immich

Connection-code messages and fixes are in the [Immich guide](immich.md#connection-errors).
Move/carry-over rows below refer to the existing implementation, not the pending direct-database
move design. Final move troubleshooting and acceptance are paused.

| Symptom / outbox code | Cause | Fix |
|---|---|---|
| Zero **Files matched** | Wrong mapped prefix, unscanned library or different physical share | Compare a test file's exact path in both containers; **Auto-detect** needs existing indexed/scanned samples. |
| Status seems stale | UI refresh is event-driven, not periodic polling | Use **Check connection** for a live connection probe; **Sync now** refreshes asset data. |
| `immich_unavailable` | Remote server is temporarily unavailable | Restore connectivity; queued updates back off and resume, or use **Retry all** after fixing the cause. |
| `unreadable` | Volume cannot be safely inspected | Fix the offline mount/permissions before retrying; absence has not been proved. |
| `not_offline` | Immich still considers the old asset online | Rescan the external library in Immich and retry; never force-delete an online asset to bypass the safeguard. |
| `scan_timeout` | Immich has not observed/imported a change in time | Check scan completion and library path/exclusions, then **Retry selected**. |
| `carry_failed` | Immich refused album/metadata transfer | Inspect the replacement asset and permissions before retrying. Permanent carry failure blocks deletion. |
| `path_mismatch` | Fresh asset path differs from the mapped old location | Correct mapping; do not bypass the path check. |
| Trash appears as new photos | Immich watcher imported `.apm-trash` | Use **Keep the trash out of Immich** or ask an admin to set exclusions; check [shared paths](immich.md#prerequisites-and-shared-paths). |
| Changes wait in limited mode | Regular account cannot trigger scans | Wait for or request Immich's own library scan through an admin. |
| Albums missing after a move/restore | Best-effort carry-over has explicit limits | Read [carry-over limits](immich.md#carry-over-limits), including delayed imports and whole-folder trash. APM is not an album backup. |

## Performance

| Symptom | Cause | Fix |
|---|---|---|
| Slow first index / busy disks | Hashing reads file contents; metadata and thumbnail work also consume CPU/I/O | Start with low worker counts in **Indexing**; allow the first pass to finish before comparing later scans. |
| Memory pressure on a small NAS | Too many concurrent metadata/thumbnail jobs or a very large media file | Reduce workers and see [Docker resource settings](../docker/README.md#configuration). Do not confuse image-build memory with runtime indexing. |

## Diagnostics

1. From the Compose project folder inspect status and recent logs:

   ```sh
   docker compose ps
   docker compose logs --tail=200 apm
   curl --fail http://NAS_ADDRESS:PORT/api/health
   curl --fail http://NAS_ADDRESS:PORT/api/index/status
   ```

   Replace `NAS_ADDRESS` and `PORT` with your LAN address and published port. Use the same Docker
   OS user/prefix as installation. GET diagnostics do not require the custom request header.

   You should see: health output plus index tasks, held removals and watcher state.

2. If necessary set `APM_LOG` to `info,apm_server::indexer=debug` in Compose and recreate the service.
   Revert elevated logging after the investigation. Debug logs can include private paths;
   redact paths, user identities and addresses, and never share passwords, tokens, database files
   or `/data/immich.key`.

   You should see: detailed indexing context without changing file permissions or disabling security.
