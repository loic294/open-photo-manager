# Diagnose a deployment

For an owner checking a failed start, missing files or delayed Immich changes.
These tables describe source-verified behavior; DSM procedures and the full local walkthrough are
not verified on a DSM device.

1. Find the matching symptom below before changing permissions, data or mappings.

   You should see: a specific cause to check, not a reason to run the container as root.

2. Collect the [diagnostics](#diagnostics) for unresolved failures.

   You should see: health, index status and bounded log output; redact private data before sharing.

## Startup and access

| Symptom | Cause | Fix |
|---|---|---|
| Container exits; data-directory writability error | Selected UID/GID cannot write `/data`, or it is mounted `:ro` | Check numeric ownership and ACLs on app storage; see [permissions](synology.md#prepare-the-nas). Named volumes also need correct ownership. Do not run as root to mask it. |
| Startup refuses with `does not safely support SQLite WAL` or `SQLite WAL could not be verified` | `/data` is on a known WAL-unsafe filesystem, or the live WAL probe failed | Keep `/data` on a local Docker named volume. Back up existing app data before migrating it; there is no rollback-journal fallback. See the [Windows host guide](windows.md#keep-app-data-on-a-local-docker-volume). |
| `No space left on device` | Storage used for data, scratch or Docker image layers is full | Check that specific filesystem/engine's free space. Host free space does not prove a Docker Desktop VM has room; DSM storage is a separate environment. Do not prune unrelated containers/volumes. |
| SQLite refuses app data | `/data` is on NFS/SMB, corrupted, or contains newer migrations | Use local storage. An old binary cannot open a newer schema; restore a matching backup for downgrade, never delete the database casually. |
| `host_not_allowed` (403) | Browser hostname is not accepted | Set `OPM_ALLOWED_HOSTS` and recreate the container; behind a proxy forward the original `Host`. |
| `csrf_rejected` (403) | Request header missing, cross-site request, or proxy changes `Host` | The web client sends `x-opm-request: 1`; check same-origin routing. API scripts must send that header for non-GET requests. Do not weaken the guard. |
| Image pull denied or not found | The image name or tag is wrong, or the package was made private | Use `ghcr.io/loic294/open-photo-manager` with an existing tag such as `edge`. If both are right, the owner checks the package visibility in its GitHub package settings; see [GHCR publishing](../docker/README.md#public-ghcr-publishing). |
| Wrong architecture / executable will not start | Unsupported 32-bit NAS or offline image for another platform | Compare `uname -m` with [image architectures](../docker/README.md#build). |
| Running but `unhealthy` | Server is not answering the configured health-check port | Read logs and verify `OPM_LISTEN` and published port; see [health checks](../docker/README.md#health-check). |

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

## Tools

| Symptom | Cause | Fix |
|---|---|---|
| Duplicate finder shows no matches | The selected match mode, filters or pane bases exclude the files; hash mode also waits for indexing | Check both pane bases and filters, choose a suitable **Match by** mode, and let hashing finish. |
| Move or formatting left empty folders | Empty source directories are not automatically removed | Review **Empty folder cleanup** at the source folder; it removes qualifying child folders after confirmation. |
| Cleanup leaves a folder behind | It contains a file, protected/unrecognized content, or no longer qualifies at execution time | Inspect its contents and index state; cleanup keeps the base folder and anything that is not safely empty. |
| Move result reports `shared_sidecar_retained` | A matching sidecar was already at that destination | The asset moved; the result names the original sidecar retained at the source. It is not a skipped photo. |
| Folder formatting pattern is invalid or has no preview | No source folder is selected, or the pattern contains an invalid token/path | Select a source and fix the pattern shown by **Folder pattern**; use **Variables and examples** and its live preview. |

## Browsing and media

| Symptom | Cause | Fix |
|---|---|---|
| `folder_not_found` (404) | Folder no longer exists in the index | Navigate to an existing ancestor and refresh after indexing. |
| HEIC/TIFF/RAW has no viewer image | JPEG rendering failed or media tooling lacks support for that file | Check logs and [Docker media-tool guidance](../docker/README.md#troubleshooting). Originals remain unchanged. |
| Video thumbnail works but playback fails | Browser does not support the original container/codec | Download/open the original in a compatible player. OPM does not transcode video for playback. |
| Sidecar/unknown file has no preview | No supported viewer rendition for that kind | Use the original download; thumbnails are not a universal preview guarantee. |

## Sync Manager

Check the connection and the root’s write status in **Settings** > **Sync Manager**.
For binding or badge issues, see [Sync Manager](ism.md).

## Performance

| Symptom | Cause | Fix |
|---|---|---|
| Slow first index / busy disks | Hashing reads file contents; metadata and thumbnail work also consume CPU/I/O | Start with low worker counts in **Indexing**; allow the first pass to finish before comparing later scans. |
| Memory pressure on a small NAS | Too many concurrent metadata/thumbnail jobs or a very large media file | Reduce workers and see [Docker resource settings](../docker/README.md#configuration). Do not confuse image-build memory with runtime indexing. |

## Diagnostics

1. From the Compose project folder inspect status and recent logs:

   ```sh
   docker compose ps
   docker compose logs --tail=200 opm
   curl --fail http://NAS_ADDRESS:PORT/api/health
   curl --fail http://NAS_ADDRESS:PORT/api/index/status
   ```

   Replace `NAS_ADDRESS` and `PORT` with your LAN address and published port. Use the same Docker
   OS user/prefix as installation. GET diagnostics do not require the custom request header.

   You should see: health output plus index tasks, held removals and watcher state.

2. If necessary set `OPM_LOG` to `info,opm_server::indexer=debug` in Compose and recreate the service.
   Revert elevated logging after the investigation. Debug logs can include private paths;
   redact paths, user identities and addresses, and never share passwords, tokens, database files
   or `/data/immich.key`.

   You should see: detailed indexing context without changing file permissions or disabling security.

3. For a volume bound to an Immich Sync Manager, OPM logs each ISM operation it sends under
   `opm_server::ism` (`docker compose logs opm | grep -i ism`): the idempotency key and ISM
   `operation_id` with each state (`submitted`, `finished`, `refused`, `lost`,
   `cancelled_before_submission`), `ISM item failed` with the item index and ISM's error code, and
   `ISM batch failed` for a batch that could not run (connection, `direct_writes_unavailable`,
   `maintenance_blocked`). For per-item messages and progress set `OPM_LOG` to
   `info,opm_server::ism=debug,opm_server::fsops=debug`. Match the `operation_id` against ISM's own
   operation history and logs. The task in OPM's task list keeps each failed item's code and message.

   You should see: one `ISM operation state` line per state change of every operation OPM sent.
