# Browse, settings and recovery

For someone checking a newly indexed library before making changes.
This is an intentionally partial guide: the four tool walkthroughs, pattern-token table and complete
screen tour will be added after their implementation contracts land and are verified.
No planned tool labels or actions are described here as shipped.

## Check the library

1. Open **Browse**, choose a mounted volume and navigate its folders. Folder counts and items come
   from the index. Each pane navigates independently; the virtual top level represents the mounts.
   Use filters to narrow camera, rating, flags and tags. A missing folder is not silently treated
   as an empty library: the client walks back to an existing ancestor.

   You should see: indexed folders above the item grid, and counts reflecting your filters.

2. Open a photo or video when selection is inactive. Still-image formats the browser cannot
   display directly use a JPEG rendition where supported by the server; video codecs are not
   transcoded. See [media troubleshooting](troubleshooting.md#browsing-and-media).

   You should see: the viewer, or an explanation/download path for unsupported media.

## Settings and indexing

1. Open **Settings** > **General**. **Files and sidecars** controls **Sidecar extensions**,
   **Ignored names** and **Ignored prefixes**. Defaults link full-filename or same-stem companions
   with extensions `xmp`, `json`, `aae`, `dop`, `pp3`, `xml`. Metadata extraction reads XMP;
   recognising other companion extensions does not imply parsing their metadata.
   XMP fields override embedded metadata field by field.

   You should see: edits stay local until **Save**; **Discard** restores saved values.
   Re-index after changing sidecar/junk rules to apply them to existing assets.

2. **Moving and deleting** includes **Longest generated name**, conflict policy and
   **Keep the original in the trash after moving a file to another volume**.
   Cross-volume moves normally remove the source only after a size/hash-verified copy; enabling
   that setting keeps the original in trash and uses additional space.
   Do not confuse a move with an undoable trash deletion.

   You should see: a choice of **Smart**, **Skip**, **Rename** and **Replace**.
   Replacement sends the existing destination to trash rather than overwriting it in place.
   Detailed tool previews remain deferred until the tools are integrated.

3. **Indexing** controls scans, hashing, thumbnails, watches and periodic repair.
   The top-right task indicator lets background work proceed without locking the rest of the app.
   Failed and interrupted tasks remain visible until dismissed; dismissing history is not a retry.
   See [watcher fallback](synology.md#watcher-and-rescan-fallback) and
   [indexing troubleshooting](troubleshooting.md#indexing).

   You should see: progress as indexing fills the library, with held-removal warnings if a mount
   unexpectedly loses files.

4. **Re-index current folders** forces metadata, checksums and thumbnails to be reread for the
   folders shown by both panes, including subfolders; identical pane locations are sent once.
   With both panes at the virtual volumes root, open a real folder first.
   **Re-index all assets** asks for confirmation, then forces rereading across every volume.
   Ordinary periodic rescans are incremental, not a reason to repeatedly force the entire library.

   You should see: a background task while the app stays usable. Use current folders after local
   metadata/sidecar changes; use all assets for library-wide rule changes or repair.

## Trash and recovery

1. File-deletion actions send content to `<volume>/.apm-trash/<batch>/<original-relative-path>`,
   not to Synology's recycle bin. Linked sidecars follow owner-aware rules: a shared sidecar is
   trashed only when all its owners are trashed. Moves are not automatically reversible;
   cross-volume source removal is separate from trash, as explained above.
   Back up photos and [app state](synology.md#backups-updates-and-removal).

   You should see: deleted content in **Trash**, retaining its original path and deletion batch.

2. Use **Restore selected** for selected items, **Restore** for one item or **Restore all** for a batch.
   Restore runs as background file work and resolves destination conflicts; it cannot recover
   content already permanently purged. A read-only or offline share cannot accept restored files.

   You should see: restored files at their original location, subject to the chosen conflict policy.

3. **Empty trash…** opens a confirmation and permanently removes the trash covered by the displayed
   snapshot. Items newly trashed after that snapshot are not swept into the same manual empty task.
   This is not undoable in APM.

   You should see: a warning before permanent deletion, then a background trash-empty task.

4. **Delete trashed files automatically** is off by default. Enabling it with **Delete after**
   permanently purges items older than that many days, without a separate manual empty action.
   Shortening retention can take effect as soon as you save. Leave it off if you want manual control.

   You should see: automatic deletion remains disabled unless you explicitly enable and save it.
