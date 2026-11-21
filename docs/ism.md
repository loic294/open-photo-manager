# Immich Sync Manager (ISM)

**Settings** > **Sync Manager** verifies an OPM client token, stores it encrypted, lists the roots the
token can see and their write status, and binds volumes to roots. The encryption key remains in
OPM's data directory alongside the database, so protect both.

In **Volume bindings**, choose the ISM root for a volume. The root must have an
OPM path (`opm_prefix`) and the volume must sit inside it; one volume per root. A binding change is
staged and applied the next time OPM starts, before any file operation can run, so a running task
never sees a volume change hands. A bind waits while the volume has unfinished local file
operations; the binding screen shows the reason. Unbinding also applies at the next start. A connection cannot be
removed or switched to another ISM instance while a volume is bound or a bind is staged.

By default, OPM never mutates files on a bound volume itself:

- Move, folder-format and rename plans send their moves to ISM as one operation per batch of up to
  500 files. OPM first asks ISM for a preview, then submits with an idempotency key it stores
  before sending; only a lost answer or a server error is retried, with the same key and body. When
  ISM advertises the `partial_submit` capability, OPM skips the preview and submits with
  `allow_partial: true`, so ISM fails blocked items one by one instead of refusing the whole
  batch. If the outcome of a submit stays unknown, OPM looks the operation up by its key
(`GET /api/v1/instances/{id}/operations?idempotency_key=`) only when ISM advertises
`find_operation_by_key`, and follows it; when nothing is found
  (404, including servers without the lookup) OPM sends a cancel-by-key only when ISM advertises
  `cancel_operation_by_key`, which reserves the key so a late submission cannot run without OPM
  following it. If neither recovery capability is advertised, OPM reports the submit as unreachable
  instead of guessing. ISM
  moves the sidecars with the asset and reports each file; OPM updates its index from ISM's
  streamed events (`progress`, `item`, `finished`, reconnecting with `Last-Event-ID` and reading the
  operation again on `resync`; servers advertising `instance_scoped_operations` are read through
  the instance-scoped status route), and the task shows each item as ISM settles it.
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

The top-bar **Local only changes** toggle is a per-browser, persisted override for file
operations. While it is on, plans, deletes, restores and empty-trash tasks still change the local
files and OPM index, but do not submit an operation to ISM. The mode is saved with each task so it
also survives a restart and recovery. Sync-manager trash restore and permanent deletion remain
remote-only actions and are refused until the toggle is turned off.

OPM logs every ISM operation it sends (key, `operation_id`, state changes, item failure codes);
see [Diagnostics](troubleshooting.md#diagnostics) for how to read and raise those logs.

The `.ism-root`, `.ism-staging` and `.ism-trash` paths of every ISM root are excluded from indexing
and protected from folder cleanup; OPM refuses to trash a folder that contains them. Unbound volumes have no Immich integration.


## File rules and Immich badges

In **Settings** > **General**, **Files and sidecars** stays editable when no compatible Sync Manager is
connected. A connection that supports shared file settings makes these fields read-only; the current
values are shown with the Sync Manager and, if designated, its primary instance. Edit them in Sync
Manager. OPM retains its last settings if that server is temporarily unreachable and restores its
previous local values when disconnected. Re-index affected folders after changing sidecar or junk
rules, just as after a local edit.

For volumes bound to roots linked to Immich, **In Immich** identifies live assets reported by Sync
Manager. A scan or operation updates the badge; OPM keeps the last snapshot during brief outages.

## Upgrading from a direct Immich connection

Before upgrading, finish any pending direct Immich work in the old OPM version. The upgrade removes
its direct connection, mappings, asset index and queued Immich updates; these are not imported into
Sync Manager. Remove obsolete direct Immich environment variables and secret mounts, then connect to
Sync Manager and stage the appropriate volume bindings. Keep the existing app data directory and its
key file when upgrading an existing Sync Manager connection.
