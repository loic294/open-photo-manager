-- Historical migrations stay in place so databases that applied them still validate.
DROP TABLE immich_volume_map;
DROP TABLE immich_assets;
DROP TABLE immich_sync_state;
DROP TABLE immich_outbox;
DROP TABLE immich_outbox_state;
DROP TABLE immich_direct_move;
DROP TABLE immich_direct_db_state;
DROP TABLE immich_connection;
DROP INDEX fs_journal_direct_move_lookup;
DELETE FROM tasks WHERE kind IN ('immich_sync', 'immich_cleanup');

ALTER TABLE ism_connection ADD COLUMN file_settings TEXT CHECK (json_valid(file_settings));

CREATE TABLE ism_assets (
    volume_id INTEGER NOT NULL REFERENCES ism_volume_roots (volume_id) ON DELETE CASCADE,
    dir_path TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (volume_id, dir_path, name)
) STRICT, WITHOUT ROWID;

CREATE TRIGGER ism_assets_binding_changed AFTER UPDATE OF root_id, root_prefix ON ism_volume_roots
WHEN OLD.root_id <> NEW.root_id OR OLD.root_prefix <> NEW.root_prefix
BEGIN
    DELETE FROM ism_assets WHERE volume_id = OLD.volume_id;
END;
