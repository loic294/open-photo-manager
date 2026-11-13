# Browse, organize and recover files

For a library owner reviewing media, planning changes and recovering deleted files.
The five tools operate on the index and show a plan before a confirmed file operation starts.

## Tour the screen

1. Use the menu to switch between **Browse**, **Duplicate finder**, **Asset move**,
   **Folder formatting**, **Empty folder cleanup**, **Trash** and **Settings**. The re-index
   commands are also in the menu.

   You should see: each tool keeps its own source and comparison pane locations.

2. The info icon beside the tool name opens an explanation and usage steps. The filter button
   beside it reveals the filter bar, which starts collapsed. Hiding it does not clear active filters.
   The filter bar offers **Cameras**, **Rating**, **Flags** and **Tags** where filtering applies.
   **Clear filters** resets them. **Hide folders without matches** switches between hiding
   unmatched folders and leaving them visible but greyed out; a planned folder is not hidden.
   The duplicate finder also has **Apply to** choices **Both**, **Source** and **Comparison**.

   You should see: item counts and the pane listings update to reflect the active filters.

3. Use **Use this folder** in a pinned pane to change its base and **Go to base** to return.
   The bottom bar shows matching counts, selected counts, **Select all**, **Clear selection** and
   one thumbnail-size slider on the left. The top-right task indicator shows background work.

   You should see: selection stays independent, while the thumbnail-size slider scales both panes.
   **Sync navigation**, beside **Include subfolders**, makes both panes show the exact same folder
   and links their scrolling. Turn it off to browse separately. Tasks continue while you use other tools.
   Long folder paths collapse their middle into **...**; open it to navigate to a hidden parent.
   Hover or focus a path segment to see the full path in a tooltip.
   Hover a filename for its full name, or its capture date for the time, file size and date source.
   The list/grid icons in each pane's toolbar switch folders between compact rows and photo
   thumbnails. Each pane remembers its own choice. Previews use a matching indexed photo from
   the folder or its descendants; folders without photos show a folder icon. Counts and
   right-click actions work in either view, and the thumbnail-size slider also scales folder tiles.

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

## Duplicate finder

Use **Folder view** for the existing source/comparison workflow, or **File view** to review
groups of matching copies together.

### File view

Choose a **Source** folder, optionally include its subfolders, then **Find duplicates**.
The source determines which files start the search, not where their copies may be:
matches are found across all volumes. **Exclude folder** omits that folder and its entire
subtree from both sides; remove an exclusion to include it again.

Each group has a separate card containing large thumbnails of all its copies. Green borders and **Keep** labels mean retain;
red borders and **Remove** labels mean move to Trash after confirmation. Click a thumbnail to
prefer that copy. Filename, size and capture date appear beneath each thumbnail; fields that
differ between copies are bold. Differing sizes include exact byte counts, and capture dates
include seconds (a missing date is shown explicitly).
Star ratings and flags appear as icon pills on the thumbnail when set. Thumbnails retain
the image's aspect ratio. Hover a thumbnail or its Keep/Remove pill to see the selecting rule.
The bottom slider adjusts thumbnail size; groups load as you scroll, without changing selection
rules or the final action. Name-based matches are not always transitive, so a group can require
multiple keepers: every removal must directly match a retained file.

**Automatic Selection** lists folder preferences first, then filename regular expressions,
shorter/longer filenames, capture, modification and creation dates in either order,
file size in either order, presence of ratings, descriptions, flags or tags, a specific tag
among the matching copies, and multiple preferred folders (exact or including descendants).
Enable rules individually and drag them into priority order; Up/Down buttons also reorder.
The first enabled rule wins, later rules break ties, missing dates rank last, and exact ties
use the lowest file ID. Applying rules replaces manual choices in every group.

Right-click a thumbnail and choose **Keep files from this folder**. Choose its folder or any pre-populated ancestor, then exact-folder or
subfolder matching. Applying makes that preference the first rule for all groups.

**Trash duplicates** confirms the total and warns that copies outside the source can be removed.
The existing plan executor rechecks identity, metadata and matching survivors before using
journaled file operations. Nothing is permanently deleted; use Trash to recover files.
Changes to indexed files or search settings require a refresh before another action.
File view is bounded to 20,000 matched files per side and the existing two-million-pair limit;
an oversized search is refused rather than truncated. Narrow the source folder if necessary.
Descriptions are read from embedded EXIF/IPTC/XMP and linked XMP metadata; existing indexed
images and videos are queued for metadata extraction after the database upgrade.

The source folder shows matching file counts separately for that folder and its subfolders.
After a search, the source picker also shows matching counts from the current plan.

### Folder view

1. Set a source folder in the left pane and a comparison folder in the right pane. **Match by**
   offers **Hash exact match** (same content), **Filename and time exact match** (same filename
   and capture time to the second), **Filename exact match, different time**, and
   **Filename prefix match**. Prefix matching can include **Strict prefix** to avoid treating a
   longer number or word as a continuation.

   You should see: matching files have a green border, and the summary reports matching files in
   both panes. Matches update as background hashing finishes.

2. The compact split action in the bottom bar offers **Delete source items**, **Delete comparison items** and
   **Move files**. Its last-used choice is remembered. Delete actions ask for confirmation and
   send only matched files to Trash when a matching survivor remains outside the effective
   deletion set. **Move files** hands the chosen files to **Asset move**; no files move until its
   separate plan is confirmed.
   With a selection, **Move files** passes the active pane's selected files; without one, it
   passes the comparison-side matches.

   You should see: a confirmation describing the survivor rule before deletion, or the selected
   files carried into the move tool.

## Asset move

1. Choose source and destination folders with **Use this folder**. **Move type** offers
   **Keep source file structure**, **Keep destination file structure**, **Flat file transfer**
   and **Use custom file structure**. The **Destination** pane previews the plan; planned
   files and folders are marked rather than written to disk.

   You should see: the summary counts files and new folders that will move, plus any skipped,
   renamed, replaced or cross-volume items.

2. **If a file with the same name exists** chooses **Smart**, **Skip**, **Rename** or **Replace**.
   Smart skips identical content and renames a different conflict, for example `photo.jpg` to
   `photo_1.jpg`. Skip leaves the source file in place. Rename gives the incoming file a new name.
   Replace moves the existing destination to Trash before using its name. No policy overwrites a
   destination in place. Confirm with **Move files** only after reviewing the **Destination** pane.

   You should see: a final count and destination in the confirmation. Folders left empty by a move
   remain; use **Empty folder cleanup** separately.

3. For an Immich-tracked move, see [direct path updates](immich.md#direct-path-updates-for-tracked-moves).
   If a move result includes `shared_sidecar_retained`, the assets moved and the result names the
   retained original sidecar path: an identical sidecar was already at that destination. That is
   not a skipped asset. The first successful owner for each distinct sidecar destination gets a
   journaled copy in its move group, so cancellation or restart cannot strand a moved photo without
   its companion. Distinct destinations get their own matching sidecars.

   You should see: the run result explicitly says the assets moved and identifies retained
   metadata copies; it does not imply the photo was skipped.

## Folder formatting

1. Open a source folder. The destination initially follows the source; change it with
   **Use this folder** in the **Destination** pane if needed. Choose **Edit formatting** to open
   the pattern and conflict-policy dialog. Edit **Folder pattern** or insert tokens
   from **Variables and examples**. The live preview checks the pattern against sample files.
   Dates use the best available capture time: preferred embedded original date, then linked XMP,
   other embedded or video dates, then file birth time or modification time.

   You should see: generated folders and filenames in the preview. If the last pattern component
   has no `{name}` or `{filename}`, the original filename is appended.

2. Set **If a file with the same name exists**, close the dialog with **Done**, review the
   planned formatting in the toolbar and destination pane, and confirm with **Move and format files**.
   The same conflict policies apply as in **Asset move**; the operation changes files only after
   confirmation. It does not remove folders that become empty.

   You should see: a confirmation with the number of files and destination pattern.

## Batch rename

1. Open **Batch rename**. The source follows the current folder and uses its filters; turn on
   recursive scope to include subfolders. **Filename match mode** can include **All files**, **Glob**
   or **Regular expression**. Glob matches the whole filename (`*`, `?`, `[abc]`); regular expressions
   use Rust syntax and search anywhere unless anchored with `^` and `$`. Matching ignores case by
   default when a matcher pattern is used.

   You should see: matching source files are marked, while the comparison pane shows only
   proposed names as non-selectable ghost cards. Folder rows and counts remain real because files stay
   in their current folders.

2. **Clean numbered copies** optionally removes recognized copy suffixes from the stem, including
   `(1)`, `-1`, `-1-1` and common `copy` suffixes; extensions are left alone. **Enable regex rewrite**
   applies a find/replace expression to the cleaned stem. Rewrites replace all non-overlapping
   matches; use `$1` or `${name}` for captures and `$$` for a literal dollar. Both cleaning and
   rewriting are off by default. A single-digit tail such as `2024-12-5` can be removed by Clean;
   `_1` is not cleaned.

   You should see: the new stem in the live sample. The sample covers only the first matching
   folder and is not a prediction of final conflicts; when truncated, counter values are approximate.

3. Edit **Name pattern** to build the new filename from the
   [rename token catalogue](#rename-pattern-tokens). `{count}` restarts at 1 for each folder and
   name group; **Counter digits** uses **Exact number** for an unpadded counter, or 1–9 for that
   minimum zero-padded width. Width never truncates. RAW/JPEG pairs normally share a count; a pattern
   that varies between them, such as one using `{ext}`, can put them in separate groups. Without
   `{ext}` or `{filename}`, the original extension is appended, and the extension cannot be changed.
   Files needing metadata are not renamed until indexing has supplied it.

   You should see: names in the sample and the comparison ghosts update after a successful preview.
   The plan still uses full-scope conflicts and can differ from a sample limited to one folder.

4. Choose **Smart**, **Skip** or **Rename** for name conflicts, then review the plan and confirm with
   **Rename files**. Replace is not available: batch rename never trashes another file. Files retain
   their directory and extension; sidecars follow the renamed asset. A name freed elsewhere in the
   same batch is not reused.

   You should see: a confirmation before any rename starts, then a background task and result.

## Rename pattern tokens

The rename name-pattern dialect supports the tokens below. The `{count}` token is rename-only; it is
not accepted by **Folder formatting**. The examples use the generated sample file.

<!-- rename-tokens:start -->
| Group | Token | Meaning | Sample value |
|---|---|---|---|
| Date | `{YYYY}` | Year, 4 digits | `2026` |
| Date | `{YY}` | Year, last 2 digits | `26` |
| Date | `{mm}`, `{MM}` | Month number, 2 digits | `10` |
| Date | `{MMM}` | Month name, abbreviated (English) | `Oct` |
| Date | `{MMMM}` | Month name, in full (English) | `October` |
| Date | `{dd}` | Day of the month, 2 digits | `05` |
| Date | `{HH}` | Hour on the 24-hour clock, 2 digits | `14` |
| Date | `{min}` | Minute, 2 digits | `07` |
| Date | `{sec}` | Second, 2 digits | `09` |
| File | `{filename}` | Original file name, with its extension | `IMG_0042.CR3` |
| File | `{name}` | Original file name, without its extension | `IMG_0042` |
| File | `{ext}` | File extension without the dot, in its original case | `CR3` |
| File | `{count}` | Folder-local sequence number for files with the same rendered stem | `IMG_0042_1.CR3` |
| Other | `{camera}` | Camera make and model, or a placeholder when unknown | `Canon EOS R5` |
| Other | `{folder}` | Name of the folder the file is in now | `Holiday` |
<!-- rename-tokens:end -->

## Empty folder cleanup

1. Open the source folder that is the cleanup base. By default the scan includes subfolders;
   turning off **Include subfolders** at the right of the toolbar restricts it to the current folder's children.
   Empty means no indexed files of any kind, regardless of filters. The base folder itself is
   never removed. Planned deletions have a red border.

   You should see: the comparison pane marks the empty folders below the base that qualify.
   Parent folders show the number of matches below them even when the parent itself is not empty;
   only folders that will actually be removed have a red border.

2. Choose **Delete empty folders (N)** and review the counts of top-level and nested folders.
   Empty directories are removed, not sent to Trash. Leftover junk files (such as thumbnail
   caches or `.DS_Store`) go to Trash; folders with other contents are left alone.

   You should see: a confirmation before folder removal and a background task afterwards.

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

3. **Indexing** controls scans, hashing, thumbnails, watches and periodic repair.
   The top-right task indicator lets background work proceed without locking the rest of the app.
   Up to three tasks run at once across indexing, file operations and Immich; additional tasks
   wait in submission order. Multiple move/delete tasks can progress together. Each file or
   owner/sidecar group is rechecked and changed as one synchronized unit, so tasks interleave
   between items rather than racing a duplicate's last surviving copy.
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

5. To leave a folder out of the library, right-click it in a pane and choose
   **Exclude folder…**, then confirm. The folder and everything below it leave the index and every
   tool: duplicates, moves, formatting, renames and cleanup never plan or touch its files, and a
   move into it is refused. Its files stay on disk, untouched. Volume rows cannot be excluded.
   **Settings** > **General** > **Excluded folders** lists them; **Include again** re-indexes one.

   You should see: the folder disappears from the pane at once, and its counts leave the totals
   after a short background scan.

6. To leave a folder out of tools without hiding it, right-click it and choose **Soft exclude
   folder**. The folder and its descendants stay indexed and browsable with a **Soft exclude**
   tag, but are omitted from duplicate matching, moves, folder formatting, batch rename and
   empty-folder cleanup. This is one shared list across tools, separate from search-specific
   exclusions. Plans refresh when the list changes; older plans cannot modify newly excluded files.

   Choose **Remove soft exclusion** on the exact folder, or **Include in tools** under
   **Settings** > **General** > **Soft-excluded folders**, to include it again immediately.
   Neither action requires a rescan. A child excluded by its parent must be re-included at that
   parent or through Settings.

## Trash and recovery

1. File-deletion actions send content to `<volume>/.opm-trash/<batch>/<original-relative-path>`,
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
   This is not undoable in OPM.

   You should see: a warning before permanent deletion, then a background trash-empty task.

4. **Delete trashed files automatically** is off by default. Enabling it with **Delete after**
   permanently purges items older than that many days, without a separate manual empty action.
   Shortening retention can take effect as soon as you save. Leave it off if you want manual control.

   You should see: automatic deletion remains disabled unless you explicitly enable and save it.

## Pattern language reference

Tokens are case-sensitive; unknown names are errors. `{mm}` and `{MM}` both mean month;
minutes use `{min}`. Month names are English. The sample is `IMG_0042.CR3`, captured at
2026-10-05 14:07:09, with camera `Canon EOS R5`, in folder `Holiday`.

<!-- tokens:start -->
| Group | Token | Meaning | Sample value |
|---|---|---|---|
| Date | `{YYYY}` | Year, 4 digits | `2026` |
| Date | `{YY}` | Year, last 2 digits | `26` |
| Date | `{mm}`, `{MM}` | Month number, 2 digits | `10` |
| Date | `{MMM}` | Month name, abbreviated (English) | `Oct` |
| Date | `{MMMM}` | Month name, in full (English) | `October` |
| Date | `{dd}` | Day of the month, 2 digits | `05` |
| Date | `{HH}` | Hour on the 24-hour clock, 2 digits | `14` |
| Date | `{min}` | Minute, 2 digits | `07` |
| Date | `{sec}` | Second, 2 digits | `09` |
| File | `{filename}` | Original file name, with its extension | `IMG_0042.CR3` |
| File | `{name}` | Original file name, without its extension | `IMG_0042` |
| File | `{ext}` | File extension without the dot, in its original case | `CR3` |
| Other | `{camera}` | Camera make and model, or a placeholder when unknown | `Canon EOS R5` |
| Other | `{folder}` | Name of the folder the file is in now | `Holiday` |
<!-- tokens:end -->

1. Date group: `{YYYY}/{mm}` produces `2026/10/IMG_0042.CR3`.
   When the last component contains no `{name}` or `{filename}`, the original filename is appended.

   You should see: the original filename below the generated date folders.

2. File group: `{name}_copy.{ext}` produces `IMG_0042_copy.CR3`.
   `{name}` omits the extension, while `{filename}` includes it; `{ext}` preserves its original case.

   You should see: one renamed file component, with no duplicate appended filename.

3. Other group: `{camera}/{folder}/{filename}` produces `Canon EOS R5/Holiday/IMG_0042.CR3`.
   A missing camera uses `Unknown camera`; folder means the current containing folder's name.
   A trailing slash forces the original filename to be appended, and token-generated names are sanitised
   and limited by the configured generated-name byte limit.

   You should see: camera and current-folder names used as folder components.

## Selection, viewer and file safety

1. Click a file to open it when no selection is active. Click a checkbox, Cmd/Ctrl-click, or
   Shift-click to select or extend a selection; once selection is active, clicking another file
   selects it. **Select all** acts on the active pane. Cmd/Ctrl+A selects that pane, and Escape
   clears the active pane, then the other pane if nothing was selected there.

   You should see: selection checkboxes and counts for the pane you used.

2. Use arrow keys, Home, End, PageUp or PageDown to move through the grid; Enter opens a file and
   Space toggles its selection; Shift+Space extends a range. In the viewer, Left/Right moves
   through the pane's file list, `i` toggles **Info**, `z` or a double-click on the image toggles
   100% zoom, and Escape closes. **Open original** opens the original in a new tab; **Download**
   downloads it.

   You should see: image/video preview and file details where available. Video playback uses the
   original codec; browser support varies.

3. File deletions go to the volume's `.opm-trash` and can be restored from **Trash**. Empty
   folders are removed, not trashed. **Empty trash…** permanently deletes the displayed snapshot;
   **Delete trashed files automatically** is off by default, but enabling retention also permanently
   deletes old items. A permanent delete cannot be restored by OPM.

   You should see: the warning and a confirmation before a permanent delete.

## Re-index and settings

1. Use **Re-index current folders** to force metadata, checksums and thumbnails to be reread in
   the folders shown by both panes, including subfolders. Use **Re-index all assets** for a
   confirmed library-wide refresh. Ordinary scans are incremental; do not force all assets for
   every external edit.

   You should see: a background index task while the app remains usable.

2. **Settings** > **General** controls sidecars, ignored names/prefixes, conflict policy, generated
   name length, trash retention and indexing. **Settings** > **Immich** contains the Immich
   connection, library mappings, sync and outbox. Database URL configuration for direct move
   updates is outside Settings; see the [Immich guide](immich.md).

   You should see: unsaved General settings expose **Save** and **Discard**; Immich settings show
   the connection and sync status without exposing the database secret.

## Install as an app

Open OPM over HTTPS (or localhost during development), then use your browser's **Install app**
command. On iPhone or iPad, use Safari's **Share > Add to Home Screen**. A NAS accessed through
a plain HTTP LAN address is not a secure context: configure HTTPS before installing.

The installed app still needs the OPM server. Only an offline notice is cached, never library
media or API responses; file changes are not queued while offline.

## Interrupted background work

New tasks keep their saved requests and checkpoints across server restarts, including file-changing
tasks. They resume automatically using the same task ID. Completed file operations are recognized
from the filesystem journal, rather than applied twice. Scans resume directory batches and pipeline
stages, Immich resumes committed pages, and plan/trash operations resume completed items.
An unfinished file copy may be retried from its beginning; byte-offset copy resumption is not supported.
Explicitly cancelled tasks do not restart. Tasks created by an older version without saved requests
remain interrupted and must be submitted again.
