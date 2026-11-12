-- 0001_core: the core index schema.
--
-- Column names and semantics are a CONTRACT shared by the scanner, the query engine, file operations, trash,
-- tasks and the plan engine. The module docs of `crates/apm-server/src/db/mod.rs` explain how to use it.
--
-- Conventions
--   * Every table is STRICT: a value of the wrong storage class is an error instead of being coerced.
--   * Timestamps are integer epoch SECONDS (UTC) unless a column says otherwise.
--   * Booleans are INTEGER 0/1 guarded by a CHECK.
--   * Foreign keys are enforced (`PRAGMA foreign_keys = ON` on every connection, verified when the database opens).
--   * A path is a `RelPath` string: relative to the volume root, '/' separated, no leading or trailing '/', '' is
--     the root. Paths are the exact on-disk bytes (no Unicode normalisation); matching uses the `*_norm` columns.
--   * Unique indexes are created explicitly (not as table constraints) so they have a stable name that
--     EXPLAIN QUERY PLAN assertions can rely on.
--   * Not here on purpose: plan tables (migration block 0200) and Immich tables (block 0100).

-- A volume is a sub-directory of APM_VOLUMES_DIR. Rows are NEVER purged for an offline volume.
CREATE TABLE volumes (
  id           INTEGER PRIMARY KEY,
  name         TEXT NOT NULL UNIQUE,                                  -- directory name under APM_VOLUMES_DIR, shown in the UI
  root         TEXT NOT NULL,                                         -- absolute (canonical) path inside the container
  writable     INTEGER NOT NULL DEFAULT 0 CHECK (writable IN (0, 1)), -- probed by creating a file, never assumed
  online       INTEGER NOT NULL DEFAULT 1 CHECK (online IN (0, 1)),   -- 0 when the root is missing or unreadable
  scan_gen     INTEGER NOT NULL DEFAULT 0,                            -- bumped at the start of each full scan
  last_scan_at INTEGER,
  created_at   INTEGER NOT NULL
) STRICT;

-- Every directory of a volume, the empty ones and the junk-only ones included.
CREATE TABLE dirs (
  id          INTEGER PRIMARY KEY,
  volume_id   INTEGER NOT NULL REFERENCES volumes (id) ON DELETE CASCADE,
  path        TEXT NOT NULL,                                          -- '' = the volume root
  parent_path TEXT,                                                   -- NULL only for the root; '' for top-level directories
  name        TEXT NOT NULL,                                          -- last path component, '' for the root
  mtime       INTEGER,
  seen_gen    INTEGER NOT NULL DEFAULT 0,                             -- rows with seen_gen < volumes.scan_gen after a scan are removal candidates
  CHECK (substr(path, 1, 1) <> '/' AND substr(path, -1) <> '/'),
  CHECK (
    (path = '' AND parent_path IS NULL AND name = '')
    OR (
      path <> '' AND parent_path IS NOT NULL AND name <> '' AND instr(name, '/') = 0
      AND path = CASE WHEN parent_path = '' THEN name ELSE parent_path || '/' || name END
    )
  )
) STRICT;
CREATE UNIQUE INDEX dirs_volume_path ON dirs (volume_id, path);
CREATE INDEX dirs_parent ON dirs (volume_id, parent_path);

CREATE TABLE cameras (
  id   INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE                            -- normalised "Make Model"; NOCASE folds ASCII only
) STRICT;

CREATE TABLE tags (
  id   INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE                            -- flat keywords (hierarchies are flattened); NOCASE folds ASCII only
) STRICT;

-- Every on-disk file, sidecars included.
CREATE TABLE files (
  id                 INTEGER PRIMARY KEY,
  volume_id          INTEGER NOT NULL REFERENCES volumes (id) ON DELETE CASCADE,
  dir_path           TEXT NOT NULL,                                   -- the parent directory, same encoding as dirs.path
  name               TEXT NOT NULL,                                   -- as on disk
  name_norm          TEXT NOT NULL,                                   -- apm_core::names::normalize_name(name)
  stem_norm          TEXT NOT NULL,                                   -- normalised stem (name without its last extension)
  ext                TEXT NOT NULL DEFAULT '',                        -- lowercase, no dot
  kind               TEXT NOT NULL CHECK (kind IN ('image', 'video', 'audio', 'sidecar', 'other')), -- RAW is 'image'
  size               INTEGER NOT NULL,
  mtime              INTEGER NOT NULL,
  btime              INTEGER,                                         -- birth time when the platform exposes it
  capture_ts         INTEGER,                                         -- NAIVE LOCAL wall-clock capture time as epoch seconds as if it were UTC
  capture_offset_min INTEGER,                                         -- UTC offset in minutes when known
  capture_src        TEXT CHECK (capture_src IN ('exif', 'xmp', 'video', 'fs')),
  camera_id          INTEGER REFERENCES cameras (id) ON DELETE SET NULL,
  rating             INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),  -- 0 = unrated
  flag               INTEGER NOT NULL DEFAULT 0 CHECK (flag IN (-1, 0, 1)),      -- -1 rejected, 1 selected
  width              INTEGER,
  height             INTEGER,
  duration_ms        INTEGER,
  mime               TEXT,
  hash               BLOB CHECK (hash IS NULL OR length(hash) = 20),  -- SHA-1 of the content
  meta_state         INTEGER NOT NULL DEFAULT 0,                      -- pipeline state codes: 0 pending, 1 done, 2 failed,
  hash_state         INTEGER NOT NULL DEFAULT 0,                      --   3 not applicable (Rust: db::models::PipelineState)
  thumb_state        INTEGER NOT NULL DEFAULT 0,
  seen_gen           INTEGER NOT NULL DEFAULT 0,                      -- see dirs.seen_gen
  CHECK (name <> '' AND instr(name, '/') = 0),
  CHECK (substr(dir_path, 1, 1) <> '/' AND substr(dir_path, -1) <> '/')
) STRICT;
-- Also serves every (volume_id) and (volume_id, dir_path) prefix scan and the subtree idiom.
CREATE UNIQUE INDEX files_volume_dir_name ON files (volume_id, dir_path, name);
CREATE INDEX files_hash ON files (hash) WHERE hash IS NOT NULL;
CREATE INDEX files_name_capture ON files (name_norm, capture_ts);   -- its prefix (name_norm) serves name-only lookups too
CREATE INDEX files_stem ON files (stem_norm);
CREATE INDEX files_camera ON files (camera_id);                     -- also serves ON DELETE SET NULL
CREATE INDEX files_rating ON files (rating);
CREATE INDEX files_flag ON files (flag);
CREATE INDEX files_size ON files (size);
CREATE INDEX files_meta_state ON files (volume_id, meta_state);
CREATE INDEX files_hash_state ON files (volume_id, hash_state);
CREATE INDEX files_thumb_state ON files (volume_id, thumb_state);
CREATE INDEX files_seen_gen ON files (volume_id, seen_gen);

CREATE TABLE file_tags (
  file_id INTEGER NOT NULL REFERENCES files (id) ON DELETE CASCADE,
  tag_id  INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
  PRIMARY KEY (file_id, tag_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX file_tags_tag ON file_tags (tag_id, file_id);

-- Links a file to its sidecars: `file_id` owns `sidecar_id` (a `files` row of kind 'sidecar').
CREATE TABLE file_sidecars (
  file_id    INTEGER NOT NULL REFERENCES files (id) ON DELETE CASCADE,
  sidecar_id INTEGER NOT NULL REFERENCES files (id) ON DELETE CASCADE,
  kind       TEXT NOT NULL CHECK (kind IN ('exact', 'stem')),
  PRIMARY KEY (file_id, sidecar_id),
  CHECK (file_id <> sidecar_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX file_sidecars_sidecar ON file_sidecars (sidecar_id);

-- Server-side selection sets (handoff between tools); expired ones are purged by the plan engine.
CREATE TABLE selections (
  id         INTEGER PRIMARY KEY,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  label      TEXT,
  item_count INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX selections_expires ON selections (expires_at);

CREATE TABLE selection_items (
  selection_id INTEGER NOT NULL REFERENCES selections (id) ON DELETE CASCADE,
  file_id      INTEGER NOT NULL REFERENCES files (id) ON DELETE CASCADE,
  PRIMARY KEY (selection_id, file_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX selection_items_file ON selection_items (file_id);      -- makes the cascade from `files` an index lookup

CREATE TABLE tasks (
  id           INTEGER PRIMARY KEY,
  kind         TEXT NOT NULL,
  title        TEXT NOT NULL,
  lane         TEXT NOT NULL,
  state        TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'interrupted')),
  items_done   INTEGER NOT NULL DEFAULT 0,
  items_total  INTEGER,
  bytes_done   INTEGER NOT NULL DEFAULT 0,
  bytes_total  INTEGER,
  error        TEXT,
  params_json  TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(params_json)),
  result_json  TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
  created_at   INTEGER NOT NULL,
  started_at   INTEGER,
  finished_at  INTEGER
) STRICT;
CREATE INDEX tasks_state ON tasks (state);
CREATE INDEX tasks_created ON tasks (created_at);

-- Files moved to the per-volume trash directory (`.apm-trash`).
CREATE TABLE trash_items (
  id         INTEGER PRIMARY KEY,
  volume_id  INTEGER NOT NULL REFERENCES volumes (id) ON DELETE CASCADE,
  batch      TEXT NOT NULL,
  orig_path  TEXT NOT NULL,                                           -- volume-relative RelPath strings
  trash_path TEXT NOT NULL CHECK (substr(trash_path, 1, 11) = '.apm-trash/'),
  is_dir     INTEGER NOT NULL DEFAULT 0 CHECK (is_dir IN (0, 1)),
  size       INTEGER NOT NULL DEFAULT 0,
  reason     TEXT,
  task_id    INTEGER,                                                 -- no FK: task history may be purged
  meta_json  TEXT CHECK (meta_json IS NULL OR json_valid(meta_json)),
  trashed_at INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX trash_items_volume_trash_path ON trash_items (volume_id, trash_path);

-- Crash-recovery log of file-system operations. No FK to volumes: the journal outlives a removed volume.
CREATE TABLE fs_journal (
  id            INTEGER PRIMARY KEY,
  task_id       INTEGER,
  op            TEXT NOT NULL CHECK (op IN ('move', 'copy', 'trash', 'restore', 'rmdir', 'mkdir', 'delete')),
  state         TEXT NOT NULL CHECK (state IN ('pending', 'done', 'failed', 'abandoned')),
  src_volume_id INTEGER,
  src_path      TEXT,
  dst_volume_id INTEGER,
  dst_path      TEXT,
  part_path     TEXT,
  size          INTEGER,
  hash          BLOB,
  error         TEXT,
  created_at    INTEGER NOT NULL,
  finished_at   INTEGER
) STRICT;
CREATE INDEX fs_journal_state ON fs_journal (state);

-- Server configuration: JSON documents keyed by name (the typed settings live under 'app').
CREATE TABLE settings (
  key        TEXT NOT NULL PRIMARY KEY,
  value      TEXT NOT NULL CHECK (json_valid(value)),
  updated_at INTEGER NOT NULL
) STRICT;
