-- 0110_immich_assets: what Immich reports for the mapped libraries, resolved to the files of the index.
--
-- Migration block 0110-0119 belongs to todo `syncing-immich-assets`. Conventions are those of 0001_core: STRICT tables,
-- booleans as INTEGER 0/1 guarded by a CHECK, paths as `RelPath` strings (no leading or trailing '/', '' is the volume
-- root). The one exception to "timestamps are epoch seconds" is `updated_at`, kept in milliseconds as Immich reports it.
--
-- Written ONLY by the asset sync (`crates/apm-server/src/immich/assets.rs`, called from `immich/sync.rs`). Nothing here is
-- ever sent back to Immich. Read by the `in_immich` badge of the browse projection (`query/items.rs`) and by the Immich
-- outbox, which needs to know which asset stands for a local file before it moves or deletes it.

-- One row per Immich asset of a sync scope (see `immich_sync_state`): live, offline and trashed assets alike, because
-- Immich keeps a deleted file as an offline asset in its trash and the application has to see the difference.
--
-- The join to `files` is by the PATH of the file, `(volume_id, dir_path, name)`, never by a stored file id and never by
-- checksum: the checksum Immich computes for an external-library asset is derived from its path, not from its content, so
-- it can not identify a local file; assets that were uploaded to Immich (not read from a library) are out of scope.
CREATE TABLE immich_assets (
  asset_id      TEXT PRIMARY KEY,                                            -- the Immich asset id
  scope         TEXT NOT NULL,                                               -- immich_sync_state.scope of the pass that last wrote the row
  library_id    TEXT,                                                        -- NULL when the asset response does not carry it
  original_path TEXT NOT NULL,                                               -- exactly as Immich reports it, never normalised
  is_offline    INTEGER NOT NULL CHECK (is_offline IN (0, 1)),               -- Immich found the file missing during a library scan
  is_trashed    INTEGER NOT NULL CHECK (is_trashed IN (0, 1)),               -- in Immich's trash (is_trashed or deleted_at)
  updated_at    INTEGER NOT NULL,                                            -- epoch MILLISECONDS (the high-water mark); 0 when Immich sent none
  sync_run      INTEGER NOT NULL,                                            -- immich_sync_state.run of the pass that last wrote the row
  -- The resolved location through the path mapping: all three NULL (the path is outside every mapping) or all three set.
  volume_id     INTEGER REFERENCES volumes (id) ON DELETE CASCADE,
  dir_path      TEXT,                                                        -- same encoding as files.dir_path
  name          TEXT,                                                        -- as on disk, like files.name
  CHECK (
    (volume_id IS NULL AND dir_path IS NULL AND name IS NULL)
    OR (volume_id IS NOT NULL AND dir_path IS NOT NULL AND name IS NOT NULL)
  ),
  CHECK (dir_path IS NULL OR (substr(dir_path, 1, 1) <> '/' AND substr(dir_path, -1) <> '/')),
  CHECK (name IS NULL OR (name <> '' AND instr(name, '/') = 0))
) STRICT;

-- The lookup by file location: the badge of every item row and the outbox. It starts with volume_id, so it is also the
-- index the foreign key needs. The two flags ride along so the badge ("a LIVE asset is there") is answered from the index
-- alone, without reading the table.
CREATE INDEX immich_assets_location ON immich_assets (volume_id, dir_path, name, is_offline, is_trashed);

-- The cleanup after a complete full pass: the rows of a scope that this pass did not touch no longer exist in Immich.
CREATE INDEX immich_assets_scope_run ON immich_assets (scope, sync_run);
