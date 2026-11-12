-- 0120_immich_outbox: the file operations that still have to be reflected in Immich.
--
-- Migration block 0120-0139 belongs to todo `reflecting-file-ops-in-immich`. Written by the `FsOps` hook, the scheduled
-- Immich task and the outbox routes (`crates/apm-server/src/immich/outbox/`). Conventions are those of 0001_core: STRICT
-- tables, epoch SECONDS, booleans as INTEGER 0/1 guarded by a CHECK.
--
-- A row says "the file that was at `old_*` is now at `new_*`" (a move), "was at `old_*` and is gone" (a trash) or "now
-- is at `new_*`" (a copy or a restore). The hook writes it in the same transaction that makes the file operation final.
-- Rows that finish are DELETED (the task result holds the summary of a run), so the table only ever holds work that is
-- still to do or needs a human: it never grows with history.
CREATE TABLE immich_outbox (
  id                INTEGER PRIMARY KEY,
  -- The indexed file the change is about. NOT a foreign key: the row of a trashed or deleted file is gone while its
  -- change is still queued. Moves keep the file id, so changes of one file merge on it (see outbox/merge.rs).
  file_id           INTEGER NOT NULL,
  -- Where the file was. All three columns are set or all are NULL (nothing to remove from Immich: a copy or a restore).
  -- No foreign key either: a volume that no longer exists simply drops the row. `dir` is a RelPath ('' for the root).
  old_volume_id     INTEGER,
  old_dir           TEXT,
  old_name          TEXT,
  -- Where the file is now (all set, or all NULL when it left for the trash). At least one side is set.
  new_volume_id     INTEGER,
  new_dir           TEXT,
  new_name          TEXT,
  asset_id          TEXT,                                                    -- the OLD Immich asset, resolved by the first pass
  new_asset_id      TEXT,                                                    -- the asset Immich made for the new location
  snapshot_json     TEXT CHECK (snapshot_json IS NULL OR json_valid(snapshot_json)), -- favorite, rating, albums, tags of the old asset
  -- 0 = no library scan was asked for this change yet (rows that are still merged), 1 = the scan was requested and the
  -- row waits for Immich to notice.
  stage             INTEGER NOT NULL DEFAULT 0 CHECK (stage IN (0, 1)),
  state             TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'attention')),
  carried           INTEGER NOT NULL DEFAULT 0 CHECK (carried IN (0, 1)),    -- favorite, rating, albums and tags are on the new asset
  attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  last_error        TEXT,                                                    -- a short code or message, never a secret or a path
  offline_seen_at   INTEGER,                                                 -- when a pass first saw the old asset offline
  scan_requested_at INTEGER,                                                 -- when the library scan was requested for this row
  next_attempt_at   INTEGER NOT NULL,                                        -- not handled before this instant
  -- When the latest change of the row was recorded: a merge of the hook records it again, nothing else does. A library
  -- scan that was asked for at or after it covers the row, and the age of a row that never had a scan counts from it.
  created_at        INTEGER NOT NULL,
  CHECK ((old_volume_id IS NULL) = (old_dir IS NULL) AND (old_dir IS NULL) = (old_name IS NULL)),
  CHECK ((new_volume_id IS NULL) = (new_dir IS NULL) AND (new_dir IS NULL) = (new_name IS NULL)),
  CHECK (old_volume_id IS NOT NULL OR new_volume_id IS NOT NULL)
) STRICT;

-- The due-row query of a pass and of the scheduler, and the counts of the status.
CREATE INDEX immich_outbox_due ON immich_outbox (state, next_attempt_at);

-- The merge target of the hook: at most one waiting change per file. A change that already had its scan requested
-- (stage 1) is never merged into, so it is not part of the index.
CREATE UNIQUE INDEX immich_outbox_merge ON immich_outbox (file_id) WHERE stage = 0 AND state = 'pending';
