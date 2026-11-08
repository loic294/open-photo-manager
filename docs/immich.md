# Connect and mirror changes to Immich

For an owner who wants to "login into an Immich instance using a username and password",
"map our docker volumes to Immich external libraries" and reflect moves/deletes in Immich.
The login identifier is the account email. This guide describes the landed repository code;
the isolated mock-backed screen walkthrough is still pending, and the published `edge` may lag behind.

Warning: move integration is being redesigned to update paths directly in Immich's database.
The existing outbox/carry-over description below is a reference to the currently landed implementation,
not the final design or an accepted move walkthrough. Database setup, supported versions and move
instructions are paused until the replacement contract is supplied and verified. No direct database
access or credentials are configured by this guide.

## Confirmed move requirements awaiting implementation verification

The direct-database move target is exactly Immich v3.3.3. These are confirmed requirements,
not a claim that the feature has landed; database configuration, schema checks, exact error messages
and retry controls must be reconciled with the implementation owner's feature-specific documentation.

- An affected Immich-tracked move must be refused if the database is missing, unavailable or fails
  the v3.3.3 schema check. The failure must clearly state that the affected file was not moved and
  explain how to retry after restoring database access or resolving the schema mismatch.
- Untracked files in a mixed move may proceed. A failed tracked item therefore does not imply that
  every other file stayed in place; inspect the per-item outcome before retrying.
- A database failure after successful preflight uses durable outbox retry and a warning. Filesystem
  and database changes are not a cross-system atomic transaction: a file may already have moved
  while its Immich update is pending.
- Immich discourages direct database writes. They can race its scanner and require a current backup.
  This integration must not be described as officially supported or atomic.

## Prerequisites and shared paths

1. Use an Immich server reachable from the APM container, not a `localhost` URL pointing back at APM.
   The client targets Immich major version 2 or later and compares the reported version with its
   API specification; newer versions can show a compatibility warning, not a compatibility guarantee.
   An administrator account is recommended: regular accounts operate in limited mode without
   library listing, library updates or scan triggering.

   You should see: the server version and account mode after connecting.

2. Mount the same physical share in both containers and configure an Immich external library.
   For example, `/volume1/photo` can be `/volumes/photo` in APM and `/mnt/photos` in Immich.
   The file `2024/trip/a.jpg` then maps to `/mnt/photos/2024/trip/a.jpg`; the APM volume is `photo`,
   and its Immich path prefix is `/mnt/photos`, not `/volume1/photo` or `/volumes/photo`.
   Read-only mounting in Immich lets it import the share without granting filesystem writes;
   APM needs write permission for moves/trash. APM sync cannot create the shared mount for you.

   You should see: the same test file in APM and Immich's scanned external library.
   Direct Immich uploads are not external-library files and are outside this integration.

3. Configure Immich external-library exclusions `**/@eaDir/**` and `**/.apm-trash/**`.
   The APM button described below adds only the trash exclusion, not both patterns.
   Do not rely solely on hidden-directory scan defaults: Immich's watcher may import trash.

   You should see: neither Synology thumbnails nor APM trash imported as new photos.

## Connect

1. Open **Settings** > **Immich**. Enter **Server URL**, **Email** and **Password**, then **Connect**.
   Use a trusted HTTPS server where possible. The optional **Allow a self-signed certificate**
   setting relaxes TLS verification; use it only for a trusted local server.
   No password is kept: APM logs in, creates a scoped API key, verifies it, encrypts it in `/data`,
   then ends that temporary login session. Connection is a bounded request, not a progress task.

   You should see: **Connection**, account identity, server version and full or limited mode.
   Never put your password or the created key in Compose, logs or an agent conversation.

2. The key is named `Advanced Photo Manager`. Its requested permission set is:

   <!-- immich-permissions:start -->
   `asset.read`, `asset.update`, `asset.delete`, `library.read`, `library.update`, `album.read`,
   `albumAsset.create`, `tag.read`, `tag.asset`, `server.about`, `user.read`, `apiKey.delete`
   <!-- immich-permissions:end -->

   These are the client permissions, not the entire Immich permission registry.
   APM never requests upload or on-disk file-deletion permission. Protect the database and its
   encryption key together; [backup guidance](synology.md#backups-updates-and-removal) explains why.

   You should see: the named key in your own Immich account's API-key list.

3. Use **Check connection** to make an explicit reachability/key check.
   Status otherwise refreshes at startup, on event-stream reconnect and after relevant tasks finish;
   the UI does not periodically poll Immich. **Connect again** replaces the stored key.
   Same-server/same-account reconnect keeps cached assets and queued work; changing either clears them.

   You should see: connection errors explained without blocking browsing.

4. **Disconnect** attempts to revoke the key, forgets the connection, cached sync data and pending
   Immich changes, but keeps volume mappings. It does not delete photos or Immich assets.
   If revocation fails, APM still disconnects and tells you to remove the named key manually in Immich.
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
   It adds `**/.apm-trash/**` to mapped libraries without replacing existing exclusions.
   Inspect per-library failures as well as successful updates; limited-mode users need an admin
   to configure this exclusion in Immich.

   You should see: the pattern and each library's updated, already-excluded or failed result.

3. **Sync now** reads incremental changes; **Full resync** rereads everything.
   Sync is read-only towards Immich and the filesystem. Its scheduler checks every 30 seconds,
   with incremental passes after 15 minutes and full passes after 24 hours; new/changed mappings
   are due immediately. Mapping save also requests a sync. Work is coalesced on the Immich task lane.
   Matching uses mapped paths, not content hashes: external-library checksums are path-based.

   You should see: **Files matched**, total/live counts and **Library sync details**.
   The **In Immich** badge applies to live, mapped assets, not offline or trashed entries.
   There is currently no separate Immich membership filter in the browse API.

## Existing outbox reference

This section records the pre-redesign implementation only. Do not treat its move/carry-over mechanism
as the final design; the replacement direct-database move contract and acceptance are pending.

1. APM records mapped photo/video moves, copies, trash and restores in a transactional outbox.
   Filesystem work does not wait for Immich. The scheduler waits for quiet file operations; see
   `APM_IMMICH_QUIET_SECS` in the [configuration reference](../docker/README.md#configuration).
   This delay is not a promise that Immich finishes importing within that window.
   Changes before the scan is requested can merge; later moves create separate rows.

   You should see: **Changes waiting for Immich** and background update tasks.

2. The task refreshes affected libraries, snapshots the old asset's albums and metadata, requests
   one scan per affected library when permitted, then waits for the old asset to become offline
   and the replacement to be imported. It carries albums, favorite, rating, archive state,
   description and tags to a replacement without overwriting fields already customised there.
   Moving outside all mapped libraries removes the old offline record without a replacement;
   moving into a mapped library asks for import.

   You should see: the new path appear in Immich after its scan, rather than an in-place path edit.
   Limited-mode accounts wait for Immich's own scan schedule.

3. Safety rules: only outbox-named assets can be removed; a fresh asset read must confirm offline;
   its path must match the mapped old path; that old file must be absent on a readable volume.
   A failed carry-over blocks deletion and requires attention. The hook records changes in the
   file-operation transaction, and remote mirroring calls run in cancellable background tasks.
   A cached offline flag alone never authorises deletion: force-deleting an online Immich asset
   could delete an original, so APM refuses that operation.

   You should see: unreadable volumes and online old assets wait instead of being forcibly deleted.

4. Inspect attention rows and their last errors. After fixing the cause, use **Retry selected**
   or **Retry all**. **Dismiss selected** forgets queued work without touching Immich; it is not
   an undo, and the offline asset remains for Immich's own cleanup.
   Scan waits can need attention after two hours for admin accounts or three days for limited accounts;
   transient outages retry with backoff rather than blocking other tools.

   You should see: an attention indication in **Settings** and an explicit decision for affected rows.

## Carry-over limits

1. Treat carry-over as best effort, not a metadata backup. People, stacks and shared links do not
   transfer. Trash processed in Immich can lose album membership even if the file is restored.
   Whole-directory trash/restore is not mirrored file by file. Locked assets are invisible to API keys.
   Google Takeout supplemental JSON filenames do not link as supported sidecars.

   You should see: supported external-library photo/video changes, not uploaded or locked assets.

2. Important: once the old asset is observed offline, a replacement file that Immich has still not
   imported after ten minutes can cause the old record to be removed without carry-over.
   Repeated moves after scanning begins also depend on the intermediate asset being online when
   the next snapshot is taken. Keep an independent metadata/album backup and verify results.

   You should see: no guarantee that album membership survives every delayed import or move chain.

## Connection errors

The table quotes APM's UI messages; upstream validation can also add field-specific details.

| Code | Message in APM | Fix |
|---|---|---|
| `immich_not_connected` | Connect to Immich first. | Connect in the Immich settings tab. |
| `immich_needs_reconnect` | Immich no longer accepts the stored key. Connect again. | Reconnect; check the app-state key file after restoration. |
| `immich_auth_failed` | Immich refused the email or password. Check both and try again. | Check your Immich account credentials yourself. |
| `immich_invalid_url` | Enter a valid HTTP or HTTPS address for Immich. | Use a server base URL reachable from APM, without embedded credentials. |
| `immich_admin_required` | This action needs an Immich administrator account. | Use an admin for library operations or accept limited mode. |
| `immich_unreachable` | The Immich address could not be reached. Check the URL and that Immich is running. | Check routing, server health and TLS trust. |
| `immich_unauthorized` | Immich refused the stored key. Connect again. | Reconnect to replace the key. |
| `immich_forbidden` | This account does not have permission for this action in Immich. | Check account permissions; do not repeatedly retry an admin-only action. |
| `immich_not_mapped` | Map a volume to Immich before syncing. | Save a valid library/prefix mapping. |
| `immich_error` | Immich refused the request. Check the connection and try again. | Check connection and upstream compatibility; inspect the last error. |

For outbox error causes and diagnostics, see [troubleshooting](troubleshooting.md#immich).
