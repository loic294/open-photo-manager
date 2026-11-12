-- 0100_immich_connection: the connection to the Immich server and the volume-to-library mapping.
--
-- Migration block 0100-0109 belongs to todo `building-immich-client`. The asset and sync tables follow in
-- block 0110-0119 (`syncing-immich-assets`) and the outbox in block 0120-0139 (`reflecting-file-ops-in-immich`).
--
-- Conventions are those of 0001_core: STRICT tables, epoch SECONDS, booleans as INTEGER 0/1 guarded by a CHECK.

-- The one connection to an Immich server: at most one row, always id = 1. It stores what the application needs to
-- keep talking to Immich WITHOUT the user's password: that is used once to create an API key and never stored.
-- Written only by the Immich service (`crates/apm-server/src/immich/store.rs`).
CREATE TABLE immich_connection (
  id                 INTEGER PRIMARY KEY CHECK (id = 1),
  server_url         TEXT NOT NULL,                                          -- normalised: scheme, host, port and optional path prefix
  email              TEXT NOT NULL,                                          -- the Immich account the key belongs to
  user_id            TEXT NOT NULL,
  user_name          TEXT NOT NULL,
  is_admin           INTEGER NOT NULL CHECK (is_admin IN (0, 1)),            -- 0 = limited mode (libraries cannot be managed)
  api_key_id         TEXT NOT NULL,                                          -- the Immich id of the key, needed to revoke it
  api_key_enc        TEXT NOT NULL,                                          -- the API key, encrypted (immich/crypto.rs); never plaintext
  allow_insecure_tls INTEGER NOT NULL DEFAULT 0 CHECK (allow_insecure_tls IN (0, 1)),
  server_version     TEXT,                                                   -- as reported by Immich, e.g. 'v3.4.0'
  connected_at       INTEGER NOT NULL,
  last_ok_at         INTEGER,                                                -- last successful call to Immich
  last_error         TEXT,                                                   -- last failure message (never contains secrets)
  key_rejected       INTEGER NOT NULL DEFAULT 0 CHECK (key_rejected IN (0, 1)) -- Immich answered 401 to the key: connect again
) STRICT;

-- Which Immich external library holds the files of a volume, and the path Immich sees as the root of that volume.
-- A mapped file `<volume root>/<rel>` is `<path_prefix>/<rel>` for Immich (see immich/paths.rs).
-- library_id is NULL in limited mode, where the libraries cannot be listed but the prefix is still known.
CREATE TABLE immich_volume_map (
  volume_id    INTEGER PRIMARY KEY REFERENCES volumes (id) ON DELETE CASCADE,
  library_id   TEXT,
  library_name TEXT NOT NULL DEFAULT '',                                     -- display copy, refreshed whenever the libraries are listed
  path_prefix  TEXT NOT NULL CHECK (substr(path_prefix, 1, 1) = '/')         -- absolute, no trailing '/' (except the lone '/')
) STRICT;
CREATE UNIQUE INDEX immich_volume_map_prefix ON immich_volume_map (path_prefix);
