# Connect and mirror changes to Immich

For volumes bound to an Immich Sync Manager, see
[Immich Sync Manager (ISM)](#immich-sync-manager-ism).

For an owner who wants to "login into an Immich instance using a username and password",
"map our docker volumes to Immich external libraries" and reflect moves/deletes in Immich.
The login identifier is the account email. This guide describes the landed repository code;
the published `edge` may lag behind.

Warning: Immich discourages direct database modification and does not officially support these writes.
Take a current backup before tracked moves. PostgreSQL transactions do not serialize with Immich's
scanner: pause watching and scheduled/manual scans and drain queued jobs. An active scan can race a move,
import a duplicate or remove an asset. The filesystem and PostgreSQL cannot share one atomic transaction.

## Direct path updates for tracked moves

1. A file marked **In Immich** moved within its mapped library stays the same asset: OPM updates
   `originalPath` in PostgreSQL for Immich v3.3.3 only. A path-derived `sha1-path` checksum changes
   in the same transaction; content-derived checksums do not. Existing API deletion safeguards are
   unchanged.

   You should see: the existing asset and its associations at the new path.

2. Configure `OPM_IMMICH_DATABASE_URL_FILE` as a read-only secret file, not a Settings field.
   It requires `verify-full` TLS. Follow the [production database reference](../docker/README.md#immich-direct-database-access)
   for the Compose mount, private networking, grants, exclusions and scanner precautions; the
   [isolated schema gate](../docker/immich-postgres-test.md) is a developer test only.
   If the database is missing, unreachable or fails the v3.3.3 schema check, the tracked move is
   refused before moving files and the warning starts "No file was moved." Untracked files in a
   mixed move may still proceed; fix the cause, refresh status and rebuild/retry.

   You should see: a clear preflight refusal, or a retryable outbox warning if PostgreSQL fails
   after a move passed preflight. Filesystem and PostgreSQL cannot share one transaction.

Warning: Immich discourages direct database edits. Take a current backup and pause Immich scans and
watching while OPM moves files; its scanner can race the update. This is neither officially supported
by Immich nor atomic across the filesystem and database.

## Prerequisites and shared paths

1. Use an Immich server reachable from the OPM container, not a `localhost` URL pointing back at OPM.
   The client targets Immich major version 2 or later and compares the reported version with its
   API specification; newer versions can show a compatibility warning, not a compatibility guarantee.
   An administrator account is recommended: regular accounts operate in limited mode without
   library listing, library updates or scan triggering.

   You should see: the server version and account mode after connecting.

2. Mount the same physical share in both containers and configure an Immich external library.
   For example, `/volume1/photo` can be `/volumes/photo` in OPM and `/mnt/photos` in Immich.
   The file `2024/trip/a.jpg` then maps to `/mnt/photos/2024/trip/a.jpg`; the OPM volume is `photo`,
   and its Immich path prefix is `/mnt/photos`, not `/volume1/photo` or `/volumes/photo`.
   Read-only mounting in Immich lets it import the share without granting filesystem writes;
   OPM needs write permission for moves/trash. OPM sync cannot create the shared mount for you.

   You should see: the same test file in OPM and Immich's scanned external library.
   Direct Immich uploads are not external-library files and are outside this integration.

3. Configure Immich external-library exclusions `**/@eaDir/**` and `**/.opm-trash/**`.
   The OPM button described below adds only the trash exclusion, not both patterns.
   Do not rely solely on hidden-directory scan defaults: Immich's watcher may import trash.

   You should see: neither Synology thumbnails nor OPM trash imported as new photos.

## Connect

1. Open **Settings** > **Immich**. Enter **Server URL**, **Email** and **Password**, then **Connect**.
   Use a trusted HTTPS server where possible. The optional **Allow a self-signed certificate**
   setting relaxes TLS verification; use it only for a trusted local server.
   No password is kept: OPM logs in, creates a scoped API key, verifies it, encrypts it in `/data`,
   then ends that temporary login session. Connection is a bounded request, not a progress task.

   You should see: **Connection**, account identity, server version and full or limited mode.
   Never put your password or the created key in Compose, logs or an agent conversation.

2. The key is named `Open Photo Manager`. Its requested permission set is:

   <!-- immich-permissions:start -->
   `asset.read`, `asset.update`, `asset.delete`, `library.read`, `library.update`, `album.read`,
   `albumAsset.create`, `tag.read`, `tag.asset`, `server.about`, `user.read`, `apiKey.delete`
   <!-- immich-permissions:end -->

   These are the client permissions, not the entire Immich permission registry.
   OPM never requests upload or on-disk file-deletion permission. Protect the database and its
   encryption key together; [backup guidance](synology.md#backups-updates-and-removal) explains why.

   You should see: the named key in your own Immich account's API-key list.

3. Use **Check connection** to make an explicit reachability/key check.
   Status otherwise refreshes at startup, on event-stream reconnect and after relevant tasks finish;
   the UI does not periodically poll Immich. **Connect again** replaces the stored key.
   Same-server/same-account reconnect keeps cached assets and queued work; changing either clears them.

   You should see: connection errors explained without blocking browsing.

4. **Disconnect** attempts to revoke the key, forgets the connection, cached sync data and pending
   Immich changes, but keeps volume mappings. It does not delete photos or Immich assets.
   If revocation fails, OPM still disconnects and tells you to remove the named key manually in Immich.
   Changes made while disconnected are not queued for later replay.

   You should see: confirmation explaining how many unmirrored changes will be forgotten.

## Map volumes and sync

1. In **Library mapping**, choose the Immich library and enter the prefix *as Immich sees it*.
   In limited mode enter the prefix manually; no library choice is available.
   **Auto-detect** compares sampled indexed files with paths Immich already knows.
   It needs a scanned library and indexed sample files; **Use** accepts a proposed prefix.
   Clear both library and prefix to unmap in full mode. Save with **Save**.

   You should see: **Mappings saved. Syncing with Immich…** and a background sync task.
   If no asset matches, check the exact prefix and whether Immich scanned the same share.

2. In full mode click **Keep the trash out of Immich** after saving mappings.
   It adds `**/.opm-trash/**` to mapped libraries without replacing existing exclusions.
   Inspect per-library failures as well as successful updates; limited-mode users need an admin
   to configure this exclusion in Immich.

   You should see: the pattern and each library's updated, already-excluded or failed result.

3. **Sync now** reads incremental changes; **Full resync** rereads everything.
   Sync is read-only towards Immich and the filesystem. Its scheduler checks every 30 seconds,
   with incremental passes after 15 minutes and full passes after 24 hours; new/changed mappings
   are due immediately. Mapping save also requests a sync. Work is coalesced on the Immich task lane.
   Matching uses mapped paths, not content hashes. An external asset's stored checksum algorithm
   can be path-derived or content-derived; sync does not treat either as a substitute for mapping.

   You should see: **Files matched**, total/live counts and **Library sync details**.
   The **In Immich** badge applies to live, mapped assets, not offline or trashed entries.
   There is currently no separate Immich membership filter in the browse API.

## Deletions and other queued changes

The API asset-deletion safeguards are unchanged. Moves outside mapped libraries retain the
offline-confirmed removal path; tracked in-library moves use the direct update described above.

1. OPM records mapped photo/video moves, copies, trash and restores in a transactional outbox.
   Tracked in-library moves first require the database preflight; queued mirroring then runs in
   background tasks. The scheduler waits for quiet file operations; see
   `OPM_IMMICH_QUIET_SECS` in the [configuration reference](../docker/README.md#configuration).
   This delay is not a promise that Immich finishes importing within that window.
   Changes before the scan is requested can merge; later moves create separate rows.

   You should see: **Changes waiting for Immich** and background update tasks.

2. Trash and moves outside mapped libraries remove the old record only after Immich reports it
   offline and the old file is absent. Copies/restores or entry into a mapped library can ask for
   import through Immich's scan. Library scan triggers require an administrator account;
   limited-mode accounts wait for Immich's own schedule.

   You should see: queued changes and scans where applicable, not delete/recreate for a direct move.

3. Safety rules: only outbox-named assets can be removed; a fresh asset read must confirm offline;
   its path must match the mapped old path; that old file must be absent on a readable volume.
   A failed carry-over blocks deletion and requires attention. The hook records changes in the
   file-operation transaction, and remote mirroring calls run in cancellable background tasks.
   A cached offline flag alone never authorises deletion: force-deleting an online Immich asset
   could delete an original, so OPM refuses that operation.

   You should see: unreadable volumes and online old assets wait instead of being forcibly deleted.

4. Inspect attention rows and their last errors. After fixing the cause, use **Retry selected**
   or **Retry all**. **Dismiss selected** forgets queued work without touching Immich; it is not
   an undo, and the offline asset remains for Immich's own cleanup.
   Scan waits can need attention after two hours for admin accounts or three days for limited accounts;
   transient outages retry with backoff rather than blocking other tools.

   You should see: an attention indication in **Settings** and an explicit decision for affected rows.

## Carry-over limits

1. Direct moves preserve the existing asset rather than carrying metadata to a new one.
   This is not a metadata backup or a guarantee against concurrent scanner changes.
   Where a legacy API copy/reimport flow carries metadata between assets, people, stacks and
   shared links do not transfer. Trash processed in Immich can lose album membership even if restored.
   Whole-directory trash/restore is not mirrored file by file. Locked assets are invisible to API keys.
   Google Takeout supplemental JSON filenames do not link as supported sidecars.

   You should see: supported external-library photo/video changes, not uploaded or locked assets.

2. For legacy API carry-over only: once the old asset is observed offline, a replacement file that has not
   imported after ten minutes can cause the old record to be removed without carry-over.
   Repeated moves after scanning begins also depend on the intermediate asset being online when
   the next snapshot is taken. Keep an independent metadata/album backup and verify results.

   You should see: no promise of album recovery after trash/reimport. These carry-over limits do not
   describe the new tracked in-library direct-move mechanism.

## Connection errors

The table quotes OPM's UI messages; upstream validation can also add field-specific details.

| Code | Message in OPM | Fix |
|---|---|---|
| `immich_not_connected` | Connect to Immich first. | Connect in the Immich settings tab. |
| `immich_needs_reconnect` | Immich no longer accepts the stored key. Connect again. | Reconnect; check the app-state key file after restoration. |
| `immich_auth_failed` | Immich refused the email or password. Check both and try again. | Check your Immich account credentials yourself. |
| `immich_invalid_url` | Enter a valid HTTP or HTTPS address for Immich. | Use a server base URL reachable from OPM, without embedded credentials. |
| `immich_admin_required` | This action needs an Immich administrator account. | Use an admin for library operations or accept limited mode. |
| `immich_unreachable` | The Immich address could not be reached. Check the URL and that Immich is running. | Check routing, server health and TLS trust. |
| `immich_unauthorized` | Immich refused the stored key. Connect again. | Reconnect to replace the key. |
| `immich_forbidden` | This account does not have permission for this action in Immich. | Check account permissions; do not repeatedly retry an admin-only action. |
| `immich_not_mapped` | Map a volume to Immich before syncing. | Save a valid library/prefix mapping. |
| `immich_error` | Immich refused the request. Check the connection and try again. | Check connection and upstream compatibility; inspect the last error. |

For outbox error causes and diagnostics, see [troubleshooting](troubleshooting.md#immich).

## Immich Sync Manager (ISM)

**Settings > Sync Manager** verifies an OPM client token, stores it encrypted, lists the roots the
token can see and their write status, and binds volumes to roots. The encryption key remains in
OPM's data directory alongside the database, so protect both.

**Bind a volume.** In **Volume bindings**, choose the ISM root for a volume. The root must have an
OPM path (`opm_prefix`) and the volume must sit inside it; one volume per root. A binding change is
staged and applied the next time OPM starts, before any file operation can run, so a running task
never sees a volume change hands. A bind waits (the reason is shown) while the volume still has OPM
Immich outbox records, direct-move records or pending file-operation journal entries; settle that
work first. Completed direct-move proof is retained while its journal exists and must not be
discarded to bypass this check. If such work appears for an already bound volume, OPM refuses to
start rather than replaying it. Unbinding also applies at the next start. A connection cannot be
removed or switched to another ISM instance while a volume is bound or a bind is staged.

**What a binding changes.** OPM never mutates files on a bound volume itself:

- Move, folder-format and rename plans send their moves to ISM as one operation per batch of up to
  500 files. OPM first asks ISM for a preview, then submits with an idempotency key it stores
  before sending; only a lost answer or a server error is retried, with the same key and body. When
  ISM advertises the `partial_submit` capability, OPM skips the preview and submits with
  `allow_partial: true`, so ISM fails blocked items one by one instead of refusing the whole
  batch. If the outcome of a submit stays unknown, OPM looks the operation up by its key
  (`GET /api/v1/instances/{id}/operations?idempotency_key=`) and follows it; when nothing is found
  (404, including servers without the lookup) OPM sends a cancel-by-key, which reserves the key so
  a late submission cannot run without OPM following it. ISM
  moves the sidecars with the asset and reports each file; OPM updates its index from ISM's
  streamed events (`progress`, `item`, `finished`, reconnecting with `Last-Event-ID` and reading the
  operation again on `resync`), and the task shows each item as ISM settles it.
- Every move carries the file's revision (`<size>-<mtime in ns>`) read from the index-checked file.
  If the revision cannot be computed exactly the item fails (`revision_unavailable`) instead of
  being sent without a guard; ISM answers `revision_changed` when the file changed meanwhile.
- Trash (including empty-folder cleanup, which trashes folders holding only junk) goes to the ISM
  trash of the root (`.ism-trash`), not OPM's trash. A trashed file carries its revision; a folder
  removed by cleanup lists the junk files OPM checked, with their revisions
  (`expected_entries`), so ISM refuses when anything else appeared in it.
- **Trash** shows a **Sync manager trash** section per bound volume, read from ISM. Restore sends
  `restore` actions; **Delete for good…** sends `purge` actions and appears only when the token has
  the `purge` grant; it is always confirmed. Both run as tasks through the same operations API.
- Folder moves and renames (`move_folder`) and folder creation (`create_folder`) go to ISM; OPM
  updates the folder's files and subfolders in its index from the result.
- Cancelling the task cancels the ISM operation; items ISM had not started end as cancelled.
- ISM refusals are per-item failures with ISM's code. `direct_writes_unavailable` (OPM checks the
  root's `direct_writes` state before sending anything: changes run while it is `enabled` or
  `not_linked`, and are refused while it is `blocked`, with ISM's reason shown in settings and in
  the failure), `maintenance_blocked`, `root_unavailable` and `forbidden` are final: OPM
  does not retry them and never falls back to changing the files locally.
- Refused on bound volumes: copies, moves between a bound and an unbound volume
  (`ism_cross_boundary`), the **Replace** conflict policy (`ism_replace_unsupported`; use rename or
  skip), and restoring or purging OPM trash items there (items OPM trashed before the binding).

OPM logs every ISM operation it sends (key, `operation_id`, state changes, item failure codes);
see [Diagnostics](troubleshooting.md#diagnostics) for how to read and raise those logs.

The `.ism-root`, `.ism-staging` and `.ism-trash` paths of every ISM root are excluded from indexing
and protected from folder cleanup; OPM refuses to trash a folder that contains them. Volumes that
are not bound keep OPM's existing Immich integration unchanged.
