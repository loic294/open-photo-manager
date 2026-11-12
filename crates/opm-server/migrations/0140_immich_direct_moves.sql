-- Prepared intents are inert until the FsOps finalising hook queues them, in its
-- own local transaction. Remote I/O never runs with the SQLite write lock held.
CREATE TABLE immich_direct_move (
    old_volume_id INTEGER NOT NULL,
    old_rel TEXT NOT NULL,
    new_volume_id INTEGER NOT NULL,
    new_rel TEXT NOT NULL,
    request_json TEXT NOT NULL,
    -- Stable operation proof, not FKs: index/journal pruning must not cascade
    -- away a still-unconfirmed remote update.
    file_id INTEGER NOT NULL,
    journal_id INTEGER,
    journal_created_at INTEGER,
    prepared_at INTEGER NOT NULL,
    queued INTEGER NOT NULL DEFAULT 0 CHECK (queued IN (0, 1, 2)),
    -- Not an FK: replay proof survives removal of the completed outbox row.
    outbox_id INTEGER,
    sidecar_confirmed INTEGER NOT NULL DEFAULT 0 CHECK (sidecar_confirmed IN (0, 1)),
    PRIMARY KEY (old_volume_id, old_rel, new_volume_id, new_rel)
);
CREATE UNIQUE INDEX immich_direct_move_outbox ON immich_direct_move(outbox_id);
CREATE INDEX immich_direct_move_asset
    ON immich_direct_move(json_extract(request_json, '$.spec.asset_id'), queued);
CREATE INDEX immich_direct_move_pending_destination
    ON immich_direct_move(new_volume_id, new_rel) WHERE queued = 1;
CREATE INDEX fs_journal_direct_move_lookup
    ON fs_journal(src_volume_id, src_path, dst_volume_id, dst_path, op, state, id);

CREATE TABLE immich_direct_db_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    ready INTEGER NOT NULL DEFAULT 0 CHECK (ready IN (0, 1)),
    reason TEXT NOT NULL,
    checked_at INTEGER
);
INSERT INTO immich_direct_db_state (id, reason)
VALUES (1, 'Direct database access has not been checked. Tracked moves require a compatible Immich v3.3.3 database.');
