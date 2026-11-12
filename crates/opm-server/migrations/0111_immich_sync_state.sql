-- 0111_immich_sync_state: where the asset sync stands, per scope.
--
-- Migration block 0110-0119 belongs to todo `syncing-immich-assets`. Written ONLY by the asset sync
-- (`crates/apm-server/src/immich/assets.rs`); `GET /api/immich/status` reads it. Timestamps are epoch SECONDS except
-- `high_water_ms`.

-- One row per sync scope. A scope is the unit the sync reads from Immich in one go:
--   * full mode (an administrator account): one scope per distinct mapped library, `scope` = the library id;
--   * limited mode (no library can be named): one scope per mapped path prefix, `scope` = 'path:' || the prefix.
-- A row exists from the first attempt on. Disconnecting clears the table, and the sync removes the rows of scopes that
-- no mapping feeds any more.
CREATE TABLE immich_sync_state (
  scope                TEXT PRIMARY KEY,
  -- A hash of what feeds the scope (the connection, the library, the prefixes and volumes mapped to it) as of the last
  -- COMPLETE full pass; '' until there is one. A scope whose current hash differs is read in full again, which is how a
  -- first connect and an edit of the mappings need no callback.
  fingerprint          TEXT NOT NULL DEFAULT '',
  -- The largest Immich `updatedAt` (epoch ms) seen by complete passes: the next incremental pass asks for what changed
  -- after it (minus a safety margin). Never moved by a failed or cancelled pass.
  high_water_ms        INTEGER NOT NULL DEFAULT 0,
  run                  INTEGER NOT NULL DEFAULT 0,                           -- the last immich_assets.sync_run handed out
  last_attempt_at      INTEGER,                                              -- start of the latest pass, complete or not (for the back-off)
  last_sync_at         INTEGER,                                              -- end of the latest COMPLETE pass
  last_full_at         INTEGER,                                              -- end of the latest COMPLETE full pass
  last_error           TEXT,                                                 -- why the latest attempt failed; NULL after a success (never a secret)
  consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
  -- What the latest complete pass left in immich_assets for this scope.
  assets_total         INTEGER NOT NULL DEFAULT 0,                           -- every row: live, offline and trashed
  assets_live          INTEGER NOT NULL DEFAULT 0,                           -- not offline and not trashed
  assets_matched       INTEGER NOT NULL DEFAULT 0,                           -- resolved to a volume AND an indexed file exists at that path
  assets_unmapped      INTEGER NOT NULL DEFAULT 0                            -- the path is outside every mapping
) STRICT;
